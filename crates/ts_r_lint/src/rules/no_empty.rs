//! `no-empty`: disallow empty block statements.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-empty",
        "Disallow empty block statements",
        ESLINT_REC,
        Category::Suspicious,
    )
    .suggestions(),
    create: create::<NoEmpty>,
};

#[derive(Debug, Default)]
struct NoEmpty {
    allow_empty_catch: bool,
}

impl FromOptions for NoEmpty {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(NoEmpty {
            allow_empty_catch: bool_field(obj_opt(opts, 0), "allowEmptyCatch", false),
        })
    }
}

impl Rule for NoEmpty {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Block, NodeKind::Switch]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        match *cx.node(node) {
            Node::Block { stmts } => {
                if !ast.list(stmts).is_empty() {
                    return;
                }
                if let Some(p) = cx.parent(node) {
                    match *cx.node(p) {
                        Node::FunctionDecl { .. }
                        | Node::FunctionExpr { .. }
                        | Node::Arrow { .. } => return,
                        Node::CatchClause { .. } if self.allow_empty_catch => return,
                        _ => {}
                    }
                }
                let span = cx.span(node);
                if !ast.comments_in(span).is_empty() {
                    return;
                }
                cx.report(span, "Empty block statement.")
                    .id("unexpected")
                    .suggest(
                        "suggestComment",
                        "Add comment inside empty block statement.",
                        Fix::replace(span, "{ /* empty */ }"),
                    );
            }
            Node::Switch { disc, cases } => {
                if !ast.list(cases).is_empty() {
                    return;
                }
                let span = cx.span(node);
                let Some(close_paren) = find_char(cx.src(), cx.span(disc).end, b')') else {
                    return;
                };
                let Some(open) = find_char(cx.src(), close_paren + 1, b'{') else {
                    return;
                };
                let braces = Span::new(open, span.end);
                if !ast.comments_in(braces).is_empty() {
                    return;
                }
                cx.report(braces, "Empty switch statement.")
                    .id("unexpected")
                    .suggest(
                        "suggestComment",
                        "Add comment inside empty switch statement.",
                        Fix::replace(braces, "{ /* empty */ }"),
                    );
            }
            _ => {}
        }
    }
}
