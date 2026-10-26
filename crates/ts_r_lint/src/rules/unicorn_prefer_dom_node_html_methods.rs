//! `unicorn/prefer-dom-node-html-methods`: reading `x.innerHTML` (not written to; option
//! `checkGetHTML`, on by default) is reported at the property and fixed to `x.getHTML()`,
//! wrapped when it is an unparenthesized `new` callee, unless the member (through TS
//! wrappers) is itself called, constructed or used as a tag. Assigning to it (option
//! `checkSetHTML`, off by default) is reported at the property; a plain `=` whose value is
//! unused gets a suggestion to write `x.setHTML(value)`, spaced from a glued keyword, unless
//! a comment outside the object and the value would be lost.

use super::prelude::*;
use super::unicorn_ast::{is_left_hand_side, is_value_not_usable};
use super::unicorn_text::{member_object_text, paren_span, paren_text, would_remove_comments};
use super::unicorn_util::{in_unparenthesized_new_callee, space_around_keyword, with_edits};
use ts_r_span::Span;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-dom-node-html-methods",
        "Prefer `.getHTML()` and `.setHTML()` over `.innerHTML`.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create::<PreferDomNodeHtmlMethods>,
};

#[derive(Debug)]
struct PreferDomNodeHtmlMethods {
    check_get: bool,
    check_set: bool,
}

impl FromOptions for PreferDomNodeHtmlMethods {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(PreferDomNodeHtmlMethods {
            check_get: bool_field(o, "checkGetHTML", true),
            check_set: bool_field(o, "checkSetHTML", false),
        })
    }
}

/// `x.innerHTML`: `(object, property)`.
fn inner_html(ast: &ts_r_ast::Ast, id: NodeId) -> Option<(NodeId, NodeId)> {
    match *ast.node(id) {
        Node::Member { obj, prop, .. } if ident_name(ast, prop) == Some("innerHTML") => {
            Some((obj, prop))
        }
        _ => None,
    }
}

/// The member (through parentheses and TS wrappers) is a callee, `new` callee or tag.
fn is_callee_or_tag(cx: &Ctx<'_, '_>, member: NodeId) -> bool {
    let mut cur = member;
    while let Some(p) = cx.parent(cur) {
        match *cx.node(p) {
            Node::Paren { .. }
            | Node::As { .. }
            | Node::Satisfies { .. }
            | Node::NonNull { .. }
            | Node::TypeAssertion { .. }
            | Node::Instantiation { .. } => cur = p,
            Node::Call { callee, .. } | Node::New { callee, .. } => return callee == cur,
            Node::TaggedTemplate { tag, .. } => return tag == cur,
            _ => return false,
        }
    }
    false
}

impl Rule for PreferDomNodeHtmlMethods {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Member, NodeKind::Assign]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        match *cx.node(node) {
            Node::Member { .. } => {
                let Some((_, prop)) = inner_html(ast, node) else {
                    return;
                };
                if !self.check_get || is_left_hand_side(cx, node) {
                    return;
                }
                let fix = (!is_callee_or_tag(cx, node)).then(|| {
                    let span = cx.span(node);
                    let fix = Fix::replace(cx.span(prop), "getHTML");
                    if in_unparenthesized_new_callee(cx, node) {
                        fix.and(Span::at(span.end), "())")
                            .and(Span::at(span.start), "(")
                    } else {
                        fix.and(Span::at(span.end), "()")
                    }
                });
                cx.report_node(prop, "Prefer `.getHTML()` over `.innerHTML`.")
                    .id("prefer-dom-node-html-methods/read")
                    .fix_opt(fix);
            }
            Node::Assign { left, op, right } => {
                let Some((obj, prop)) = inner_html(ast, skip_parens(ast, left)) else {
                    return;
                };
                if !self.check_set {
                    return;
                }
                let span = cx.span(node);
                let suggestion = (op == AssignOp::Assign
                    && is_value_not_usable(cx, node)
                    && !would_remove_comments(
                        cx,
                        span,
                        &[paren_span(cx, obj), paren_span(cx, right)],
                    ))
                .then(|| {
                    let text = format!(
                        "{}.setHTML({})",
                        member_object_text(cx, obj, node),
                        paren_text(cx, right)
                    );
                    with_edits(Fix::replace(span, text), space_around_keyword(cx, node))
                });
                let report = cx
                    .report_node(prop, "Prefer `.setHTML()` over assigning to `.innerHTML`.")
                    .id("prefer-dom-node-html-methods/write");
                if let Some(fix) = suggestion {
                    report.suggest(
                        "prefer-dom-node-html-methods/write-suggestion",
                        "Switch to `.setHTML()`.",
                        fix,
                    );
                }
            }
            _ => {}
        }
    }
}
