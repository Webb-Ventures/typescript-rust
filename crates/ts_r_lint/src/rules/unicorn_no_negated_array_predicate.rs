//! `unicorn/no-negated-array-predicate`: `!x.some(cb)` / `!x.every(cb)` (through
//! parentheses and TS wrappers on the call; one or two arguments, no spread, no optional
//! chaining, no type arguments) where `cb` is an arrow or function expression (not async,
//! a generator, or with a return type or type parameters) that only returns an
//! expression, and the receiver isn't known to be a non-array. Reported at the method name.
//! A `yield !` whose `!` ends its line is left alone (removing it would end the `yield`).
//!
//! The fix drops the `!`, swaps the method and negates the returned expression: `!e`
//! becomes `e` (wrapped when a concise arrow body would start with `{` or is a sequence),
//! anything else `!e` (wrapped by unary precedence; comment-holding parentheses kept).
//! The argument is wrapped when it would now start a statement or arrow body with `{`; a
//! `return`/`throw` whose `!` ends the line gets its argument parenthesized; otherwise a
//! glued keyword gets a space and a `;` is added when the code would continue the previous
//! line. No fix with a comment right after the `!` or inside the returned expression.

use super::prelude::*;
use super::unicorn_ast::{method_call, no_spread_in, paren_return_or_throw};
use super::unicorn_no_typeof_undefined::keyword_argument_parent;
use super::unicorn_text::{
    expression_text, is_parenthesized, needs_parens_at_expression_start, needs_semicolon,
    paren_span,
};
use super::unicorn_types::is_known_non_indexed_collection;
use super::unicorn_util::space_around_keyword;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-negated-array-predicate",
        "Disallow negated array predicate calls.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<NoNegatedArrayPredicate>,
};

#[derive(Debug, Default)]
struct NoNegatedArrayPredicate;

fn skip_ts(ast: &Ast, mut id: NodeId) -> NodeId {
    for _ in 0..1000 {
        match *ast.node(id) {
            Node::Paren { expr }
            | Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::NonNull { expr }
            | Node::TypeAssertion { expr, .. } => id = expr,
            _ => break,
        }
    }
    id
}

/// The returned expression of a callback that only returns one (parentheses kept).
fn returned_expression(ast: &Ast, f: NodeId) -> Option<NodeId> {
    let body = match *ast.node(f) {
        Node::Arrow {
            flags,
            type_params: None,
            return_type: None,
            body,
            ..
        } if !flags.contains(FnFlags::ASYNC) => {
            if flags.contains(FnFlags::EXPRESSION_BODY) {
                return Some(body);
            }
            body
        }
        Node::FunctionExpr {
            flags,
            type_params: None,
            return_type: None,
            body: Some(body),
            ..
        } if !flags.contains(FnFlags::ASYNC) && !flags.contains(FnFlags::GENERATOR) => body,
        _ => return None,
    };
    let Node::Block { stmts } = *ast.node(body) else {
        return None;
    };
    match *ast.list(stmts) {
        [s] => match *ast.node(s) {
            Node::Return { arg } => arg,
            _ => None,
        },
        _ => None,
    }
}

fn is_bang(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(id),
        Node::Unary {
            op: UnaryOp::Not,
            ..
        }
    )
}

fn has_line_break(text: &str) -> bool {
    text.contains(['\n', '\r', '\u{2028}', '\u{2029}'])
}

