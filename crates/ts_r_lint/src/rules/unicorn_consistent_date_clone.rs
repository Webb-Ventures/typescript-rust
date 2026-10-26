//! `unicorn/consistent-date-clone`: `new Date(date.getTime())` clones a date the long way;
//! the `.getTime()` (from the method name to the end of the call) is reported, and the fix
//! removes it unless a comment between the receiver and the end of the call would go.

use super::prelude::*;
use super::unicorn_ast::{method_call, would_remove_comments};
use super::unicorn_text::paren_span;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/consistent-date-clone",
        "Prefer passing `Date` directly to the constructor when cloning.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<ConsistentDateClone>,
};

#[derive(Debug, Default)]
struct ConsistentDateClone;

impl Rule for ConsistentDateClone {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::New]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::New { callee, args, .. } = *cx.node(node) else {
            return;
        };
        let args = ast.list(args);
        if ident_name(ast, skip_parens(ast, callee)) != Some("Date") || args.len() != 1 {
            return;
        }
        let call_id = args[0];
        let Some(call) = method_call(ast, call_id) else {
            return;
        };
        if call.name != "getTime" || !call.args.is_empty() || !call.plain() {
            return;
        }
        let call_span = cx.span(call_id);
        let obj = paren_span(cx, call.obj);
        let Node::Call { callee, .. } = *ast.node(call_id) else {
            return;
        };
        let member = cx.span(skip_parens(ast, callee));
        let member_outer = cx.span(callee);
        let fix = (!would_remove_comments(ast, call_span, &[obj])).then(|| {
            Fix::delete(Span::new(obj.end, member.end))
                .and(Span::new(member_outer.end, call_span.end), "")
        });
        let span = Span::new(cx.span(call.prop).start, call_span.end);
        cx.report(span, "Unnecessary `.getTime()` call.")
            .id("consistent-date-clone/error")
            .fix_opt(fix);
    }
}
