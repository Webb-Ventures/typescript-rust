//! `unicorn/prefer-object-from-entries`: `Object.fromEntries(pairs)` over building an object
//! from pairs.
//!
//! - `pairs.reduce(o => ({...o, key}), {})` and `pairs.reduce(o => Object.assign(o, {key}),
//!   {})`: a non-async arrow whose first parameter is an identifier referenced exactly once
//!   (by the spread / `assign` target), one plain property (not a method or accessor, not a
//!   non-computed `__proto__`), an empty `{}` initial value, and a receiver not known to be a
//!   non-array (typed arrays map to numbers). Reported on `reduce`; fixed (unless the call
//!   has type arguments or a comment sits in the callback or before the `{}`) to
//!   `Object.fromEntries(pairs.map((...rest) => [key, value]))`, an identifier key quoted.
//! - `const o = {};` followed directly by `for (const|let [k, v] of pairs) o[k] = v;` (the
//!   loop's only statement; `o` not one of the pair names and not used in `pairs`): the loop
//!   is reported and, with no comment from the declaration to the loop's end, becomes
//!   `const o = Object.fromEntries(pairs);`.
//! - A one-argument call of `_.fromPairs`, `lodash.fromPairs` or a configured `functions`
//!   name or path is replaced by `Object.fromEntries`.

use super::prelude::*;
use super::unicorn_ast::remove_statements;
use super::unicorn_text::paren_span;
use super::unicorn_text::paren_text;
use super::unicorn_types::is_known_non_array;
use super::unicorn_util::{has_type_arguments, method_call_of, node_matches_path};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-object-from-entries",
        "Prefer using `Object.fromEntries(…)` to transform a list of key-value pairs into an object.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<PreferObjectFromEntries>,
};

#[derive(Debug, Default)]
struct PreferObjectFromEntries {
    functions: Vec<String>,
}

impl FromOptions for PreferObjectFromEntries {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        let mut functions: Vec<String> = o
            .get("functions")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(|s| s.trim().to_owned())
                    .collect()
            })
            .unwrap_or_default();
        functions.push("_.fromPairs".to_owned());
        functions.push("lodash.fromPairs".to_owned());
        Ok(PreferObjectFromEntries { functions })
    }
}

impl Rule for PreferObjectFromEntries {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::VarDecl, NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        match *cx.node(node) {
            Node::VarDecl { .. } => check_loop(cx, node),
            Node::Call { .. } => {
                check_reduce(cx, node);
                self.check_function(cx, node);
            }
            _ => {}
        }
    }
}

impl PreferObjectFromEntries {
    fn check_function(&self, cx: &mut Ctx<'_, '_>, node: NodeId) {
        let ast = cx.ast();
        let Node::Call {
            callee,
            optional: false,
            args,
            ..
        } = *ast.node(node)
        else {
            return;
        };
        let [arg] = *ast.list(args) else {
            return;
        };
        if matches!(ast.node(arg), Node::Spread { .. }) {
            return;
        }
        let callee = skip_parens(ast, callee);
        for name in &self.functions {
            if node_matches_path(ast, callee, name) {
                let fix = Fix::replace(cx.span(callee), "Object.fromEntries");
                cx.report_node(
                    callee,
                    format!("Prefer `Object.fromEntries()` over `{name}()`."),
                )
                .id("function")
                .fix(fix);
            }
        }
    }
}

fn is_empty_object(ast: &ts_r_ast::Ast, id: NodeId) -> bool {
    matches!(*ast.node(skip_parens(ast, id)), Node::ObjectLit { props } if ast.list(props).is_empty())
}

/// A plain `key: value` / shorthand property: the key (inner node), whether it is computed,
/// and the value.
fn plain_property(ast: &ts_r_ast::Ast, prop: NodeId) -> Option<(NodeId, bool, NodeId)> {
    let (key, value) = match *ast.node(prop) {
        Node::Property { key, value, .. } => (key, value),
        Node::ShorthandProp {
            name, init: None, ..
        } => (name, name),
        _ => return None,
    };
    let (key, computed) = match *ast.node(key) {
        Node::ComputedKey { expr } => (expr, true),
        _ => (key, false),
    };
    if !computed {
        let name = match *ast.node(key) {
            Node::Ident { name } | Node::StringLit { value: name } => Some(ast.atom(name)),
            _ => None,
        };
        if name == Some("__proto__") {
            return None;
        }
    }
    Some((key, computed, value))
}

fn same_identifier(ast: &ts_r_ast::Ast, a: NodeId, b: NodeId) -> bool {
    let (a, b) = (skip_parens(ast, a), skip_parens(ast, b));
    ident_name(ast, a).is_some() && ident_name(ast, a) == ident_name(ast, b)
}

