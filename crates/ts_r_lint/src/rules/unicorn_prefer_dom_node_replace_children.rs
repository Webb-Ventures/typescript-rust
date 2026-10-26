//! `unicorn/prefer-dom-node-replace-children`: `.replaceChildren()` over emptying a node by
//! hand, and one `.replaceChildren(…)` over emptying then adding.
//!
//! - `node.innerHTML = ""` (or `` `` ``, a static `innerHTML` key): fixed to
//!   `node.replaceChildren();` when the assignment is its own statement and no comment
//!   outside `node` would go. Not on `document` (also through `window.document`-style
//!   globals and `const` aliases), on a `createElement("template")` /
//!   `createElementNS(<HTML or unknown namespace>, "template")` result, or on a possible
//!   `HTMLTemplateElement`; `Document`, `DocumentFragment` and `ParentNode` have no
//!   `innerHTML`.
//! - `while (p.firstChild) p.removeChild(p.firstChild);` (or `lastChild`, the body one
//!   expression): replaced by `p.replaceChildren();` unless a comment would go.
//! - `p.replaceChildren(); p.append(…)` / `prepend(…)` as consecutive statements on the same
//!   reference: the first is removed and the second renamed. It is a suggestion when an
//!   argument is not a plain literal (a spread, a template with expressions, anything that
//!   could be a node), `p` may have side effects (getters count), or `p` is a document;
//!   no fix at all when the first call holds a comment.
//!
//! In every form the receiver is skipped when it can't be a node (a literal, function,
//! class, `undefined`), contains an optional chain, or is known not to be a `ParentNode`.

use super::prelude::*;
use super::unicorn_abort::next_statement;
use super::unicorn_no_array_reduce::const_initializer;
use super::unicorn_prefer_modern_dom_apis::{REPLACE_CHILDREN_RECEIVER, is_window_global};
use super::unicorn_static::{
    Static, has_side_effect, has_side_effect_with_getters, mutable_member_access,
    static_string_literal, static_value_no_side_effects,
};
use super::unicorn_types::Checker;
use super::unicorn_util::skip_wrappers;
use super::unicorn_util_h1::static_property_name;
use super::unicorn_util_h5::{
    is_same_reference, is_value_not_dom_node, member_object_needs_parens, method_call_of,
    needs_semicolon, remove_statement, would_remove_comments,
};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-dom-node-replace-children",
        "Prefer `.replaceChildren()` when replacing DOM children.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create_default::<PreferDomNodeReplaceChildren>,
};

#[derive(Debug, Default)]
struct PreferDomNodeReplaceChildren;

const HTML_NAMESPACE: &str = "http://www.w3.org/1999/xhtml";
const MAX_DEPTH: usize = 64;

/// unicorn's `isKnownNonDomNode` with nullish union members ignored and any non-node
/// member deciding.
static NON_DOM_NODE: Checker = Checker {
    target_names: &[
        "CharacterData",
        "Document",
        "DocumentFragment",
        "Element",
        "HTMLElement",
        "Node",
        "SVGElement",
        "Text",
    ],
    nullish_in_mixed_union: true,
    mixed_union_non_target: true,
    ..Checker::DEFAULT
};

/// Receivers with both `replaceChildren()` and `innerHTML`.
static INNER_HTML_RECEIVER: Checker = Checker {
    class_syntax: true,
    target_names: &["Element", "HTMLElement", "SVGElement", "ShadowRoot"],
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
        "Document",
        "DocumentFragment",
        "ParentNode",
    ],
    non_target_node: Some(is_window_global),
    nullish_in_mixed_union: true,
    mixed_union_non_target: true,
    ..Checker::DEFAULT
};

static TEMPLATE_ELEMENT: Checker = Checker {
    class_syntax: true,
    target_names: &["HTMLTemplateElement"],
    nullish_in_mixed_union: true,
    mixed_union_target: true,
    ..Checker::DEFAULT
};

