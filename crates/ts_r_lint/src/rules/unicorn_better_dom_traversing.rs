//! `unicorn/better-dom-traversing`: plainer DOM traversal.
//!
//! - `x.childNodes[0]` / `x.children[0]` (a non-negative integer literal index, plain
//!   non-optional member names) read, not written: `.firstChild` / `.firstElementChild` as a
//!   suggestion (none with a comment inside). Another `x.children[N]` is a hint toward
//!   `.querySelector()`; the inner one of `x.children[1].children[2]` and React's
//!   `props.children[N]` / `x.props.children[N]` are left alone.
//! - The outermost `x.parentElement.parentElement…` chain: a hint toward `.closest()`.
//! - The outermost `x.querySelector("a").querySelector("b")…` of two or more single,
//!   static-string selectors (no optional link in the chain, the root not optional): merged
//!   into `x.querySelector(":scope a b")` (no `:scope` on `document`, `window.document`,
//!   `globalThis.document`) as a suggestion, unless a selector has a `,` or `:scope` or a
//!   comment would go. The quote follows the innermost selector.
//!
//! None of them when the receiver can't be a node (a literal, function, class, array,
//! object, `undefined`).

use super::prelude::*;
use super::unicorn_static::static_string_literal;
use super::unicorn_text::has_comment_in;
use super::unicorn_util_h5::{
    escape_string, is_left_hand_side, is_value_not_dom_node, member_object_text, method_call_of,
};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/better-dom-traversing",
        "Prefer better DOM traversal APIs.",
        NO_PRESET,
        Category::Style,
    )
    .suggestions(),
    create: create_default::<BetterDomTraversing>,
};

#[derive(Debug, Default)]
struct BetterDomTraversing;

/// `<obj>.<name>` (non-optional, not computed), through parentheses: its object.
fn named_member(ast: &ts_r_ast::Ast, id: NodeId, name: &str) -> Option<NodeId> {
    match *ast.node(skip_parens(ast, id)) {
        Node::Member {
            obj,
            optional: false,
            prop,
        } if ident_name(ast, prop) == Some(name) => Some(obj),
        _ => None,
    }
}

/// `<obj>[N]` with `N` a non-negative safe integer literal: (object, `N`).
fn numeric_index(ast: &ts_r_ast::Ast, id: NodeId) -> Option<(NodeId, f64)> {
    let Node::Index {
        obj,
        optional: false,
        index,
    } = *ast.node(skip_parens(ast, id))
    else {
        return None;
    };
    let Node::NumberLit { value } = *ast.node(skip_parens(ast, index)) else {
        return None;
    };
    let n = value.get();
    (n >= 0.0 && n.fract() == 0.0 && n <= 9_007_199_254_740_991.0).then_some((obj, n))
}

fn is_props_children(ast: &ts_r_ast::Ast, collection: NodeId) -> bool {
    named_member(ast, collection, "children").is_some_and(|obj| {
        ident_name(ast, skip_parens(ast, obj)) == Some("props")
            || named_member(ast, obj, "props").is_some()
    })
}

/// `node` is the object of a `.children` that is itself indexed.
fn is_nested_indexed_collection(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let Some(parent) = parent_skip_parens(ast, cx.file.parents, node) else {
        return false;
    };
    let Node::Member { obj, .. } = *ast.node(parent) else {
        return false;
    };
    skip_parens(ast, obj) == node
        && named_member(ast, parent, "children").is_some()
        && parent_skip_parens(ast, cx.file.parents, parent)
            .and_then(|g| numeric_index(ast, g))
            .is_some_and(|(o, _)| skip_parens(ast, o) == parent)
}

fn check_indexed_collection(cx: &mut Ctx<'_, '_>, node: NodeId) {
    let ast = cx.ast();
    let Some((collection, index)) = numeric_index(ast, node) else {
        return;
    };
    let inner = skip_parens(ast, collection);
    let (name, owner) = if let Some(o) = named_member(ast, inner, "childNodes") {
        ("childNodes", o)
    } else if let Some(o) = named_member(ast, inner, "children") {
        ("children", o)
    } else {
        return;
    };
    if is_left_hand_side(cx, node)
        || is_value_not_dom_node(ast, owner)
        || (name == "children" && is_props_children(ast, inner))
        || is_nested_indexed_collection(cx, node)
    {
        return;
    }
    let (id, message, suggestion) = match (name, index == 0.0) {
        ("childNodes", true) => (
            "first-child",
            "Prefer `.firstChild` over `.childNodes[0]`.",
            Some((
                "suggestion-first-child",
                "Switch to `.firstChild`.",
                "firstChild",
            )),
        ),
        ("children", true) => (
            "first-element-child",
            "Prefer `.firstElementChild` over `.children[0]`.",
            Some((
                "suggestion-first-element-child",
                "Switch to `.firstElementChild`.",
                "firstElementChild",
            )),
        ),
        ("children", false) => (
            "query-selector",
            "Consider `.querySelector()` over positional child traversal.",
            None,
        ),
        _ => return,
    };
    let span = cx.span(node);
    let fix =
        suggestion
            .filter(|_| !has_comment_in(cx, span))
            .and_then(|(sid, desc, replacement)| {
                let Node::Member { prop, .. } = *ast.node(inner) else {
                    return None;
                };
                let fix = Fix::replace(cx.span(prop), replacement)
                    .and(Span::new(cx.span(collection).end, span.end), "");
                Some((sid, desc, fix))
            });
    let report = cx.report_node(node, message).id(id);
    if let Some((sid, desc, fix)) = fix {
        report.suggest(sid, desc, fix);
    }
}

