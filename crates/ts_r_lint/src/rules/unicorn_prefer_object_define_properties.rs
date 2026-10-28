//! `unicorn/prefer-object-define-properties`: a run of two or more adjacent
//! `Object.defineProperty(target, key, descriptor);` statements on the same target (unicorn's
//! reference comparison) in one statement list. Reported on the first call's
//! `defineProperty`; a run starts where the previous statement is not such a call on the
//! same target.
//!
//! The fix writes one `Object.defineProperties(target, {...})` with one property per call,
//! each descriptor re-indented one level. It is dropped when merging could change what
//! runs:
//! - two calls define the same key (static keys by value, symbols by name; dynamic keys by
//!   reference);
//! - the object literal would order the keys differently (array indices first, ascending,
//!   then strings, then symbols; a dynamic key counts as a string);
//! - a later key or descriptor could see the target before its turn: it calls, constructs,
//!   tags, awaits, yields or reads the target outside a function body;
//! - a comment in the run, or a descriptor with a string or template spanning lines.

use super::prelude::*;
use super::unicorn_conditions::{has_multiline_token, reindent_text};
use super::unicorn_static::{Static, mutable_member_access, static_value_no_side_effects};
use super::unicorn_text::{has_comment_in, paren_text};
use super::unicorn_util::{indent_unit, is_identifier_name, is_same_reference, line_start};
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-object-define-properties",
        "Prefer `Object.defineProperties()` over multiple `Object.defineProperty()` calls.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<PreferObjectDefineProperties>,
};

#[derive(Debug, Default)]
struct PreferObjectDefineProperties;

#[derive(Clone, Copy)]
struct DefineCall {
    property: NodeId,
    target: NodeId,
    key: NodeId,
    descriptor: NodeId,
}

/// `Object.defineProperty(a, b, c)` as an expression statement: no spread, no optional
/// chaining, a plain `.defineProperty` member.
fn define_call(ast: &Ast, stmt: NodeId) -> Option<DefineCall> {
    let Node::ExprStmt { expr } = *ast.node(stmt) else {
        return None;
    };
    let Node::Call {
        callee,
        optional: false,
        args,
        ..
    } = *ast.node(skip_parens(ast, expr))
    else {
        return None;
    };
    let Node::Member {
        obj,
        optional: false,
        prop,
    } = *ast.node(callee)
    else {
        return None;
    };
    if ident_name(ast, obj) != Some("Object")
        || ident_name(ast, prop) != Some("defineProperty")
        || !matches!(ast.node(prop), Node::Ident { .. })
    {
        return None;
    }
    let &[target, key, descriptor] = ast.list(args) else {
        return None;
    };
    if [target, key, descriptor]
        .iter()
        .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
    {
        return None;
    }
    Some(DefineCall {
        property: prop,
        target,
        key,
        descriptor,
    })
}

#[derive(Debug, PartialEq)]
enum Key {
    Str(String),
    Symbol(String),
}

fn static_key(cx: &Ctx<'_, '_>, key: NodeId) -> Option<Key> {
    let value = static_value_no_side_effects(cx, key)?;
    if mutable_member_access(cx, key, &mut Vec::new()) {
        return None;
    }
    match value {
        Static::Builtin(p) if p.starts_with("Symbol.") => Some(Key::Symbol(p)),
        v => v.to_js_string().map(Key::Str),
    }
}

fn has_duplicate_keys(cx: &Ctx<'_, '_>, calls: &[DefineCall]) -> bool {
    let mut statics: Vec<Key> = Vec::new();
    let mut dynamics: Vec<NodeId> = Vec::new();
    for c in calls {
        match static_key(cx, c.key) {
            Some(k) => {
                if statics.contains(&k) {
                    return true;
                }
                statics.push(k);
            }
            None => {
                if dynamics.iter().any(|&d| is_same_reference(cx, d, c.key)) {
                    return true;
                }
                dynamics.push(c.key);
            }
        }
    }
    false
}

fn array_index(key: &str) -> Option<f64> {
    let canonical = key == "0"
        || (key.starts_with(|c: char| ('1'..='9').contains(&c))
            && key.bytes().all(|b| b.is_ascii_digit()));
    let n: f64 = key.parse().ok().filter(|_| canonical)?;
    (n < 4_294_967_295.0).then_some(n)
}

fn has_reordered_keys(cx: &Ctx<'_, '_>, calls: &[DefineCall]) -> bool {
    let mut group = 0u8;
    let mut previous = -1.0;
    for c in calls {
        let key = match static_key(cx, c.key) {
            Some(Key::Symbol(_)) => {
                group = 2;
                continue;
            }
            Some(Key::Str(s)) => s,
            None => String::new(),
        };
        match array_index(&key) {
            None => {
                if group > 1 {
                    return true;
                }
                group = 1;
            }
            Some(n) => {
                if group > 0 || n < previous {
                    return true;
                }
                previous = n;
            }
        }
    }
    false
}

/// An identifier read as a value (not a member name or a non-computed key).
fn is_reference_ident(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let Some(p) = cx.parent(id) else {
        return true;
    };
    match *cx.node(p) {
        Node::Member { prop, .. } => prop != id,
        Node::Property { key, .. }
        | Node::MethodDef { key, .. }
        | Node::PropertyDef { key, .. } => key != id,
        _ => true,
    }
}

