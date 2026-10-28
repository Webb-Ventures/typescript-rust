//! `unicorn/no-useless-set-construction`: `new Set(x)` (the unshadowed global, one
//! non-spread argument, no type arguments) that a Set method doesn't need. The methods are
//! `union intersection difference symmetricDifference` (which return a fresh Set) and
//! `isSubsetOf isSupersetOf isDisjointFrom`, called with one argument, without optional
//! chaining. Sets and maps are known from syntax (`unicorn_types::builtin_collection_type`).
//! Three shapes are reported (at the construction):
//! - a copy of a fresh result: `new Set(set.union(other))`;
//! - an argument copy: `set.union(new Set(x))` where `x` is a known Set, or `.keys()` /
//!   `.values()` of one, or `.keys()` of a known Map;
//! - a receiver copy: `new Set(x).union(other)` where `x` (or the Set behind its
//!   `.keys()` / `.values()`) is a known Set and `other` is a known Set or Map without
//!   side effects.
//!
//! The fix replaces the construction with the collection's text, parenthesized when it
//! would start a statement badly, needs it as a member object, or becomes the callee of a
//! `new`; a `;` is added when a statement now starts with `(`/`[`/... and would continue
//! the previous line. No fix when it would remove comments.

use super::prelude::*;
use super::unicorn_ast::method_call;
use super::unicorn_static::{has_side_effect, is_global_ident};
use super::unicorn_text::{
    is_parenthesized, member_object_needs_parens, needs_parens_at_expression_start,
    needs_semicolon, paren_span, paren_text, would_remove_comments,
};
use super::unicorn_types::builtin_collection_type;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-useless-set-construction",
        "Disallow unnecessary `Set` construction around `Set` methods.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<NoUselessSetConstruction>,
};

#[derive(Debug, Default)]
struct NoUselessSetConstruction;

const RETURNING: &[&str] = &["union", "intersection", "difference", "symmetricDifference"];
const ALL: &[&str] = &[
    "union",
    "intersection",
    "difference",
    "symmetricDifference",
    "isSubsetOf",
    "isSupersetOf",
    "isDisjointFrom",
];

/// A Set method call from `methods`: its receiver, name and argument.
fn set_method_call<'a>(
    cx: &Ctx<'a, '_>,
    id: NodeId,
    methods: &[&str],
) -> Option<(NodeId, &'a str, NodeId)> {
    let call = method_call(cx.ast(), id)?;
    if !methods.contains(&call.name)
        || call.optional_call
        || call.optional_member
        || call.args.len() != 1
        || matches!(cx.node(call.args[0]), Node::Spread { .. })
    {
        return None;
    }
    Some((call.obj, call.name, call.args[0]))
}

fn is_builtin_set(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    builtin_collection_type(cx, id) == Some("Set")
}

/// The known Set or Map whose elements `id` iterates as a Set-like argument.
fn collection(cx: &Ctx<'_, '_>, id: NodeId) -> Option<NodeId> {
    let id = skip_parens(cx.ast(), id);
    if is_builtin_set(cx, id) {
        return Some(id);
    }
    let call = method_call(cx.ast(), id)?;
    if !matches!(call.name, "keys" | "values")
        || !call.args.is_empty()
        || call.optional_call
        || call.optional_member
    {
        return None;
    }
    match builtin_collection_type(cx, call.obj) {
        Some("Set") => Some(call.obj),
        Some("Map") if call.name == "keys" => Some(call.obj),
        _ => None,
    }
}

/// The parent through parentheses, and the child on the path to it.
fn parent_np(cx: &Ctx<'_, '_>, id: NodeId) -> Option<(NodeId, NodeId)> {
    let mut cur = id;
    for _ in 0..10_000 {
        let p = cx.parent(cur)?;
        if matches!(cx.node(p), Node::Paren { .. }) {
            cur = p;
        } else {
            return Some((p, cur));
        }
    }
    None
}