fn document_node(cx: &Ctx<'_, '_>, id: NodeId, _: &dyn Fn(NodeId) -> bool) -> bool {
    is_global_document(cx, id, &mut Vec::new())
}

static DOCUMENT: Checker = Checker {
    class_syntax: true,
    mixed_union_target: true,
    target_names: &["Document", "HTMLDocument", "XMLDocument"],
    target_node: Some(document_node),
    ..Checker::DEFAULT
};

/// The global `document`, `window.document` (or `globalThis`, `self`, `top`, `parent`,
/// `frames`), through `const` aliases.
fn is_global_document(cx: &Ctx<'_, '_>, id: NodeId, seen: &mut Vec<NodeId>) -> bool {
    let ast = cx.ast();
    let id = skip_wrappers(ast, id);
    if let Some(init) = const_initializer(cx, id)
        && !seen.contains(&init)
        && seen.len() < MAX_DEPTH
    {
        seen.push(init);
        return is_global_document(cx, init, seen);
    }
    if ident_name(ast, id) == Some("document") {
        return cx.is_global_ref(id, "document");
    }
    let obj = match *ast.node(id) {
        Node::Member {
            obj,
            optional: false,
            ..
        }
        | Node::Index {
            obj,
            optional: false,
            ..
        } => obj,
        _ => return false,
    };
    static_property_name(cx, id).as_deref() == Some("document")
        && ["frames", "globalThis", "parent", "self", "top", "window"]
            .iter()
            .any(|n| cx.is_global_ref(obj, n))
}

/// `<x>.<method>(...)` (a static key, nothing optional) with `min..=max` arguments.
fn static_method_call<'a>(
    cx: &Ctx<'a, '_>,
    id: NodeId,
    method: &str,
    min: usize,
    max: usize,
) -> Option<&'a [NodeId]> {
    let ast = cx.ast();
    let Node::Call {
        callee,
        optional: false,
        args,
        ..
    } = *ast.node(id)
    else {
        return None;
    };
    let callee = skip_parens(ast, callee);
    if !matches!(
        ast.node(callee),
        Node::Member {
            optional: false,
            ..
        } | Node::Index {
            optional: false,
            ..
        }
    ) || static_property_name(cx, callee).as_deref() != Some(method)
    {
        return None;
    }
    let args = ast.list(args);
    (min..=max).contains(&args.len()).then_some(args)
}

fn static_string_from_scope(cx: &Ctx<'_, '_>, id: NodeId) -> Option<String> {
    let id = skip_wrappers(cx.ast(), id);
    static_string_literal(cx.ast(), id).or_else(|| match static_value_no_side_effects(cx, id) {
        Some(Static::Str(s)) => Some(s),
        _ => None,
    })
}

fn is_unknown_or_html_namespace(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let id = skip_wrappers(cx.ast(), id);
    if let Some(s) = static_string_literal(cx.ast(), id) {
        return s == HTML_NAMESPACE;
    }
    match static_value_no_side_effects(cx, id) {
        None => true,
        Some(Static::Str(s)) => s == HTML_NAMESPACE,
        Some(_) => false,
    }
}

/// `document.createElement("template")`-style, through `const` aliases.
fn may_create_template(cx: &Ctx<'_, '_>, id: NodeId, seen: &mut Vec<NodeId>) -> bool {
    let id = skip_wrappers(cx.ast(), id);
    if let Some(init) = const_initializer(cx, id)
        && !seen.contains(&init)
        && seen.len() < MAX_DEPTH
    {
        seen.push(init);
        return may_create_template(cx, init, seen);
    }
    if let Some(args) = static_method_call(cx, id, "createElement", 1, 2) {
        return match static_string_from_scope(cx, args[0]) {
            Some(tag) => tag.eq_ignore_ascii_case("template"),
            None => mutable_member_access(cx, args[0], &mut Vec::new()),
        };
    }
    let Some(args) = static_method_call(cx, id, "createElementNS", 2, 3) else {
        return false;
    };
    static_string_from_scope(cx, args[1]).is_some_and(|t| t.eq_ignore_ascii_case("template"))
        && is_unknown_or_html_namespace(cx, args[0])
}

