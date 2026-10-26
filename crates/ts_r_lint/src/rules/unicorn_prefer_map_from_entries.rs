//! `unicorn/prefer-map-from-entries`: `const x = Object.fromEntries(entries)` (unannotated,
//! not exported, one declaration, the global `Object` and `Map` not shadowed by a value,
//! no type arguments) where `entries` is `Object.entries(y)` or a non-empty array literal
//! of `[key, value, ..]` arrays with static string keys that aren't array indexes, and
//! every other reference to `x` can become a `Map` operation:
//!
//! - `Object.hasOwn(x, "k")` → `x.has('k')`; `Object.keys/values/entries(x)` →
//!   `[...x.keys()]` (`;` first when it would continue the previous line);
//! - `x.k` / `x["k"]` (a static name that isn't an array index or an `Object.prototype`
//!   member; computed keys without side effects) → `x.get('k')`; a standalone `x.k = v` →
//!   `x.set('k', v)`; a standalone `delete x.k` → `x.delete('k')`;
//! - never when the access is under a TS wrapper, in a destructuring pattern, in a `new`
//!   callee, called or tagged, updated, compound-assigned, a `for` head, or in an optional
//!   chain.
//!
//! The replacements must not overlap. Reported at the `Object.fromEntries(..)` call; no
//! fix when a comment is inside the callee or inside a replaced reference.

use super::prelude::*;
use super::unicorn_ast::{method_call, no_spread_in};
use super::unicorn_iter::global_name_available;
use super::unicorn_static::static_value;
use super::unicorn_text::{
    call_argument_text, has_comment_in, has_possible_side_effect, needs_semicolon, paren_text,
    static_string_value,
};
use super::unicorn_util::escape_string;
use super::unicorn_util::has_optional_chain;
use ts_r_ast::{Ast, ParentMap};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-map-from-entries",
        "Prefer `new Map()` over `Object.fromEntries()` when using the result as a map.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<PreferMapFromEntries>,
};

#[derive(Debug, Default)]
struct PreferMapFromEntries;

const INHERITED: &[&str] = &[
    "__defineGetter__",
    "__defineSetter__",
    "__lookupGetter__",
    "__lookupSetter__",
    "__proto__",
    "constructor",
    "hasOwnProperty",
    "isPrototypeOf",
    "propertyIsEnumerable",
    "toLocaleString",
    "toString",
    "valueOf",
];

/// `Object.<method>(..)` on the identifier `Object`.
fn object_call<'a>(ast: &'a Ast, id: NodeId) -> Option<super::unicorn_ast::MethodCall<'a>> {
    let call = method_call(ast, id)?;
    let obj = skip_parens(ast, call.obj);
    (call.plain()
        && matches!(ast.node(obj), Node::Ident { .. })
        && ident_name(ast, obj) == Some("Object"))
    .then_some(call)
}

fn is_array_index(key: &str) -> bool {
    key.parse::<u64>()
        .is_ok_and(|n| n <= (1u64 << 32) - 2 && n.to_string() == key)
}

fn is_object_entries_call(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    object_call(ast, id).is_some_and(|c| {
        c.name == "entries"
            && c.args.len() == 1
            && no_spread_in(ast, c.args, 1)
            && global_name_available(cx, "Object", id)
    })
}

fn is_string_key_entry(ast: &Ast, id: NodeId) -> bool {
    let Node::ArrayLit { elements } = *ast.node(skip_parens(ast, id)) else {
        return false;
    };
    let elements = ast.list(elements);
    elements.len() >= 2
        && !matches!(ast.node(elements[0]), Node::Hole {} | Node::Spread { .. })
        && static_string_value(ast, skip_parens(ast, elements[0]))
            .is_some_and(|k| !is_array_index(&k))
}

fn is_known_string_key_entries(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let id = skip_parens(ast, id);
    if is_object_entries_call(cx, id) {
        return true;
    }
    let Node::ArrayLit { elements } = *ast.node(id) else {
        return false;
    };
    let elements = ast.list(elements);
    !elements.is_empty()
        && elements
            .iter()
            .all(|&e| !matches!(ast.node(e), Node::Spread { .. }) && is_string_key_entry(ast, e))
}

/// The parent through parentheses (ESTree has none).
fn parent_np(cx: &Ctx<'_, '_>, id: NodeId) -> Option<NodeId> {
    parent_skip_parens(cx.ast(), cx.file.parents, id)
}

/// The outermost parenthesized form of `id`.
fn outer(cx: &Ctx<'_, '_>, id: NodeId) -> NodeId {
    let mut cur = id;
    while let Some(p) = cx.parent(cur) {
        if !matches!(cx.node(p), Node::Paren { .. }) {
            break;
        }
        cur = p;
    }
    cur
}