fn replacement<'a>(cx: &Ctx<'a, '_>, node: NodeId, arg: NodeId) -> Option<(NodeId, &'a str)> {
    let ast = cx.ast();
    let arg = skip_parens(ast, arg);
    if let Some((obj, name, _)) = set_method_call(cx, arg, RETURNING)
        && is_builtin_set(cx, obj)
    {
        return Some((arg, name));
    }
    let (parent, held) = parent_np(cx, node)?;
    if let Some((obj, name, call_arg)) = set_method_call(cx, parent, ALL)
        && call_arg == held
        && is_builtin_set(cx, obj)
        && let Some(r) = collection(cx, arg)
    {
        return Some((r, name));
    }
    let Node::Member { obj, .. } = *ast.node(parent) else {
        return None;
    };
    if obj != held {
        return None;
    }
    let (call, callee) = parent_np(cx, parent)?;
    let Node::Call { callee: c, .. } = *ast.node(call) else {
        return None;
    };
    if c != callee {
        return None;
    }
    let (_, name, other) = set_method_call(cx, call, ALL)?;
    let r = collection(cx, arg)?;
    (is_builtin_set(cx, r)
        && builtin_collection_type(cx, other).is_some()
        && !has_side_effect(ast, other))
    .then_some((r, name))
}

fn is_ts_wrapper(node: &Node) -> bool {
    matches!(
        node,
        Node::As { .. }
            | Node::Satisfies { .. }
            | Node::NonNull { .. }
            | Node::TypeAssertion { .. }
            | Node::Instantiation { .. }
    )
}

fn fix(cx: &Ctx<'_, '_>, node: NodeId, replacement: NodeId) -> Option<Fix> {
    let ast = cx.ast();
    let span = cx.span(node);
    if would_remove_comments(cx, span, &[paren_span(cx, replacement)]) {
        return None;
    }
    let mut text = paren_text(cx, replacement).to_string();
    let parent = parent_np(cx, node);
    let member_object = matches!(parent, Some((p, held)) if matches!(*ast.node(p), Node::Member { obj, .. } if obj == held));
    // The outermost member object / tag / TS wrapper chain `node` starts.
    let mut callee_parent = parent;
    for _ in 0..10_000 {
        let Some((p, held)) = callee_parent else {
            break;
        };
        let continues = match *ast.node(p) {
            Node::Member { obj, .. } | Node::Index { obj, .. } => obj == held,
            Node::TaggedTemplate { tag, .. } => tag == held,
            ref n => is_ts_wrapper(n),
        };
        if !continues {
            break;
        }
        callee_parent = parent_np(cx, p);
    }
    let new_callee = matches!(callee_parent, Some((p, held)) if matches!(*ast.node(p), Node::New { callee, .. } if callee == held));
    if !is_parenthesized(cx, replacement)
        && (needs_parens_at_expression_start(cx, replacement, node)
            || (member_object && member_object_needs_parens(cx, replacement, node))
            || new_callee)
    {
        text = format!("({text})");
    }
    // A statement that now starts with the replacement may continue the previous line.
    let mut expression = node;
    let mut statement = None;
    for _ in 0..10_000 {
        match cx.parent(expression) {
            Some(p) if matches!(ast.node(p), Node::ExprStmt { .. }) => {
                statement = Some(p);
                break;
            }
            Some(p) => expression = p,
            None => break,
        }
    }
    if statement.is_some()
        && cx.span(expression).start == span.start
        && needs_semicolon(cx, span.start, &text)
    {
        text = format!(";{text}");
    }
    Some(Fix::replace(span, text))
}

impl Rule for NoUselessSetConstruction {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::New]
    }
    fn enter(&self, id: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::New {
            callee,
            type_args: None,
            args,
            ..
        } = *ast.node(id)
        else {
            return;
        };
        let &[arg] = ast.list(args) else {
            return;
        };
        if matches!(ast.node(arg), Node::Spread { .. })
            || !matches!(ast.node(callee), Node::Ident { .. })
            || !is_global_ident(cx, callee, "Set")
        {
            return;
        }
        let Some((r, method)) = replacement(cx, id, arg) else {
            return;
        };
        let method = method.to_string();
        let fix = fix(cx, id, r);
        let r = cx
            .report(
                cx.span(id),
                format!("This `Set` construction is unnecessary for `Set#{method}()`."),
            )
            .id("no-useless-set-construction");
        if let Some(fix) = fix {
            r.fix(fix);
        }
    }
}
