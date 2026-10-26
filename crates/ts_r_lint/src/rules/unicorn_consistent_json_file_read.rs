//! `unicorn/consistent-json-file-read`: read a file passed to `JSON.parse(...)` as a string
//! (the default, `"string"`) or as a buffer (`"buffer"`).
//!
//! The one argument of a plain `JSON.parse(x)` is followed through `await`s and through
//! variables declared once by a plain declarator that have exactly two references (the
//! initialising write and this read) to a call of `readFile`/`readFileSync`: a non-optional
//! member call, or a call of a name imported from `fs`, `node:fs`, `fs/promises` or
//! `node:fs/promises`, with one or two arguments and no spread. In string mode a call with
//! one argument is reported (the fix adds `'utf8'`), and a second argument that is
//! statically `undefined`/`null` or `{encoding: undefined | null}` is reported (replaced by
//! `'utf8'` unless it holds a comment). In buffer mode a second argument that is statically
//! `"utf8"`/`"utf-8"` (any case) or `{encoding: <that>}` is reported and removed. An options
//! variable whose `encoding` gets an accessor through `Object.defineProperty(ies)` (also
//! through `const` aliases) is never treated as static.

use super::prelude::*;
use super::unicorn_ast::{method_call, remove_argument};
use super::unicorn_static::{Static, static_value_no_side_effects};
use super::unicorn_text::{has_comment_in, paren_span};
use super::unicorn_util_h1::static_property_name;
use serde_json::Value;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/consistent-json-file-read",
        "Enforce consistent JSON file reads before `JSON.parse()`.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<ConsistentJsonFileRead>,
};

const MAX_DEPTH: u32 = 64;
const FS_MODULES: &[&str] = &["fs", "node:fs", "fs/promises", "node:fs/promises"];

#[derive(Debug, Default)]
struct ConsistentJsonFileRead {
    buffer: bool,
}

impl FromOptions for ConsistentJsonFileRead {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(ConsistentJsonFileRead {
            buffer: opts.first().and_then(Value::as_str) == Some("buffer"),
        })
    }
}

fn skip_awaits(cx: &Ctx<'_, '_>, mut id: NodeId) -> NodeId {
    for _ in 0..MAX_DEPTH {
        let inner = skip_parens(cx.ast(), id);
        match *cx.node(inner) {
            Node::Await { arg } => id = arg,
            _ => return inner,
        }
    }
    id
}

/// The expression `id` stands for: itself, or through single-use variables their initialiser.
fn resolve(cx: &Ctx<'_, '_>, mut id: NodeId) -> Option<NodeId> {
    for _ in 0..MAX_DEPTH {
        id = skip_awaits(cx, id);
        if !matches!(cx.node(id), Node::Ident { .. }) {
            return Some(id);
        }
        let sem = cx.semantic();
        let sym = sem.reference(sem.reference_at(id)?).symbol()?;
        let sym = sem.symbol(sym);
        let [decl] = sym.decls[..] else {
            return None;
        };
        if sym.refs.len() != 2 {
            return None;
        }
        let Node::VarDeclarator { name, init, .. } = *cx.node(decl.node) else {
            return None;
        };
        if name != decl.name {
            return None;
        }
        id = init?;
    }
    None
}

/// The imported name when `callee` is a name imported from an `fs` module.
fn fs_import<'a>(cx: &Ctx<'a, '_>, callee: NodeId) -> Option<&'a str> {
    let ast = cx.ast();
    if !matches!(ast.node(callee), Node::Ident { .. }) {
        return None;
    }
    let sem = cx.semantic();
    let sym = sem.reference(sem.reference_at(callee)?).symbol()?;
    sem.symbol(sym).decls.iter().find_map(|d| {
        let Node::ImportDecl { source, .. } = *ast.node(d.parent) else {
            return None;
        };
        let Node::StringLit { value } = *ast.node(source) else {
            return None;
        };
        if !FS_MODULES.contains(&ast.atom(value)) {
            return None;
        }
        let Node::ImportSpecifier {
            imported, local, ..
        } = *ast.node(d.node)
        else {
            return None;
        };
        let name = imported.unwrap_or(local);
        match ast.node(name) {
            Node::Ident { .. } => ident_name(ast, name),
            _ => None,
        }
    })
}

