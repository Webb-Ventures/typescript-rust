//! `unicorn/max-nested-calls`: a call or `new` that is an argument (directly, or through
//! parentheses and spreads) of more than `max` (default 3) enclosing calls, counting
//! itself. Functions, classes, static blocks and JSX elements reset the count.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/max-nested-calls",
        "Limit the depth of nested calls.",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<MaxNestedCalls>,
};

#[derive(Debug)]
struct MaxNestedCalls {
    max: u64,
}

impl FromOptions for MaxNestedCalls {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let max = obj_opt(opts, 0)
            .get("max")
            .and_then(Value::as_u64)
            .unwrap_or(3);
        Ok(MaxNestedCalls { max })
    }
}

impl Rule for MaxNestedCalls {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call, NodeKind::New]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let mut depth = 1u64;
        let mut child = node;
        for _ in 0..1_000_000 {
            let Some(p) = cx.parent(child) else {
                break;
            };
            match *ast.node(p) {
                Node::FunctionDecl { .. }
                | Node::FunctionExpr { .. }
                | Node::Arrow { .. }
                | Node::ClassDecl { .. }
                | Node::ClassExpr { .. }
                | Node::JsxElement { .. }
                | Node::JsxFragment { .. }
                | Node::StaticBlock { .. } => break,
                Node::Call { args, .. } | Node::New { args, .. }
                    if ast.list(args).contains(&child) =>
                {
                    depth += 1;
                }
                _ => {}
            }
            child = p;
        }
        if depth > self.max {
            let max = self.max;
            cx.report(
                cx.span(node),
                format!("Call is nested too deeply. Maximum allowed is {max}."),
            )
            .id("max-nested-calls");
        }
    }
}
