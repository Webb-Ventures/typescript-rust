//! `unicorn/prefer-type-literal-last`: in a union, an object type literal (through
//! parentheses) is followed by a member that is neither a type literal, `null` /
//! `undefined`, nor a union or intersection. The first such literal is reported.
//!
//! The fix reorders the members to the others, then the literals, then `null` / `undefined`
//! (each keeping its parentheses, joined with ` | `), unless a comment between the token
//! before the union and the token after it lies outside every member.

use super::prelude::*;
use super::unicorn_ast::would_remove_comments;
use ts_r_ast::Ast;
use ts_r_span::Span;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-type-literal-last",
        "Require type literals to be last in union types.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<PreferTypeLiteralLast>,
};

#[derive(Debug, Default)]
struct PreferTypeLiteralLast;

fn unwrap_parens(ast: &Ast, mut id: NodeId) -> NodeId {
    while let Node::ParenType { ty } = *ast.node(id) {
        id = ty;
    }
    id
}

fn is_literal(ast: &Ast, id: NodeId) -> bool {
    matches!(ast.node(unwrap_parens(ast, id)), Node::TypeLiteral { .. })
}

fn is_nullish(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(unwrap_parens(ast, id)),
        Node::KeywordType {
            kind: KeywordTypeKind::Undefined | KeywordTypeKind::Null
        }
    )
}

fn is_composite(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(unwrap_parens(ast, id)),
        Node::UnionType { .. } | Node::IntersectionType { .. }
    )
}

impl Rule for PreferTypeLiteralLast {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::UnionType]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::UnionType { types } = *cx.node(node) else {
            return;
        };
        let types = ast.list(types);
        let mut literal = None;
        let mut problem = None;
        for &t in types {
            if is_literal(ast, t) {
                literal = literal.or(Some(t));
            } else if !is_nullish(ast, t) && !is_composite(ast, t) && literal.is_some() {
                problem = literal;
                break;
            }
        }
        let Some(problem) = problem else {
            return;
        };

        let src = cx.src();
        let span = cx.span(node);
        let region = Span::new(
            skip_trivia_backward(ast, src, span.start as usize) as u32,
            skip_trivia_forward(src, span.end as usize) as u32,
        );
        let kept: Vec<Span> = types.iter().map(|&t| cx.span(t)).collect();
        let fix = (!would_remove_comments(ast, region, &kept)).then(|| {
            let others = types
                .iter()
                .filter(|&&t| !is_literal(ast, t) && !is_nullish(ast, t));
            let literals = types.iter().filter(|&&t| is_literal(ast, t));
            let nullish = types.iter().filter(|&&t| is_nullish(ast, t));
            let text: Vec<&str> = others
                .chain(literals)
                .chain(nullish)
                .map(|&t| cx.text(t))
                .collect();
            Fix::replace(span, text.join(" | "))
        });
        cx.report_node(
            unwrap_parens(ast, problem),
            "Move type literals to the end of this union.",
        )
        .id("prefer-type-literal-last")
        .fix_opt(fix);
    }
}