fn is_read_file_call(cx: &Ctx<'_, '_>, call: NodeId) -> bool {
    let ast = cx.ast();
    let Node::Call {
        callee,
        optional: false,
        args,
        ..
    } = *ast.node(call)
    else {
        return false;
    };
    let args = ast.list(args);
    if !(1..=2).contains(&args.len())
        || args
            .iter()
            .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
    {
        return false;
    }
    let callee = skip_parens(ast, callee);
    if let Some(name) = fs_import(cx, callee) {
        return matches!(name, "readFile" | "readFileSync");
    }
    match *ast.node(callee) {
        Node::Member {
            optional: false, ..
        }
        | Node::Index {
            optional: false, ..
        } => matches!(
            static_property_name(cx, callee).as_deref(),
            Some("readFile" | "readFileSync")
        ),
        _ => false,
    }
}

/// The static name of an object-literal member's key.
fn key_name(cx: &Ctx<'_, '_>, prop: NodeId) -> Option<String> {
    let ast = cx.ast();
    let key = match *ast.node(prop) {
        Node::Property { key, .. } | Node::MethodDef { key, .. } => key,
        Node::ShorthandProp { name, .. } => return ident_name(ast, name).map(str::to_owned),
        _ => return None,
    };
    match *ast.node(key) {
        Node::ComputedKey { expr } => static_value_no_side_effects(cx, expr)?.to_js_string(),
        Node::NumberLit { .. } => static_value_no_side_effects(cx, key)?.to_js_string(),
        _ => crate::ast_util::key_name(ast, cx.src(), key).map(|k| k.into_owned()),
    }
}

fn is_accessor_descriptor(cx: &Ctx<'_, '_>, desc: Option<NodeId>) -> bool {
    let Some(desc) = desc else {
        return false;
    };
    let Node::ObjectLit { props } = *cx.node(desc) else {
        return false;
    };
    cx.ast().list(props).iter().any(|&p| {
        !matches!(cx.node(p), Node::Spread { .. })
            && matches!(key_name(cx, p).as_deref(), Some("get" | "set"))
    })
}

/// `options` (a name) gets an `encoding` accessor through `Object.defineProperty(ies)`,
/// directly or through a `const` alias.
fn has_encoding_accessor(cx: &Ctx<'_, '_>, node: NodeId, depth: u32) -> bool {
    let ast = cx.ast();
    if depth > MAX_DEPTH || !matches!(ast.node(node), Node::Ident { .. }) {
        return false;
    }
    let sem = cx.semantic();
    let sym = match sem
        .reference_at(node)
        .and_then(|r| sem.reference(r).symbol())
    {
        Some(s) => s,
        None => match sem.symbol_of_decl(node) {
            Some(s) => s,
            None => return false,
        },
    };
    sem.symbol(sym).refs.iter().any(|&r| {
        let ident = sem.reference(r).node;
        let Some(parent) = cx.parent(ident) else {
            return false;
        };
        match *ast.node(parent) {
            Node::Call { callee, args, .. } => {
                let args = ast.list(args);
                if args.first() != Some(&ident) {
                    return false;
                }
                let (Node::Member { obj, .. } | Node::Index { obj, .. }) = *ast.node(callee) else {
                    return false;
                };
                if !matches!(ast.node(obj), Node::Ident { .. })
                    || ident_name(ast, obj) != Some("Object")
                {
                    return false;
                }
                match static_property_name(cx, callee).as_deref() {
                    Some("defineProperty") => {
                        args.get(1).is_some_and(|&k| {
                            matches!(static_value_no_side_effects(cx, k), Some(Static::Str(s)) if s == "encoding")
                        }) && is_accessor_descriptor(cx, args.get(2).copied())
                    }
                    Some("defineProperties") => {
                        let Some(&Node::ObjectLit { props }) = args.get(1).map(|&a| ast.node(a))
                        else {
                            return false;
                        };
                        ast.list(props).iter().any(|&p| match *ast.node(p) {
                            Node::Property { value, .. } => {
                                key_name(cx, p).as_deref() == Some("encoding")
                                    && is_accessor_descriptor(cx, Some(value))
                            }
                            _ => false,
                        })
                    }
                    _ => false,
                }
            }
            Node::VarDeclarator {
                name,
                init: Some(init),
                ..
            } => {
                init == ident
                    && matches!(ast.node(name), Node::Ident { .. })
                    && cx.parent(parent).is_some_and(|d| {
                        matches!(
                            cx.node(d),
                            Node::VarDecl {
                                kind: VarKind::Const,
                                ..
                            }
                        )
                    })
                    && has_encoding_accessor(cx, name, depth + 1)
            }
            _ => false,
        }
    })
}