fn is_ts_wrapper(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(id),
        Node::As { .. }
            | Node::Satisfies { .. }
            | Node::NonNull { .. }
            | Node::TypeAssertion { .. }
            | Node::Instantiation { .. }
    )
}

fn in_destructuring(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    ancestors(cx.file.parents, id).any(|a| match ast.node(a) {
        Node::ArrayPattern { .. } | Node::ObjectPattern { .. } => true,
        Node::ArrayLit { .. } | Node::ObjectLit { .. } => {
            is_assignment_target(ast, cx.file.parents, a)
        }
        _ => false,
    })
}

/// Through TS wrappers and member objects, the callee of a `new`.
fn in_new_callee(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let mut cur = id;
    for _ in 0..100_000 {
        let Some(p) = cx.parent(cur) else {
            return false;
        };
        match *ast.node(p) {
            Node::Paren { .. } => {}
            _ if is_ts_wrapper(ast, p) => {}
            Node::Member { obj, .. } | Node::Index { obj, .. } if obj == cur => {}
            Node::New { callee, .. } => return callee == cur,
            _ => return false,
        }
        cur = p;
    }
    false
}

/// An optional link anywhere inside `id`.
fn contains_optional(ast: &Ast, id: NodeId) -> bool {
    let mut stack = vec![id];
    let mut guard = 0u32;
    while let Some(n) = stack.pop() {
        guard += 1;
        if guard > 1_000_000 {
            return true;
        }
        if matches!(
            ast.node(n),
            Node::Member { optional: true, .. }
                | Node::Index { optional: true, .. }
                | Node::Call { optional: true, .. }
        ) {
            return true;
        }
        ast.for_each_child(n, |c| stack.push(c));
    }
    false
}

/// Some enclosing member/call is part of an optional chain.
fn in_optional_chain(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    ancestors(cx.file.parents, id).any(|a| {
        matches!(
            ast.node(a),
            Node::Member { .. } | Node::Index { .. } | Node::Call { .. }
        ) && has_optional_chain(ast, a)
    })
}

fn ancestors<'p>(parents: &'p ParentMap, id: NodeId) -> impl Iterator<Item = NodeId> + 'p {
    std::iter::successors(parents.parent(id), move |&n| parents.parent(n))
}

/// The ESTree parent is an expression statement holding `id`.
fn is_standalone(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    parent_np(cx, id).is_some_and(|p| matches!(cx.node(p), Node::ExprStmt { .. }))
}

/// `id` starts the expression statement it is in.
fn starts_statement(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    ancestors(cx.file.parents, id)
        .find(|&a| matches!(cx.node(a), Node::ExprStmt { .. }))
        .is_some_and(|s| cx.span(s).start == cx.span(id).start)
}

struct Operation {
    span: Span,
    replacement: String,
}

fn object_method_operation(cx: &Ctx<'_, '_>, ident: NodeId) -> Option<Option<Operation>> {
    let ast = cx.ast();
    let parent = parent_np(cx, ident)?;
    let call = object_call(ast, parent)?;
    if !matches!(call.name, "hasOwn" | "entries" | "keys" | "values")
        || !global_name_available(cx, "Object", parent)
        || call.args.first().map(|&a| skip_parens(ast, a)) != Some(ident)
    {
        return None;
    }
    let span = cx.span(parent);
    if call.name == "hasOwn" {
        if call.args.len() != 2 || matches!(ast.node(call.args[1]), Node::Spread { .. }) {
            return Some(None);
        }
        let Some(key) = static_string_value(ast, skip_parens(ast, call.args[1])) else {
            return Some(None);
        };
        let name = ident_name(ast, ident)?;
        return Some(Some(Operation {
            span,
            replacement: format!("{name}.has({})", escape_string(&key, None)),
        }));
    }
    if call.args.len() != 1 {
        return Some(None);
    }
    let replacement = format!("[...{}.{}()]", paren_text(cx, ident), call.name);
    let semicolon = starts_statement(cx, parent) && needs_semicolon(cx, span.start, &replacement);
    Some(Some(Operation {
        span,
        replacement: if semicolon {
            format!(";{replacement}")
        } else {
            replacement
        },
    }))
}

