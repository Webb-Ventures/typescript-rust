//! `unicorn/prefer-modern-dom-apis`: `before()` / `replaceWith()` / `append()`-style methods
//! over `insertBefore()`, `replaceChild()`, `insertAdjacent*()` and `removeChild` loops.
//!
//! - `parent.replaceChild(a, b)` / `parent.insertBefore(a, b)`: parent and both arguments
//!   plain identifiers (not `undefined`), nothing optional. With the value unused and no
//!   comment besides the arguments, `b.replaceWith(a)` is a fix and `b.before(a)` a
//!   suggestion (`insertBefore(x, null)` appends).
//! - `ref.insertAdjacentText(pos, x)` / `insertAdjacentElement`: `ref` an identifier, `pos`
//!   a static string among `beforebegin`/`afterbegin`/`beforeend`/`afterend`, `x` a literal,
//!   identifier or static string. Fixed to `ref.before(x)` / `prepend` / `append` / `after`,
//!   unless an `insertAdjacentElement` result is used or a comment would go.
//! - `while (p.firstChild) p.firstChild.remove();` (or `lastChild`; the body a single
//!   expression statement, the same reference, nothing optional anywhere in `p`, `p` possibly
//!   a node and not known to be a non-`ParentNode`, nor `window`/`self`/`globalThis`):
//!   replaced by `p.replaceChildren();`.

use super::prelude::*;
use super::unicorn_ast::is_value_not_usable;
use super::unicorn_text::member_object_needs_parens;
use super::unicorn_text::needs_semicolon;
use super::unicorn_text::would_remove_comments;
use super::unicorn_types::Checker;
use super::unicorn_util::{
    is_same_reference, is_value_not_dom_node, method_call_of, static_string,
};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-modern-dom-apis",
        "Prefer modern DOM APIs.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create_default::<PreferModernDomApis>,
};

#[derive(Debug, Default)]
struct PreferModernDomApis;

pub(crate) fn is_window_global(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    ["globalThis", "self", "window"]
        .iter()
        .any(|n| cx.is_global_ref(id, n))
}

/// The syntax-only receiver check of unicorn's `shouldReportReplaceChildrenReceiver`.
pub(crate) static REPLACE_CHILDREN_RECEIVER: Checker = Checker {
    class_syntax: true,
    target_names: &[
        "Document",
        "DocumentFragment",
        "Element",
        "HTMLDocument",
        "HTMLElement",
        "ParentNode",
        "SVGElement",
        "ShadowRoot",
    ],
    non_target_names: &[
        "Attr",
        "CDATASection",
        "CharacterData",
        "ChildNode",
        "Comment",
        "DocumentType",
        "Node",
        "Text",
        "Window",
        "WindowProxy",
    ],
    non_target_node: Some(is_window_global),
    nullish_in_mixed_union: true,
    mixed_union_non_target: true,
    ..Checker::DEFAULT
};

impl Rule for PreferModernDomApis {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call, NodeKind::While]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        match *cx.node(node) {
            Node::Call { .. } => {
                check_replace_or_insert(cx, node);
                check_insert_adjacent(cx, node);
            }
            Node::While { test, body } => check_replace_children(cx, node, test, body),
            _ => {}
        }
    }
}

fn plain_ident(ast: &ts_r_ast::Ast, id: NodeId) -> Option<&str> {
    ident_name(ast, skip_parens(ast, id))
}

fn check_replace_or_insert(cx: &mut Ctx<'_, '_>, node: NodeId) {
    let ast = cx.ast();
    let Some((obj, prop, &[new_child, old_child])) = method_call_of(
        ast,
        node,
        &["replaceChild", "insertBefore"],
        2,
        Some(2),
        Some(false),
    ) else {
        return;
    };
    let (Some(parent), Some(new_name), Some(old_name)) = (
        plain_ident(ast, obj),
        plain_ident(ast, new_child),
        plain_ident(ast, old_child),
    ) else {
        return;
    };
    if new_name == "undefined" || old_name == "undefined" {
        return;
    }
    let method = ident_name(ast, prop).unwrap_or_default();
    let preferred = if method == "replaceChild" {
        "replaceWith"
    } else {
        "before"
    };
    let ok = is_value_not_usable(cx, node)
        && !would_remove_comments(
            cx,
            cx.span(node),
            &[
                cx.span(skip_parens(ast, new_child)),
                cx.span(skip_parens(ast, old_child)),
            ],
        );
    let fix =
        ok.then(|| Fix::replace(cx.span(node), format!("{old_name}.{preferred}({new_name})")));
    let r = cx
        .report_node(
            node,
            format!(
                "Prefer `{old_name}.{preferred}({new_name})` over `{parent}.{method}({new_name}, {old_name})`."
            ),
        )
        .id("replaceChildOrInsertBefore");
    if let Some(f) = fix {
        if method == "replaceChild" {
            r.fix(f);
        } else {
            r.suggest(
                "replaceChildOrInsertBeforeSuggestion",
                format!("Switch to `{old_name}.{preferred}({new_name})`."),
                f,
            );
        }
    }
}