fn check_parent_element_chain(cx: &mut Ctx<'_, '_>, node: NodeId) {
    let ast = cx.ast();
    let Some(obj) = named_member(ast, node, "parentElement") else {
        return;
    };
    if named_member(ast, obj, "parentElement").is_none() {
        return;
    }
    if let Some(parent) = parent_skip_parens(ast, cx.file.parents, node)
        && let Node::Member { obj: o, .. } = *ast.node(parent)
        && skip_parens(ast, o) == node
        && named_member(ast, parent, "parentElement").is_some()
    {
        return;
    }
    let mut root = obj;
    for _ in 0..10_000 {
        match named_member(ast, root, "parentElement") {
            Some(o) => root = o,
            None => break,
        }
    }
    if is_value_not_dom_node(ast, root) {
        return;
    }
    cx.report_node(
        node,
        "Consider `.closest()` over chaining `.parentElement`.",
    )
    .id("closest");
}

/// `<obj>.querySelector(<static string>)`: (object, selector).
fn static_query_selector(ast: &ts_r_ast::Ast, id: NodeId) -> Option<(NodeId, NodeId, String)> {
    let (obj, _, &[arg]) = method_call_of(
        ast,
        skip_parens(ast, id),
        &["querySelector"],
        1,
        Some(1),
        Some(false),
    )?
    else {
        return None;
    };
    let arg = skip_parens(ast, arg);
    Some((obj, arg, static_string_literal(ast, arg)?))
}

fn is_query_selector_call(ast: &ts_r_ast::Ast, id: NodeId) -> bool {
    method_call_of(ast, id, &["querySelector"], 1, Some(1), Some(false)).is_some()
}

/// The call is the object of another `querySelector` call with a static selector.
fn is_followed_by_static_query(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let Some(member) = parent_skip_parens(ast, cx.file.parents, node) else {
        return false;
    };
    let Node::Member { obj, .. } = *ast.node(member) else {
        return false;
    };
    if skip_parens(ast, obj) != node {
        return false;
    }
    parent_skip_parens(ast, cx.file.parents, member).is_some_and(|call| {
        matches!(*ast.node(call), Node::Call { callee, .. } if skip_parens(ast, callee) == member)
            && static_query_selector(ast, call).is_some()
    })
}

/// An optional link on the member/call spine of `id` (stopping at parentheses).
fn spine_is_optional(ast: &ts_r_ast::Ast, mut id: NodeId) -> bool {
    for _ in 0..10_000 {
        id = match *ast.node(id) {
            Node::Member { optional: true, .. }
            | Node::Index { optional: true, .. }
            | Node::Call { optional: true, .. } => return true,
            Node::Member { obj, .. } | Node::Index { obj, .. } => obj,
            Node::Call { callee, .. } => callee,
            _ => return false,
        };
    }
    false
}

/// The call sits inside an optional chain (unicorn's `ChainExpression`).
fn is_part_of_chain(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let mut top = node;
    for _ in 0..10_000 {
        let Some(p) = cx.parent(top) else {
            break;
        };
        let up = match *ast.node(p) {
            Node::Member { obj, .. } | Node::Index { obj, .. } => obj == top,
            Node::Call { callee, .. } => callee == top,
            _ => false,
        };
        if !up {
            break;
        }
        top = p;
    }
    spine_is_optional(ast, top)
}

fn is_document_object(ast: &ts_r_ast::Ast, id: NodeId) -> bool {
    let id = skip_parens(ast, id);
    ident_name(ast, id) == Some("document")
        || named_member(ast, id, "document").is_some_and(|o| {
            matches!(
                ident_name(ast, skip_parens(ast, o)),
                Some("globalThis" | "window")
            )
        })
}

fn check_query_selector_chain(cx: &mut Ctx<'_, '_>, node: NodeId) {
    let ast = cx.ast();
    if !is_query_selector_call(ast, node)
        || is_followed_by_static_query(cx, node)
        || is_part_of_chain(cx, node)
    {
        return;
    }
    let mut selectors = Vec::new();
    let mut quote_node = None;
    let mut root = node;
    for _ in 0..10_000 {
        let Some((obj, arg, selector)) = static_query_selector(ast, root) else {
            break;
        };
        selectors.push(selector);
        quote_node = Some(arg);
        root = obj;
    }
    let root = skip_parens(ast, root);
    let Some(quote_node) = quote_node else {
        return;
    };
    if selectors.len() < 2 || spine_is_optional(ast, root) || is_value_not_dom_node(ast, root) {
        return;
    }
    selectors.reverse();
    let mergeable = selectors
        .iter()
        .all(|s| !s.contains(',') && !s.contains(":scope"));
    let suggestion = (mergeable && !has_comment_in(cx, cx.span(node))).then(|| {
        let scope = if is_document_object(ast, root) {
            ""
        } else {
            ":scope "
        };
        let quote = (matches!(ast.node(quote_node), Node::StringLit { .. })
            && cx.text(quote_node).starts_with('"'))
        .then_some('"');
        let selector = escape_string(&format!("{scope}{}", selectors.join(" ")), quote);
        let text = format!(
            "{}.querySelector({selector})",
            member_object_text(cx, root, node)
        );
        Fix::replace(cx.span(node), text)
    });
    let report = cx
        .report_node(node, "Consider merging chained `.querySelector()` calls.")
        .id("merge-query-selector");
    if let Some(fix) = suggestion {
        report.suggest(
            "suggestion-merge-query-selector",
            "Merge the `.querySelector()` calls.",
            fix,
        );
    }
}

impl Rule for BetterDomTraversing {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Index, NodeKind::Member, NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        match cx.node(node) {
            Node::Index { .. } => check_indexed_collection(cx, node),
            Node::Member { .. } => check_parent_element_chain(cx, node),
            Node::Call { .. } => check_query_selector_chain(cx, node),
            _ => {}
        }
    }
}
