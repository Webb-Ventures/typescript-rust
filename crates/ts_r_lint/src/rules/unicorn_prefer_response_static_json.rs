//! `unicorn/prefer-response-static-json`: `new Response(JSON.stringify(data), ...)` (the
//! global-looking `Response` name, `JSON.stringify` a plain call with one argument) is
//! reported at `JSON.stringify`. The fix turns it into `Response.json(data, ...)`: adds
//! `.json`, drops `new` (see [`new_to_call`]) and the `JSON.stringify(`…`)` around the data.
//! It is withheld when a comment sits in the removed parts, or when the data is
//! `undefined`, `void …`, a function or a class (which `Response.json` throws on).

use super::prelude::*;
use super::unicorn_ast::method_call;
use super::unicorn_static::skip_ts_only;
use super::unicorn_util::{new_to_call, parenthesized_span};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-response-static-json",
        "Prefer `Response.json()` over `new Response(JSON.stringify())`.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<PreferResponseStaticJson>,
};

#[derive(Debug, Default)]
struct PreferResponseStaticJson;

impl Rule for PreferResponseStaticJson {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::New]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::New { callee, args, .. } = *cx.node(node) else {
            return;
        };
        let callee = skip_parens(ast, callee);
        if ident_name(ast, callee) != Some("Response") {
            return;
        }
        let Some(&first) = ast.list(args).first() else {
            return;
        };
        if ast
            .list(args)
            .iter()
            .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
        {
            return;
        }
        let stringify = skip_parens(ast, first);
        let Some(call) = method_call(ast, stringify) else {
            return;
        };
        if call.name != "stringify"
            || !call.plain()
            || ident_name(ast, skip_parens(ast, call.obj)) != Some("JSON")
            || !matches!(ast.node(skip_parens(ast, call.obj)), Node::Ident { .. })
        {
            return;
        }
        let [data] = *call.args else {
            return;
        };
        if matches!(ast.node(data), Node::Spread { .. }) {
            return;
        }
        let Node::Call { callee: member, .. } = *ast.node(stringify) else {
            return;
        };
        let call_range = parenthesized_span(cx, stringify);
        let data_range = cx.span(data);
        let before = Span::new(call_range.start, data_range.start);
        let after = Span::new(data_range.end, call_range.end);
        let commented = !ast.comments_in(before).is_empty() || !ast.comments_in(after).is_empty();
        let value = skip_ts_only(ast, data);
        let non_json = ident_name(ast, value) == Some("undefined")
            || matches!(
                ast.node(value),
                Node::Unary {
                    op: UnaryOp::Void,
                    ..
                } | Node::FunctionExpr { .. }
                    | Node::Arrow { .. }
                    | Node::ClassExpr { .. }
            );
        let fix = (!commented && !non_json).then(|| {
            let mut fix = new_to_call(cx, node).and(Span::at(cx.span(callee).end), ".json");
            fix = fix.and(before, "").and(after, "");
            fix
        });
        cx.report(
            cx.span(skip_parens(ast, member)),
            "Prefer using `Response.json(…)` over `JSON.stringify()`.",
        )
        .id("prefer-response-static-json")
        .fix_opt(fix);
    }
}