fn is_utf8(v: &Static) -> bool {
    matches!(v, Static::Str(s) if s.eq_ignore_ascii_case("utf8") || s.eq_ignore_ascii_case("utf-8"))
}

/// The value of `encoding` when `v` is an object with that one key.
fn single_encoding(v: &Static) -> Option<&Static> {
    match v {
        Static::Object(entries) => match &entries[..] {
            [(k, v)] if k == "encoding" => Some(v),
            _ => None,
        },
        _ => None,
    }
}

fn is_utf8_encoding(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    if has_encoding_accessor(cx, node, 0) {
        return false;
    }
    let ast = cx.ast();
    if let Node::ObjectLit { props } = *ast.node(node)
        && let [p] = ast.list(props)[..]
        && let Node::Property { value, .. } = *ast.node(p)
        && key_name(cx, p).as_deref() == Some("encoding")
        && static_value_no_side_effects(cx, value).is_some_and(|v| is_utf8(&v))
    {
        return true;
    }
    static_value_no_side_effects(cx, node)
        .is_some_and(|v| is_utf8(&v) || single_encoding(&v).is_some_and(is_utf8))
}

fn is_buffer_encoding(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    if has_encoding_accessor(cx, node, 0) {
        return false;
    }
    static_value_no_side_effects(cx, node)
        .is_some_and(|v| v.is_nullish() || single_encoding(&v).is_some_and(Static::is_nullish))
}

impl Rule for ConsistentJsonFileRead {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some(m) = method_call(ast, node) else {
            return;
        };
        if m.name != "parse"
            || !m.plain()
            || !matches!(ast.node(skip_parens(ast, m.obj)), Node::Ident { .. })
            || ident_name(ast, skip_parens(ast, m.obj)) != Some("JSON")
        {
            return;
        }
        let [arg] = m.args[..] else {
            return;
        };
        if matches!(ast.node(arg), Node::Spread { .. }) {
            return;
        }
        let Some(call) = resolve(cx, arg) else {
            return;
        };
        if !is_read_file_call(cx, call) {
            return;
        }
        let Node::Call { args, .. } = *ast.node(call) else {
            return;
        };
        let args = ast.list(args);
        if !self.buffer {
            if let [file] = args[..] {
                let end = paren_span(cx, file).end;
                let next = skip_trivia_forward(cx.src(), end as usize);
                let fix = if cx.src().as_bytes().get(next) == Some(&b',') {
                    Fix::insert(next as u32 + 1, " 'utf8'")
                } else {
                    Fix::insert(end, ", 'utf8'")
                };
                cx.report(cx.span(call), "Prefer reading the JSON file as a string.")
                    .id("consistent-json-file-read/string")
                    .fix(fix);
                return;
            }
            let options = args[1];
            if !is_buffer_encoding(cx, options) {
                return;
            }
            let span = cx.span(options);
            let fix = (!has_comment_in(cx, span)).then(|| Fix::replace(span, "'utf8'"));
            cx.report(span, "Prefer reading the JSON file as a string.")
                .id("consistent-json-file-read/string")
                .fix_opt(fix);
            return;
        }
        let [_, charset] = args[..] else {
            return;
        };
        if !is_utf8_encoding(cx, charset) {
            return;
        }
        let fix = remove_argument(cx, call, 1);
        cx.report(
            cx.span(charset),
            "Prefer reading the JSON file as a buffer.",
        )
        .id("consistent-json-file-read/buffer")
        .fix_opt(fix);
    }
}