fn check_insert_adjacent(cx: &mut Ctx<'_, '_>, node: NodeId) {
    let ast = cx.ast();
    let Some((obj, prop, &[position, content])) = method_call_of(
        ast,
        node,
        &["insertAdjacentText", "insertAdjacentElement"],
        2,
        Some(2),
        Some(false),
    ) else {
        return;
    };
    let Some(pos) = static_string(ast, position) else {
        return;
    };
    let content = skip_parens(ast, content);
    let literal = matches!(
        ast.node(content),
        Node::StringLit { .. }
            | Node::NumberLit { .. }
            | Node::BigIntLit { .. }
            | Node::BoolLit { .. }
            | Node::NullLit {}
            | Node::RegexLit { .. }
            | Node::Ident { .. }
    );
    let obj = skip_parens(ast, obj);
    if !(literal || static_string(ast, content).is_some())
        || !matches!(ast.node(obj), Node::Ident { .. })
    {
        return;
    }
    let preferred = match pos {
        "beforebegin" => "before",
        "afterbegin" => "prepend",
        "beforeend" => "append",
        "afterend" => "after",
        _ => return,
    };
    let method = ident_name(ast, prop).unwrap_or_default();
    let reference = cx.text(obj);
    let content_text = cx.text(content);
    let blocked = (method == "insertAdjacentElement" && !is_value_not_usable(cx, node))
        || would_remove_comments(cx, cx.span(node), &[cx.span(obj), cx.span(content)]);
    let fix = (!blocked).then(|| {
        Fix::replace(
            cx.span(node),
            format!("{reference}.{preferred}({content_text})"),
        )
    });
    let position_text = cx.text(skip_parens(ast, position));
    cx.report_node(
        node,
        format!(
            "Prefer `{reference}.{preferred}({content_text})` over `{reference}.{method}({position_text}, {content_text})`."
        ),
    )
    .id("insertAdjacentTextOrInsertAdjacentElement")
    .fix_opt(fix);
}

/// Any optional link anywhere inside `id`.
fn contains_optional(ast: &ts_r_ast::Ast, id: NodeId, depth: u32) -> bool {
    if depth > 500 {
        return true;
    }
    if matches!(
        ast.node(id),
        Node::Member { optional: true, .. }
            | Node::Index { optional: true, .. }
            | Node::Call { optional: true, .. }
    ) {
        return true;
    }
    let mut found = false;
    ast.for_each_child(id, |c| {
        found = found || contains_optional(ast, c, depth + 1)
    });
    found
}

fn check_replace_children(cx: &mut Ctx<'_, '_>, node: NodeId, test: NodeId, body: NodeId) {
    let ast = cx.ast();
    let child = skip_parens(ast, test);
    let Node::Member {
        obj,
        prop,
        optional: false,
    } = *ast.node(child)
    else {
        return;
    };
    let Some(prop_name @ ("firstChild" | "lastChild")) = ident_name(ast, prop) else {
        return;
    };
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
    let Some((remove_obj, _, _)) = method_call_of(
        ast,
        skip_parens(ast, expr),
        &["remove"],
        0,
        Some(0),
        Some(false),
    ) else {
        return;
    };
    if !is_same_reference(cx, child, remove_obj) {
        return;
    }
    let parent = skip_parens(ast, obj);
    if is_value_not_dom_node(ast, parent)
        || contains_optional(ast, parent, 0)
        || REPLACE_CHILDREN_RECEIVER.is_known_non_target(cx, parent)
    {
        return;
    }
    let parent_text = if !matches!(ast.node(parent), Node::Super {})
        && member_object_needs_parens(cx, parent, node)
    {
        format!("({})", cx.text(parent))
    } else {
        cx.text(parent).to_owned()
    };
    let semicolon = if needs_semicolon(cx, cx.span(node).start, &parent_text) {
        ";"
    } else {
        ""
    };
    let fix = (!would_remove_comments(cx, cx.span(node), &[cx.span(parent)])).then(|| {
        Fix::replace(
            cx.span(node),
            format!("{semicolon}{parent_text}.replaceChildren();"),
        )
    });
    cx.report_node(
        node,
        format!(
            "Prefer `{parent_text}.replaceChildren()` over directly removing `.{prop_name}` in a loop."
        ),
    )
    .id("replaceChildren")
    .fix_opt(fix);
}