impl Rule for NoNegatedArrayPredicate {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Unary]
    }
    fn enter(&self, unary: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Unary {
            op: UnaryOp::Not,
            arg,
        } = *ast.node(unary)
        else {
            return;
        };
        let call_id = skip_ts(ast, arg);
        let Some(call) = method_call(ast, call_id) else {
            return;
        };
        let replacement = match call.name {
            "some" => "every",
            "every" => "some",
            _ => return,
        };
        if !(1..=2).contains(&call.args.len())
            || !no_spread_in(ast, call.args, 2)
            || !call.plain()
            || matches!(
                *ast.node(call_id),
                Node::Call {
                    type_args: Some(_),
                    ..
                }
            )
        {
            return;
        }
        let callback = skip_parens(ast, call.args[0]);
        let Some(returned) = returned_expression(ast, callback) else {
            return;
        };
        if is_known_non_indexed_collection(cx, call.obj) {
            return;
        }
        let src = cx.src();
        let span = cx.span(unary);
        let bang = span.start;
        let after_bang = skip_trivia_forward(src, bang as usize + 1) as u32;
        let bang_gap = &src[bang as usize + 1..after_bang as usize];
        let keyword_parent = keyword_argument_parent(cx, unary);
        if keyword_parent.is_some_and(|k| matches!(ast.node(k), Node::Yield { .. }))
            && has_line_break(bang_gap)
        {
            return;
        }
        let arg_inner = skip_parens(ast, arg);
        let needs_parens = !is_parenthesized(cx, arg_inner)
            && needs_parens_at_expression_start(cx, arg_inner, unary);
        let returned_inner = skip_parens(ast, returned);
        let (range, text) = if is_bang(ast, returned_inner) {
            let Node::Unary { arg: negated, .. } = *ast.node(returned_inner) else {
                return;
            };
            let text = cx.text(skip_parens(ast, negated));
            let concise = matches!(*ast.node(callback), Node::Arrow { flags, body, .. }
                if flags.contains(FnFlags::EXPRESSION_BODY) && skip_parens(ast, body) == returned_inner);
            let wrap = concise
                && (matches!(ast.node(skip_parens(ast, negated)), Node::Seq { .. })
                    || text.trim_start().starts_with('{'));
            (
                cx.span(returned_inner),
                if wrap {
                    format!("({text})")
                } else {
                    text.to_owned()
                },
            )
        } else {
            let wrap = precedence(ast, returned_inner) < 16
                || matches!(
                    ast.node(returned_inner),
                    Node::Update { .. } | Node::NonNull { .. } | Node::TypeAssertion { .. }
                );
            (
                paren_span(cx, returned_inner),
                format!("!{}", expression_text(cx, returned_inner, wrap)),
            )
        };
        let returned_span = cx.span(returned_inner);
        let comment_inside = ast.comments_in(returned_span).iter().any(|c| {
            c.span.start >= returned_span.start
                && c.span.end <= returned_span.end
                && c.span.start >= range.start
                && c.span.end <= range.end
        });
        let fix = (!comment_inside && ast.comments_in(Span::new(bang + 1, after_bang)).is_empty())
            .then(|| {
                let return_parens = keyword_parent.is_some_and(|k| {
                    matches!(ast.node(k), Node::Return { .. } | Node::Throw { .. })
                }) && has_line_break(bang_gap);
                let mut fix = Fix::replace(Span::new(bang, bang + 1), "")
                    .and(cx.span(call.prop), replacement)
                    .and(range, text);
                if needs_parens {
                    let a = cx.span(arg_inner);
                    fix = fix.and(Span::at(a.start), "(").and(Span::at(a.end), ")");
                }
                if return_parens {
                    if let Some(k) = keyword_parent {
                        for (s, t) in paren_return_or_throw(cx, k) {
                            fix = fix.and(s, t);
                        }
                    }
                    return fix;
                }
                for (s, t) in space_around_keyword(cx, unary) {
                    fix = fix.and(s, t);
                }
                let code = if needs_parens {
                    "("
                } else {
                    &src[after_bang as usize..]
                };
                if needs_semicolon(cx, bang, code) {
                    fix = fix.and(Span::at(bang), ";");
                }
                fix
            });
        let r = cx
            .report(
                cx.span(call.prop),
                format!(
                    "Prefer `Array#{replacement}()` with a negated predicate over negating `Array#{}()`.",
                    call.name
                ),
            )
            .id("no-negated-array-predicate");
        if let Some(f) = fix {
            r.fix(f);
        }
    }
}
