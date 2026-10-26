//! `unicorn/prefer-dom-node-remove`: `child.remove()` over `parent.removeChild(child)`.
//!
//! A call `parent.removeChild(child)` (one non-spread argument, not an optional call)
//! where neither side is a value that can't be a DOM node is reported as a whole.
//!
//! When the parent has no possible side effect and the call's value is unused, it is fixed:
//! `x.parentNode.removeChild(x)` (same reference) keeps `x` and replaces the rest with
//! `.remove()` (`?.remove()` after `x?.parentNode`; no fix when the callee or `parentNode`
//! is parenthesized), and any other non-optional form becomes `child.remove()`. Otherwise
//! suggestions are offered: `?.remove()` and `.remove()` when the parent's member chain has
//! an optional link, else `.remove()`. A comment inside the call blocks both.

use super::prelude::*;
use super::unicorn_util::{
    has_possible_side_effect, is_parenthesized, is_same_reference, is_value_not_dom_node,
    is_value_not_usable, member_object_text, method_call_of, needs_semicolon,
    would_remove_comments,
};
use super::unicorn_util::{parenthesized_span, space_around_keyword, with_edits};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-dom-node-remove",
        "Prefer `childNode.remove()` over `parentNode.removeChild(childNode)`.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create_default::<PreferDomNodeRemove>,
};

#[derive(Debug, Default)]
struct PreferDomNodeRemove;

/// An optional link on the callee or along the parent's member-object chain.
fn optional_parent(ast: &ts_r_ast::Ast, callee: NodeId) -> bool {
    let mut node = callee;
    for _ in 0..10_000 {
        match *ast.node(node) {
            Node::Member { obj, optional, .. } | Node::Index { obj, optional, .. } => {
                if optional {
                    return true;
                }
                node = obj;
            }
            _ => return false,
        }
    }
    false
}

impl Rule for PreferDomNodeRemove {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some((parent, _, &[child])) =
            method_call_of(ast, node, &["removeChild"], 1, Some(1), None)
        else {
            return;
        };
        if is_value_not_dom_node(ast, parent) || is_value_not_dom_node(ast, child) {
            return;
        }
        let Node::Call { callee, .. } = *cx.node(node) else {
            return;
        };
        let member = skip_parens(ast, callee);
        let parent_inner = skip_parens(ast, parent);
        let child = skip_parens(ast, child);
        let has_comment = would_remove_comments(cx, cx.span(node), &[]);
        let is_optional = optional_parent(ast, member);
        let same_ref = match *ast.node(parent_inner) {
            Node::Member {
                obj,
                prop,
                optional,
            } if ident_name(ast, prop) == Some("parentNode")
                && is_same_reference(cx, obj, child) =>
            {
                Some((obj, optional))
            }
            _ => None,
        };
        let same_ref_fix = |optional: bool| -> Option<Fix> {
            let (obj, _) = same_ref?;
            if is_parenthesized(cx, member) || is_parenthesized(cx, parent_inner) {
                return None;
            }
            let start = parenthesized_span(cx, skip_parens(ast, obj)).end;
            Some(Fix::replace(
                Span::new(start, cx.span(node).end),
                if optional { "?.remove()" } else { ".remove()" },
            ))
        };
        let plain_fix = |optional: bool| -> Fix {
            let mut text = member_object_text(cx, child, node);
            if needs_semicolon(cx, cx.span(node).start, &text) {
                text = format!(";{text}");
            }
            text.push_str(if optional { "?.remove()" } else { ".remove()" });
            with_edits(
                Fix::replace(cx.span(node), text),
                space_around_keyword(cx, node),
            )
        };
        let make = |optional: bool| -> Option<Fix> {
            if same_ref.is_some() {
                same_ref_fix(optional)
            } else {
                Some(plain_fix(optional))
            }
        };
        let message = "Prefer `childNode.remove()` over `parentNode.removeChild(childNode)`.";
        if !has_possible_side_effect(cx, parent) && is_value_not_usable(cx, node) {
            if let Some((_, optional)) = same_ref {
                let fix = if has_comment {
                    None
                } else {
                    same_ref_fix(optional)
                };
                cx.report_node(node, message).id("error").fix_opt(fix);
                return;
            }
            if !is_optional {
                let fix = (!has_comment).then(|| plain_fix(false));
                cx.report_node(node, message).id("error").fix_opt(fix);
                return;
            }
        }
        let suggestions: Vec<(bool, Fix)> = if has_comment {
            Vec::new()
        } else {
            let options: &[bool] = if is_optional {
                &[true, false]
            } else {
                &[false]
            };
            options
                .iter()
                .filter_map(|&o| make(o).map(|f| (o, f)))
                .collect()
        };
        let mut r = cx.report_node(node, message).id("error");
        for (optional, fix) in suggestions {
            r = r.suggest(
                "suggestion",
                format!(
                    "Replace `parentNode.removeChild(childNode)` with `childNode{}remove()`.",
                    if optional { "?." } else { "." }
                ),
                fix,
            );
        }
    }
}