/// Any optional link anywhere inside `id`.
fn contains_optional(ast: &ts_r_ast::Ast, id: NodeId, depth: usize) -> bool {
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

fn should_skip_receiver(cx: &Ctx<'_, '_>, parent: NodeId, inner_html: bool) -> bool {
    let receiver = if inner_html {
        &INNER_HTML_RECEIVER
    } else {
        &REPLACE_CHILDREN_RECEIVER
    };
    is_value_not_dom_node(cx.ast(), parent)
        || NON_DOM_NODE.is_known_non_target(cx, parent)
        || contains_optional(cx.ast(), parent, 0)
        || receiver.is_known_non_target(cx, parent)
}

/// `parent` as a member object written where `replacement` is.
fn receiver_text(cx: &Ctx<'_, '_>, parent: NodeId, replacement: NodeId) -> String {
    if !matches!(cx.node(parent), Node::Super {})
        && member_object_needs_parens(cx, parent, replacement)
    {
        format!("({})", cx.text(parent))
    } else {
        cx.text(parent).to_owned()
    }
}

fn message(cx: &Ctx<'_, '_>, parent: NodeId) -> String {
    format!(
        "Prefer `{}.replaceChildren()` over manually emptying DOM children.",
        receiver_text(cx, parent, parent)
    )
}

/// `<parent>.replaceChildren();` in place of `stmt`.
fn replace_children_fix(cx: &Ctx<'_, '_>, stmt: NodeId, parent: NodeId) -> Option<Fix> {
    if would_remove_comments(cx, cx.span(stmt), &[cx.span(parent)]) {
        return None;
    }
    let text = receiver_text(cx, parent, stmt);
    let semicolon = if needs_semicolon(cx, cx.span(stmt).start, &text) {
        ";"
    } else {
        ""
    };
    Some(Fix::replace(
        cx.span(stmt),
        format!("{semicolon}{text}.replaceChildren();"),
    ))
}

/// The nearest ancestor that is not a parenthesis.
fn parent_skip_parens(cx: &Ctx<'_, '_>, id: NodeId) -> Option<NodeId> {
    let mut p = cx.parent(id)?;
    for _ in 0..10_000 {
        if !matches!(cx.node(p), Node::Paren { .. }) {
            return Some(p);
        }
        p = cx.parent(p)?;
    }
    None
}

fn check_inner_html(cx: &mut Ctx<'_, '_>, node: NodeId, left: NodeId, right: NodeId) {
    let ast = cx.ast();
    let left = skip_parens(ast, left);
    let (obj, key) = match *ast.node(left) {
        Node::Member { obj, prop, .. } => (obj, prop),
        // A computed key with side effects is not a static name.
        Node::Index { obj, index, .. } if !has_side_effect(ast, index) => (obj, index),
        _ => return,
    };
    if static_property_name(cx, left).as_deref() != Some("innerHTML")
        || static_string_literal(ast, skip_wrappers(ast, right)).as_deref() != Some("")
    {
        return;
    }
    let parent = skip_parens(ast, obj);
    if should_skip_receiver(cx, parent, true)
        || is_global_document(cx, parent, &mut Vec::new())
        || may_create_template(cx, parent, &mut Vec::new())
        || TEMPLATE_ELEMENT.is_target(cx, parent)
    {
        return;
    }
    let fix = parent_skip_parens(cx, node)
        .filter(|&s| matches!(cx.node(s), Node::ExprStmt { .. }))
        .and_then(|stmt| replace_children_fix(cx, stmt, parent));
    let message = message(cx, parent);
    cx.report_node(key, message)
        .id("prefer-dom-node-replace-children")
        .fix_opt(fix);
}

fn check_remove_child_loop(cx: &mut Ctx<'_, '_>, node: NodeId, test: NodeId, body: NodeId) {
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
    if !matches!(ident_name(ast, prop), Some("firstChild" | "lastChild")) {
        return;
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
    let Some((remove_obj, _, &[arg])) = method_call_of(
        ast,
        skip_parens(ast, expr),
        &["removeChild"],
        1,
        Some(1),
        Some(false),
    ) else {
        return;
    };
    if !is_same_reference(cx, obj, remove_obj) || !is_same_reference(cx, child, arg) {
        return;
    }
    let parent = skip_parens(ast, obj);
    if should_skip_receiver(cx, parent, false) {
        return;
    }
    let fix = replace_children_fix(cx, node, parent);
    let message = message(cx, parent);
    cx.report_node(node, message)
        .id("prefer-dom-node-replace-children")
        .fix_opt(fix);
}

/// A literal (not a regex) or a template without substitutions, through TS wrappers.
fn is_safe_argument(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    match *ast.node(skip_wrappers(ast, id)) {
        Node::StringLit { .. }
        | Node::NumberLit { .. }
        | Node::BigIntLit { .. }
        | Node::BoolLit { .. }
        | Node::NullLit {} => true,
        Node::TemplateLit { parts } => ast.list(parts).len() <= 1,
        _ => false,
    }
}

fn check_replace_and_add(cx: &mut Ctx<'_, '_>, node: NodeId) {
    let ast = cx.ast();
    let Some((parent, _, _)) =
        method_call_of(ast, node, &["replaceChildren"], 0, Some(0), Some(false))
    else {
        return;
    };
    let Some(stmt) =
        parent_skip_parens(cx, node).filter(|&s| matches!(cx.node(s), Node::ExprStmt { .. }))
    else {
        return;
    };
    let Some(next) = next_statement(cx, stmt) else {
        return;
    };
    let Node::ExprStmt { expr } = *ast.node(next) else {
        return;
    };
    let Some((add_obj, add_prop, add_args)) = method_call_of(
        ast,
        skip_parens(ast, expr),
        &["append", "prepend"],
        1,
        None,
        Some(false),
    ) else {
        return;
    };
    let parent = skip_parens(ast, parent);
    if !is_same_reference(cx, parent, add_obj) || should_skip_receiver(cx, parent, false) {
        return;
    }
    let message =
        "Prefer one `.replaceChildren(…)` call over emptying and then adding DOM children.";
    let id = "prefer-dom-node-replace-children/replace-and-add";
    if would_remove_comments(cx, cx.span(stmt), &[]) {
        cx.report_node(node, message).id(id);
        return;
    }
    let (stmt_span, next_span) = (cx.span(stmt), cx.span(next));
    let separator = &cx.src()[stmt_span.end as usize..next_span.start as usize];
    let removal = if separator.trim().is_empty() {
        let range = Span::new(stmt_span.start, next_span.start);
        if needs_semicolon(cx, stmt_span.start, cx.text(next)) {
            Fix::replace(range, ";")
        } else {
            Fix::delete(range)
        }
    } else {
        remove_statement(cx, stmt)
    };
    let fix = removal.and(cx.span(add_prop), "replaceChildren");
    let suggest = add_args
        .iter()
        .any(|&a| matches!(cx.node(a), Node::Spread { .. }) || !is_safe_argument(cx, a))
        || has_side_effect_with_getters(ast, parent)
        || DOCUMENT.is_target(cx, parent);
    let report = cx.report_node(node, message).id(id);
    if suggest {
        report.suggest(
            "prefer-dom-node-replace-children/replace-and-add-suggestion",
            "Replace the two calls with one `.replaceChildren(…)` call.",
            fix,
        );
    } else {
        report.fix(fix);
    }
}

impl Rule for PreferDomNodeReplaceChildren {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Assign, NodeKind::Call, NodeKind::While]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        match *cx.node(node) {
            Node::Assign {
                left,
                op: AssignOp::Assign,
                right,
            } => check_inner_html(cx, node, left, right),
            Node::Call { .. } => check_replace_and_add(cx, node),
            Node::While { test, body } => check_remove_child_loop(cx, node, test, body),
            _ => {}
        }
    }
}
