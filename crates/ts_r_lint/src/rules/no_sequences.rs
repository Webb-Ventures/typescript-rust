//! `no-sequences`: disallow the comma operator, except where parentheses make it explicit.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-sequences",
        "Disallow comma operators",
        NO_PRESET,
        Category::Restriction,
    ),
    create: create::<NoSequences>,
};

#[derive(Debug)]
struct NoSequences {
    allow_in_parens: bool,
}

impl FromOptions for NoSequences {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(NoSequences {
            allow_in_parens: bool_field(obj_opt(opts, 0), "allowInParentheses", true),
        })
    }
}

impl Rule for NoSequences {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Seq]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Seq { exprs } = *ast.node(node) else {
            return;
        };
        let Some(&first) = ast.list(exprs).first() else {
            return;
        };
        // Count the parentheses around the sequence and find what holds it.
        let mut outer = node;
        let mut parens = 0u32;
        let mut parent = cx.parent(node);
        while let Some(p) = parent
            && matches!(ast.node(p), Node::Paren { .. })
        {
            parens += 1;
            outer = p;
            parent = cx.parent(p);
        }
        let mut needed = 1;
        if let Some(p) = parent {
            match *ast.node(p) {
                Node::For { init, update, .. } if init == Some(outer) || update == Some(outer) => {
                    return;
                }
                // The arrow body's own parentheses are required by the syntax.
                Node::Arrow { body, .. } if body == outer => needed = 2,
                _ => {}
            }
        }
        if self.allow_in_parens && parens >= needed {
            return;
        }
        let comma = skip_trivia_forward(cx.src(), cx.span(first).end as usize) as u32;
        cx.report(
            Span::new(comma, comma + 1),
            "Unexpected use of comma operator.",
        )
        .id("unexpectedCommaExpression");
    }
}
