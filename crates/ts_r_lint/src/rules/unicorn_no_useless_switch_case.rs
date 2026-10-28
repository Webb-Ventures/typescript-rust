//! `unicorn/no-useless-switch-case`: empty `case` clauses that fall through to a final
//! `default` (scanning back from it until a clause has statements). In TypeScript files
//! `case undefined`/`case void x`/`case null` stay (they narrow types). The suggestion removes
//! the clause, unless its test has a side effect or a comment is inside it.

use super::prelude::*;
use super::unicorn_util::{has_side_effect_with, is_typescript_path, switch_case_head};
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-useless-switch-case",
        "Disallow useless case in switch statements.",
        NO_PRESET,
        Category::Pedantic,
    )
    .suggestions(),
    create: create_default::<NoUselessSwitchCase>,
};

#[derive(Debug, Default)]
struct NoUselessSwitchCase;

/// An empty statement, or a block of only empty statements.
pub(crate) fn is_empty_stmt(ast: &Ast, id: NodeId) -> bool {
    match *ast.node(id) {
        Node::Empty {} => true,
        Node::Block { stmts } => ast.list(stmts).iter().all(|&s| is_empty_stmt(ast, s)),
        _ => false,
    }
}

impl Rule for NoUselessSwitchCase {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Switch]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Switch { cases, .. } = *cx.node(node) else {
            return;
        };
        let cases = ast.list(cases);
        if cases.len() < 2
            || !matches!(
                ast.node(cases[cases.len() - 1]),
                Node::SwitchCase { test: None, .. }
            )
        {
            return;
        }
        let ts = is_typescript_path(cx.file.path);
        for &case in cases[..cases.len() - 1].iter().rev() {
            let Node::SwitchCase { test, body } = *ast.node(case) else {
                return;
            };
            if !ast.list(body).iter().all(|&s| is_empty_stmt(ast, s)) {
                break;
            }
            let test = test.map(|t| skip_parens(ast, t));
            if ts
                && test.is_some_and(|t| {
                    is_undefined(ast, t)
                        || matches!(
                            ast.node(t),
                            Node::NullLit {}
                                | Node::Unary {
                                    op: UnaryOp::Void,
                                    ..
                                }
                        )
                })
            {
                continue;
            }
            let span = cx.span(case);
            let suggest = !test.is_some_and(|t| has_side_effect_with(ast, t, false, false))
                && ast.comments_in(span).is_empty();
            let head = switch_case_head(cx, case);
            let r = cx
                .report(head, "Useless case in switch statement.")
                .id("no-useless-switch-case/error");
            if suggest {
                r.suggest(
                    "no-useless-switch-case/suggestion",
                    "Remove this case.",
                    Fix::delete(span),
                );
            }
        }
    }
}