/// `root` could observe `target` before the merged call defines it.
fn may_observe_target(cx: &Ctx<'_, '_>, root: NodeId, target: NodeId) -> bool {
    let ast = cx.ast();
    let mut stack = vec![root];
    // Each node is visited once: the walk ends.
    while let Some(id) = stack.pop() {
        let node = ast.node(id);
        if matches!(
            node,
            Node::FunctionExpr { .. } | Node::FunctionDecl { .. } | Node::Arrow { .. }
        ) {
            continue;
        }
        if matches!(
            node,
            Node::Call { .. }
                | Node::New { .. }
                | Node::TaggedTemplate { .. }
                | Node::Await { .. }
                | Node::Yield { .. }
        ) {
            return true;
        }
        let candidate = !matches!(node, Node::Paren { .. })
            && (!matches!(node, Node::Ident { .. }) || is_reference_ident(cx, id));
        if candidate && is_same_reference(cx, id, target) {
            return true;
        }
        ast.for_each_child(id, |c| stack.push(c));
    }
    false
}

fn key_text(cx: &Ctx<'_, '_>, key: NodeId) -> String {
    let ast = cx.ast();
    let inner = skip_parens(ast, key);
    let text = cx.text(inner);
    match *ast.node(inner) {
        Node::StringLit { value } => {
            let value = ast.atom(value);
            if value == "__proto__" {
                format!("[{text}]")
            } else if is_identifier_name(value) {
                value.to_owned()
            } else {
                text.to_owned()
            }
        }
        Node::NumberLit { .. } => text.to_owned(),
        _ => format!("[{}]", paren_text(cx, key)),
    }
}

fn report_run(cx: &mut Ctx<'_, '_>, stmts: &[NodeId], calls: &[DefineCall]) {
    let (Some(&first), Some(&last)) = (stmts.first(), stmts.last()) else {
        return;
    };
    let span = Span::new(cx.span(first).start, cx.span(last).end);
    let target = calls[0].target;
    let fixable = !has_duplicate_keys(cx, calls)
        && !has_comment_in(cx, span)
        && calls.iter().all(|c| !has_multiline_token(cx, c.descriptor))
        && !has_reordered_keys(cx, calls)
        && calls[1..].iter().all(|c| {
            !may_observe_target(cx, c.key, target) && !may_observe_target(cx, c.descriptor, target)
        });
    let fix = fixable.then(|| {
        let src = cx.src();
        let start = span.start as usize;
        // `getIndentString`: the whitespace right before the statement on its line.
        let line = &src[line_start(src, start)..start];
        let indent = &line[line.trim_end().len()..];
        let unit = indent_unit(cx);
        let lb = super::unicorn_ast::linebreak(src);
        let props = calls
            .iter()
            .map(|c| {
                let descriptor = reindent_text(paren_text(cx, c.descriptor), "", &unit);
                format!("{indent}{unit}{}: {descriptor},", key_text(cx, c.key))
            })
            .collect::<Vec<_>>()
            .join(lb);
        Fix::replace(
            span,
            format!(
                "Object.defineProperties({}, {{{lb}{props}{lb}{indent}}});",
                cx.text(skip_parens(cx.ast(), target))
            ),
        )
    });
    cx.report_node(
        calls[0].property,
        "Prefer `Object.defineProperties()` over multiple `Object.defineProperty()` calls.",
    )
    .id("prefer-object-define-properties")
    .fix_opt(fix);
}

impl Rule for PreferObjectDefineProperties {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::Program,
            NodeKind::Block,
            NodeKind::StaticBlock,
            NodeKind::SwitchCase,
            NodeKind::ModuleBlock,
        ]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let stmts = match *cx.node(node) {
            Node::Program { stmts } | Node::Block { stmts } | Node::ModuleBlock { stmts } => stmts,
            Node::StaticBlock { body } | Node::SwitchCase { body, .. } => body,
            _ => return,
        };
        let stmts = ast.list(stmts);
        let mut i = 0;
        while i < stmts.len() {
            let Some(first) = define_call(ast, stmts[i]) else {
                i += 1;
                continue;
            };
            let mut calls = vec![first];
            let mut j = i + 1;
            while let Some(next) = stmts.get(j).and_then(|&s| define_call(ast, s)) {
                if !is_same_reference(cx, first.target, next.target) {
                    break;
                }
                calls.push(next);
                j += 1;
            }
            if calls.len() >= 2 {
                report_run(cx, &stmts[i..j], &calls);
            }
            i = j;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::array_index;
    use crate::config::{ConfigFile, ConfigKind, ResolveOptions, objects_from_eslint_json};

    fn lint(src: &str) -> usize {
        let reg = crate::Registry::global();
        let json = serde_json::json!([{ "files": ["**/*.js"], "rules": {
            "unicorn/prefer-object-define-properties": "error" } }]);
        let objs = objects_from_eslint_json(&json, reg).unwrap();
        let file = ConfigFile::new(ConfigKind::Eslint, None, "/p".into(), objs);
        let fc = file.file_config("a.js", &ResolveOptions::new()).unwrap();
        crate::lint_source(
            "/p/a.js",
            src,
            &fc,
            &crate::NoTypeInfo,
            crate::types::FileKey(0),
        )
        .diagnostics
        .len()
    }

    #[test]
    fn long_runs_and_malformed_input() {
        let run = "Object.defineProperty(o, k, {value: [[[[1]]]]});\n".repeat(2_000);
        assert_eq!(lint(&run), 1);
        for src in [
            "Object.defineProperty(o, 'a', {",
            "Object.defineProperty(o, 'a', {});\nObject.defineProperty(o,",
            "x; Object.defineProperty(o, 'a', {}); Object.defineProperty(o, 'b', {});",
        ] {
            let _ = lint(src);
        }
    }

    #[test]
    fn array_indices() {
        assert_eq!(array_index("0"), Some(0.0));
        assert_eq!(array_index("12"), Some(12.0));
        assert_eq!(array_index("012"), None);
        assert_eq!(array_index("1.5"), None);
        assert_eq!(array_index("4294967295"), None);
        assert_eq!(array_index(""), None);
    }
}