fn check_reduce(cx: &mut Ctx<'_, '_>, node: NodeId) {
    let ast = cx.ast();
    let Some((obj, prop, &[callback, init])) =
        method_call_of(ast, node, &["reduce"], 2, Some(2), Some(false))
    else {
        return;
    };
    if !is_empty_object(ast, init) {
        return;
    }
    let callback = skip_parens(ast, callback);
    let Node::Arrow {
        flags,
        params,
        body,
        ..
    } = *ast.node(callback)
    else {
        return;
    };
    if flags.contains(FnFlags::ASYNC) {
        return;
    }
    let params = ast.list(params);
    let Some(&first) = params.first() else {
        return;
    };
    let Node::Param {
        pattern,
        init: None,
        rest: false,
        ..
    } = *ast.node(first)
    else {
        return;
    };
    if !matches!(ast.node(pattern), Node::Ident { .. }) {
        return;
    }
    let body_inner = skip_parens(ast, body);
    let property = match *ast.node(body_inner) {
        Node::ObjectLit { props } => match *ast.list(props) {
            [spread, p] => match *ast.node(spread) {
                Node::Spread { arg } if same_identifier(ast, pattern, arg) => Some(p),
                _ => None,
            },
            _ => None,
        },
        Node::Call { .. } => {
            match method_call_of(ast, body_inner, &["assign"], 2, Some(2), Some(false)) {
                Some((o, _, &[target, source]))
                    if ident_name(ast, skip_parens(ast, o)) == Some("Object")
                        && same_identifier(ast, pattern, target) =>
                {
                    match *ast.node(skip_parens(ast, source)) {
                        Node::ObjectLit { props } => match *ast.list(props) {
                            [p] => Some(p),
                            _ => None,
                        },
                        _ => None,
                    }
                }
                _ => None,
            }
        }
        _ => None,
    };
    let Some(property) = property else {
        return;
    };
    let Some((key, computed, value)) = plain_property(ast, property) else {
        return;
    };
    if is_known_non_array(cx, obj) {
        return;
    }
    let sem = cx.semantic();
    let Some(symbol) = sem.symbol_of_decl(pattern) else {
        return;
    };
    if sem.symbol(symbol).decls.len() != 1 || sem.symbol(symbol).refs.len() != 1 {
        return;
    }
    let fix = (!has_type_arguments(cx, node))
        .then(|| {
            reduce_fix(
                cx,
                node,
                prop,
                callback,
                init,
                first,
                params.len(),
                body,
                key,
                computed,
                value,
            )
        })
        .flatten();
    cx.report_node(prop, "Prefer `Object.fromEntries()` over `Array#reduce()`.")
        .id("reduce")
        .fix_opt(fix);
}

#[allow(clippy::too_many_arguments)]
fn reduce_fix(
    cx: &Ctx<'_, '_>,
    call: NodeId,
    prop: NodeId,
    callback: NodeId,
    init: NodeId,
    first: NodeId,
    param_count: usize,
    body: NodeId,
    key: NodeId,
    computed: bool,
    value: NodeId,
) -> Option<Fix> {
    let ast = cx.ast();
    let src = cx.src();
    if !ast.comments_in(cx.span(callback)).is_empty() {
        return None;
    }
    // The initial value with its parentheses and the comma before it.
    let init_span = paren_span(cx, skip_parens(ast, init));
    let callback_end = paren_span(cx, callback).end;
    let comma = src[callback_end as usize..init_span.start as usize]
        .find(',')
        .map(|i| callback_end + i as u32)?;
    let init_range = Span::new(comma, init_span.end);
    if !ast.comments_in(init_range).is_empty() {
        return None;
    }
    let call_span = cx.span(call);
    let mut fix = Fix::insert(call_span.start, "Object.fromEntries(")
        .and(cx.span(prop), "map")
        .and(init_range, "");
    // The first parameter, with the comma and gap after it.
    let first_span = cx.span(first);
    let after = &src[first_span.end as usize..];
    let trimmed = after.trim_start();
    if trimmed.starts_with(',') {
        let comma_at = first_span.end + (after.len() - trimmed.len()) as u32;
        let rest = &src[comma_at as usize + 1..];
        let next = comma_at + 1 + (rest.len() - rest.trim_start().len()) as u32;
        fix = fix.and(Span::new(comma_at, next), "");
    }
    let mut add_parens = false;
    if param_count == 1 {
        // The token before `=>`.
        let arrow = src[first_span.end as usize..cx.span(body).start as usize]
            .find("=>")
            .map(|i| first_span.end as usize + i)?;
        add_parens = !src[..arrow].trim_end().ends_with(')');
    }
    fix = fix.and(first_span, if add_parens { "()" } else { "" });
    let mut key_text = paren_text(cx, key).to_owned();
    if !computed && matches!(ast.node(key), Node::Ident { .. }) {
        key_text = format!("'{key_text}'");
    }
    let value_text = paren_text(cx, value);
    let body_inner = skip_parens(ast, body);
    fix = fix.and(cx.span(body_inner), format!("[{key_text}, {value_text}]"));
    let mut cur = body_inner;
    while let Some(p) = cx.parent(cur) {
        if !matches!(cx.node(p), Node::Paren { .. }) {
            break;
        }
        let s = cx.span(p);
        fix = fix
            .and(Span::new(s.start, s.start + 1), "")
            .and(Span::new(s.end - 1, s.end), "");
        cur = p;
    }
    Some(fix.and(Span::at(call_span.end), ")"))
}

