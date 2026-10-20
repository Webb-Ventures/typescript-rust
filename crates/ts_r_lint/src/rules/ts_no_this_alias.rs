//! `@typescript-eslint/no-this-alias`: disallow aliasing `this`.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-this-alias",
        "Disallow aliasing `this`",
        TS_REC,
        Category::Correctness,
    ),
    create: create::<NoThisAlias>,
};

#[derive(Debug)]
struct NoThisAlias {
    allow_destructuring: bool,
    allowed: Vec<String>,
}

impl FromOptions for NoThisAlias {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(NoThisAlias {
            allow_destructuring: bool_field(o, "allowDestructuring", true),
            allowed: str_list(o, "allowedNames"),
        })
    }
}

impl Rule for NoThisAlias {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::VarDeclarator, NodeKind::Assign]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let (target, value, span) = match *cx.node(node) {
            Node::VarDeclarator {
                name,
                type_ann,
                init: Some(init),
                ..
            } => {
                let s = cx.span(name);
                let end = type_ann.map_or(s.end, |t| cx.span(t).end);
                (name, init, Span::new(s.start, end))
            }
            Node::Assign {
                left,
                op: AssignOp::Assign,
                right,
            } => (left, right, cx.span(left)),
            _ => return,
        };
        if !matches!(cx.node(skip_parens(ast, value)), Node::This {}) {
            return;
        }
        match *cx.node(target) {
            Node::Ident { .. } => {
                if self.allowed.iter().any(|n| n == cx.text(target)) {
                    return;
                }
                cx.report(span, "Unexpected aliasing of 'this' to local variable.")
                    .id("thisAssignment");
            }
            Node::ObjectPattern { .. }
            | Node::ArrayPattern { .. }
            | Node::ObjectLit { .. }
            | Node::ArrayLit { .. } => {
                if self.allow_destructuring {
                    return;
                }
                cx.report(
                    span,
                    "Unexpected aliasing of members of 'this' to local variables.",
                )
                .id("thisDestructure");
            }
            _ => {}
        }
    }
}
