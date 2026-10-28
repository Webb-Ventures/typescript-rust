//! `unicorn/explicit-timer-delay`: `setTimeout`/`setInterval` (the globals, or a plain member
//! of the global `window`/`globalThis`/`global`/`self`), not optionally called. Mode
//! `always` (default): a call with one argument (not a lone spread) gets `, 0` after it.
//! Mode `never`: with exactly two arguments, a zero delay (`0` under any `+`/`-`) is
//! removed with its comma, unless a comment sits there.

use super::prelude::*;
use super::unicorn_text::paren_span;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/explicit-timer-delay",
        "Enforce or disallow explicit `delay` argument for `setTimeout()` and `setInterval()`.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<ExplicitTimerDelay>,
};

#[derive(Debug, Default)]
struct ExplicitTimerDelay {
    never: bool,
}

impl FromOptions for ExplicitTimerDelay {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(ExplicitTimerDelay {
            never: opts.first().and_then(Value::as_str) == Some("never"),
        })
    }
}

fn is_zero(ast: &Ast, id: NodeId, depth: u32) -> bool {
    if depth > 1000 {
        return false;
    }
    match *ast.node(skip_parens(ast, id)) {
        Node::NumberLit { value } => value.get() == 0.0,
        Node::Unary {
            op: UnaryOp::Plus | UnaryOp::Minus,
            arg,
        } => is_zero(ast, arg, depth + 1),
        _ => false,
    }
}

impl Rule for ExplicitTimerDelay {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Call {
            callee,
            optional: false,
            args,
            ..
        } = *cx.node(node)
        else {
            return;
        };
        let callee = skip_parens(ast, callee);
        let name = match *ast.node(callee) {
            Node::Ident { .. } => {
                let Some(n @ ("setTimeout" | "setInterval")) = ident_name(ast, callee) else {
                    return;
                };
                if !cx.is_global_ref(callee, n) {
                    return;
                }
                n
            }
            Node::Member { obj, prop, .. } => {
                let Some(n @ ("setTimeout" | "setInterval")) = ident_name(ast, prop) else {
                    return;
                };
                let obj = skip_parens(ast, obj);
                match ident_name(ast, obj) {
                    Some(o @ ("window" | "globalThis" | "global" | "self"))
                        if matches!(ast.node(obj), Node::Ident { .. })
                            && cx.is_global_ref(obj, o) => {}
                    _ => return,
                }
                n
            }
            _ => return,
        };
        let args = ast.list(args);
        if !self.never {
            let [first] = *args else {
                return;
            };
            if matches!(ast.node(first), Node::Spread { .. }) {
                return;
            }
            let end = paren_span(cx, first).end;
            cx.report(
                cx.span(node),
                format!("`{name}` should have an explicit delay argument."),
            )
            .id("missing-delay")
            .fix(Fix::insert(end, ", 0"));
            return;
        }
        let [first, delay] = *args else {
            return;
        };
        if !is_zero(ast, delay, 0) {
            return;
        }
        let range = Span::new(paren_span(cx, first).end, paren_span(cx, delay).end);
        let fix = ast
            .comments_in(range)
            .is_empty()
            .then(|| Fix::delete(range));
        cx.report(
            cx.span(skip_parens(ast, delay)),
            format!("`{name}` should not have an explicit delay of `0`."),
        )
        .id("redundant-delay")
        .fix_opt(fix);
    }
}
