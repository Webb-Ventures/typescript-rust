//! `unicorn/no-useless-collection-argument`: `new Set/Map/WeakSet/WeakMap(x)` (one argument,
//! not a spread) where `x`, or the right side of `x` when it is `a ?? x`, is an empty array,
//! an empty string (literal or substitution-free template), `null` or `undefined`. That
//! value is reported. The fix removes the argument, or the `?? x` (from the end of the left
//! side, with its parentheses) together with the parentheses around the `??` unless its left
//! side is an unparenthesized sequence. When a comment sits in the argument's removal range
//! (the argument with its parentheses and a trailing comma) the fix is a suggestion.

use super::prelude::*;
use super::unicorn_ast::{remove_argument, would_remove_comments};
use super::unicorn_util::parenthesized_span;
use super::unicorn_util_h1::static_string;
use ts_r_ast::Ast;
use ts_r_span::Span;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-useless-collection-argument",
        "Disallow useless values or fallbacks in `Set`, `Map`, `WeakSet`, or `WeakMap`.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create_default::<NoUselessCollectionArgument>,
};

#[derive(Debug, Default)]
struct NoUselessCollectionArgument;

fn description(ast: &Ast, id: NodeId) -> Option<&'static str> {
    match *ast.node(id) {
        Node::ArrayLit { elements } if ast.list(elements).is_empty() => Some("empty array"),
        Node::NullLit {} => Some("`null`"),
        Node::Ident { .. } if ident_name(ast, id) == Some("undefined") => Some("`undefined`"),
        _ => static_string(ast, id)
            .is_some_and(|s| s.is_empty())
            .then_some("empty string"),
    }
}

impl Rule for NoUselessCollectionArgument {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::New]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::New {
            callee,
            has_args: true,
            args,
            ..
        } = *cx.node(node)
        else {
            return;
        };
        if !matches!(
            ident_name(ast, callee),
            Some("Set" | "Map" | "WeakSet" | "WeakMap")
        ) {
            return;
        }
        let &[arg] = ast.list(args) else {
            return;
        };
        if matches!(ast.node(arg), Node::Spread { .. }) {
            return;
        }
        let iterable = skip_parens(ast, arg);
        let fallback = match *ast.node(iterable) {
            Node::Binary {
                left,
                op: BinaryOp::Coalesce,
                right,
            } => Some((left, right)),
            _ => None,
        };
        let target = skip_parens(ast, fallback.map_or(iterable, |(_, r)| r));
        let Some(desc) = description(ast, target) else {
            return;
        };
        let fix = match fallback {
            None => remove_argument(cx, node, 0),
            Some((left, _)) => {
                let left_inner = skip_parens(ast, left);
                let left_parens = left != left_inner;
                let start = cx.span(left).end;
                let mut fix = Fix::replace(Span::new(start, cx.span(iterable).end), "");
                if left_parens || !matches!(ast.node(left_inner), Node::Seq { .. }) {
                    let mut cur = iterable;
                    while let Some(p) = cx.parent(cur)
                        && matches!(ast.node(p), Node::Paren { .. })
                    {
                        let s = cx.span(p);
                        fix = fix
                            .and(Span::new(s.start, s.start + 1), "")
                            .and(Span::new(s.end - 1, s.end), "");
                        cur = p;
                    }
                }
                Some(fix)
            }
        };
        // The argument's removal range: with its parentheses and a trailing comma.
        let src = cx.src();
        let mut removal = parenthesized_span(cx, iterable);
        let after = skip_trivia_forward(src, removal.end as usize);
        if src.as_bytes().get(after) == Some(&b',') {
            removal = Span::new(removal.start, after as u32 + 1);
        }
        let suggest = would_remove_comments(ast, removal, &[]);
        let report = cx
            .report_node(target, format!("The {desc} is useless."))
            .id("no-useless-collection-argument/error");
        match (fix, suggest) {
            (Some(f), true) => {
                report.suggest(
                    "no-useless-collection-argument/suggestion",
                    format!("Remove the {desc}"),
                    f,
                );
            }
            (Some(f), false) => {
                report.fix(f);
            }
            (None, _) => {}
        }
    }
}
