//! `no-return-assign`: disallow assignment operators in `return` statements and in the
//! expression bodies of arrow functions.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-return-assign",
        "Disallow assignment operators in `return` statements",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<NoReturnAssign>,
};

#[derive(Debug)]
struct NoReturnAssign {
    /// `"always"`: parentheses do not excuse the assignment.
    always: bool,
}

impl FromOptions for NoReturnAssign {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(NoReturnAssign {
            always: opts.first().and_then(Value::as_str) == Some("always"),
        })
    }
}

impl Rule for NoReturnAssign {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Assign]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let parents = cx.file.parents;
        if !self.always
            && cx
                .parent(node)
                .is_some_and(|p| matches!(cx.node(p), Node::Paren { .. }))
        {
            return;
        }
        // Climb to the nearest statement or function boundary.
        let mut child = node;
        let mut cur = cx.parent(node);
        while let Some(p) = cur {
            let n = ast.node(p);
            if n.is_statement()
                || matches!(
                    n,
                    Node::Arrow { .. } | Node::FunctionExpr { .. } | Node::ClassExpr { .. }
                )
            {
                break;
            }
            child = p;
            cur = parents.parent(p);
        }
        let Some(p) = cur else { return };
        match *ast.node(p) {
            Node::Return { .. } => {
                cx.report(
                    cx.span(p),
                    "Return statement should not contain assignment.",
                )
                .id("returnAssignment");
            }
            Node::Arrow { flags, body, .. }
                if flags.contains(FnFlags::EXPRESSION_BODY) && body == child =>
            {
                cx.report(cx.span(p), "Arrow function should not return assignment.")
                    .id("arrowAssignment");
            }
            _ => {}
        }
    }
}