fn member_operation(cx: &Ctx<'_, '_>, ident: NodeId) -> Option<Operation> {
    let ast = cx.ast();
    let member = parent_np(cx, ident)?;
    let (computed, key_node) = match *ast.node(member) {
        Node::Member { obj, prop, .. } if skip_parens(ast, obj) == ident => (false, prop),
        Node::Index { obj, index, .. } if skip_parens(ast, obj) == ident => (true, index),
        _ => return None,
    };
    let up = parent_np(cx, member);
    if up.is_some_and(|p| is_ts_wrapper(ast, p))
        || in_destructuring(cx, member)
        || in_new_callee(cx, member)
        || contains_optional(ast, member)
        || in_optional_chain(cx, member)
    {
        return None;
    }
    let name = if computed {
        if has_possible_side_effect(cx, key_node) {
            return None;
        }
        static_value(cx, key_node)?.to_js_string()?
    } else {
        match ast.node(key_node) {
            Node::Ident { .. } => ident_name(ast, key_node)?.to_owned(),
            _ => return None,
        }
    };
    if is_array_index(&name) || INHERITED.contains(&name.as_str()) {
        return None;
    }
    let key = escape_string(&name, None);
    let target = paren_text(cx, ident);
    let member_outer = outer(cx, member);
    if let Some(p) = up {
        match *ast.node(p) {
            Node::Assign { left, op, right } if left == member_outer => {
                if op != AssignOp::Assign || !is_standalone(cx, p) {
                    return None;
                }
                return Some(Operation {
                    span: cx.span(p),
                    replacement: format!("{target}.set({key}, {})", call_argument_text(cx, right)),
                });
            }
            Node::Unary {
                op: UnaryOp::Delete,
                ..
            } => {
                if !is_standalone(cx, p) {
                    return None;
                }
                let replacement = format!("{target}.delete({key})");
                let span = cx.span(p);
                return Some(Operation {
                    span,
                    replacement: if needs_semicolon(cx, span.start, &replacement) {
                        format!(";{replacement}")
                    } else {
                        replacement
                    },
                });
            }
            Node::Update { .. } => return None,
            Node::Call { callee, .. } | Node::New { callee, .. } if callee == member_outer => {
                return None;
            }
            Node::TaggedTemplate { tag, .. } if tag == member_outer => return None,
            Node::ForIn { left, .. } | Node::ForOf { left, .. } if left == member_outer => {
                return None;
            }
            _ => {}
        }
    }
    Some(Operation {
        span: cx.span(member),
        replacement: format!("{target}.get({key})"),
    })
}

fn operation(cx: &Ctx<'_, '_>, ident: NodeId) -> Option<Operation> {
    match object_method_operation(cx, ident) {
        Some(op) => op,
        None => member_operation(cx, ident),
    }
}

impl Rule for PreferMapFromEntries {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::VarDeclarator]
    }
    fn enter(&self, id: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::VarDeclarator {
            name,
            type_ann: None,
            init: Some(init),
            ..
        } = *ast.node(id)
        else {
            return;
        };
        if !matches!(ast.node(name), Node::Ident { .. }) {
            return;
        }
        let Some(decl) = cx.parent(id) else {
            return;
        };
        let Node::VarDecl {
            kind: VarKind::Const,
            modifiers,
            ..
        } = *ast.node(decl)
        else {
            return;
        };
        if modifiers.contains(Modifiers::EXPORT)
            || cx
                .parent(decl)
                .is_some_and(|p| matches!(cx.node(p), Node::ExportNamed { .. }))
        {
            return;
        }
        let init = skip_parens(ast, init);
        let Some(call) = object_call(ast, init) else {
            return;
        };
        if call.name != "fromEntries"
            || call.args.len() != 1
            || !no_spread_in(ast, call.args, 1)
            || matches!(
                *ast.node(init),
                Node::Call {
                    type_args: Some(_),
                    ..
                }
            )
            || !global_name_available(cx, "Object", init)
            || !global_name_available(cx, "Map", init)
            || !is_known_string_key_entries(cx, call.args[0])
        {
            return;
        }
        let sem = cx.semantic();
        let Some(sym) = sem.symbol_of_decl(name) else {
            return;
        };
        let symbol = sem.symbol(sym);
        if symbol.decls.len() != 1 {
            return;
        }
        let mut operations = Vec::new();
        for &r in &symbol.refs {
            let reference = sem.reference(r);
            if reference.flags.contains(RefFlags::INIT) {
                continue;
            }
            let Some(op) = operation(cx, reference.node) else {
                return;
            };
            operations.push(op);
        }
        if operations.is_empty() {
            return;
        }
        operations.sort_by_key(|o| o.span.start);
        if operations
            .windows(2)
            .any(|w| w[0].span.end > w[1].span.start)
        {
            return;
        }
        let Node::Call { callee, .. } = *ast.node(init) else {
            return;
        };
        let callee_span = cx.span(skip_parens(ast, callee));
        let fix = (!has_comment_in(cx, callee_span)
            && operations.iter().all(|o| !has_comment_in(cx, o.span)))
        .then(|| {
            operations
                .into_iter()
                .fold(Fix::replace(callee_span, "new Map"), |f, o| {
                    f.and(o.span, o.replacement)
                })
        });
        let r = cx
            .report(
                cx.span(init),
                "Prefer `new Map()` over `Object.fromEntries()` when using the result as a map.",
            )
            .id("prefer-map-from-entries");
        if let Some(f) = fix {
            r.fix(f);
        }
    }
}