/// The statement after `stmt` in its statement list.
fn next_statement(cx: &Ctx<'_, '_>, stmt: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let parent = cx.parent(stmt)?;
    let list = match *ast.node(parent) {
        Node::Program { stmts } | Node::Block { stmts } | Node::ModuleBlock { stmts } => stmts,
        Node::SwitchCase { body, .. } => body,
        _ => return None,
    };
    let list = ast.list(list);
    let i = list.iter().position(|&s| s == stmt)?;
    list.get(i + 1).copied()
}

fn check_loop(cx: &mut Ctx<'_, '_>, decl: NodeId) {
    let ast = cx.ast();
    let Node::VarDecl {
        decls, modifiers, ..
    } = *ast.node(decl)
    else {
        return;
    };
    if modifiers.contains(Modifiers::EXPORT) {
        return;
    }
    let [declarator] = *ast.list(decls) else {
        return;
    };
    let Node::VarDeclarator {
        name,
        init: Some(init),
        ..
    } = *ast.node(declarator)
    else {
        return;
    };
    if !matches!(ast.node(name), Node::Ident { .. }) || !is_empty_object(ast, init) {
        return;
    }
    let Some(lp) = next_statement(cx, decl) else {
        return;
    };
    let Node::ForOf {
        is_await: false,
        left,
        right,
        body,
    } = *ast.node(lp)
    else {
        return;
    };
    let Node::VarDecl {
        kind: VarKind::Const | VarKind::Let,
        decls: loop_decls,
        ..
    } = *ast.node(left)
    else {
        return;
    };
    let [loop_declarator] = *ast.list(loop_decls) else {
        return;
    };
    let Node::VarDeclarator { name: pattern, .. } = *ast.node(loop_declarator) else {
        return;
    };
    let Node::ArrayPattern { elements } = *ast.node(pattern) else {
        return;
    };
    let [k, v] = *ast.list(elements) else {
        return;
    };
    if !matches!(ast.node(k), Node::Ident { .. })
        || !matches!(ast.node(v), Node::Ident { .. })
        || same_identifier(ast, k, name)
        || same_identifier(ast, v, name)
    {
        return;
    }
    // The object's name used inside `pairs`.
    let sem = cx.semantic();
    if let Some(symbol) = sem.symbol_of_decl(name) {
        let rs = cx.span(right);
        let var = sem.symbol(symbol);
        let inside = |n: NodeId| {
            let s = cx.span(n);
            s.start >= rs.start && s.end <= rs.end
        };
        if var.decls.iter().any(|d| inside(d.name))
            || var.refs.iter().any(|&r| inside(sem.reference(r).node))
        {
            return;
        }
    }
    let stmt = match *ast.node(body) {
        Node::Block { stmts } => match *ast.list(stmts) {
            [only] => only,
            _ => return,
        },
        _ => body,
    };
    let Node::ExprStmt { expr } = *ast.node(stmt) else {
        return;
    };
    let Node::Assign {
        left: target,
        op: AssignOp::Assign,
        right: assigned,
    } = *ast.node(skip_parens(ast, expr))
    else {
        return;
    };
    let Node::Index { obj, index, .. } = *ast.node(skip_parens(ast, target)) else {
        return;
    };
    if !same_identifier(ast, name, obj)
        || !same_identifier(ast, k, index)
        || !same_identifier(ast, v, assigned)
    {
        return;
    }
    let src = cx.src().as_bytes();
    let comments = ast.comments_in(Span::new(cx.span(lp).end, src.len() as u32));
    let mut next_token = cx.span(lp).end as usize;
    for _ in 0..100_000 {
        while next_token < src.len() && src[next_token].is_ascii_whitespace() {
            next_token += 1;
        }
        match comments
            .iter()
            .find(|c| c.span.start as usize == next_token)
        {
            Some(c) => next_token = c.span.end as usize,
            None => break,
        }
    }
    let blocked = !ast
        .comments_in(Span::new(cx.span(decl).start, next_token as u32))
        .is_empty();
    let fix = (!blocked).then(|| {
        let init_inner = skip_parens(ast, init);
        Fix::replace(
            cx.span(init_inner),
            format!(
                "Object.fromEntries({})",
                paren_text(cx, skip_parens(ast, right))
            ),
        )
        .and_fix(remove_statements(cx, lp, lp, false))
    });
    cx.report_node(lp, "Prefer `Object.fromEntries()` over a `for-of` loop.")
        .id("loop")
        .fix_opt(fix);
}

trait AndFix {
    fn and_fix(self, other: Fix) -> Fix;
}

impl AndFix for Fix {
    fn and_fix(mut self, other: Fix) -> Fix {
        for e in other.edits {
            self = self.and(e.span, e.text);
        }
        self
    }
}
