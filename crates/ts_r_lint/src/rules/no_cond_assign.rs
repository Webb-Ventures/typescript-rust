//! `no-cond-assign`: disallow assignment operators in conditional expressions.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-cond-assign",
        "Disallow assignment operators in conditional expressions",
        ESLINT_REC,
        Category::Correctness,
    ),
    create: create::<NoCondAssign>,
};

#[derive(Debug, Default)]
struct NoCondAssign {
    always: bool,
}

impl FromOptions for NoCondAssign {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(NoCondAssign {
            always: opts.first().and_then(Value::as_str) == Some("always"),
        })
    }
}

fn test_of(node: &Node) -> Option<NodeId> {
    match *node {
        Node::If { test, .. } | Node::While { test, .. } | Node::DoWhile { test, .. } => Some(test),
        Node::For { test, .. } => test,
        Node::Conditional { test, .. } => Some(test),
        _ => None,
    }
}

impl Rule for NoCondAssign {
    fn kinds(&self) -> &'static [NodeKind] {
        if self.always {
            &[NodeKind::Assign]
        } else {
            &[
                NodeKind::If,
                NodeKind::While,
                NodeKind::DoWhile,
                NodeKind::For,
                NodeKind::Conditional,
            ]
        }
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        if self.always {
            // Find the nearest conditional whose test contains this assignment.
            let mut cur = node;
            while let Some(p) = cx.parent(cur) {
                let pk = ast.kind(p);
                if is_function(pk) {
                    return;
                }
                if let Some(t) = test_of(ast.node(p))
                    && t == cur
                {
                    let msg = match pk {
                        NodeKind::If => "Unexpected assignment within an 'if' statement.",
                        NodeKind::While => "Unexpected assignment within a 'while' statement.",
                        NodeKind::DoWhile => {
                            "Unexpected assignment within a 'do...while' statement."
                        }
                        NodeKind::For => "Unexpected assignment within a 'for' statement.",
                        _ => "Unexpected assignment within a 'ConditionalExpression'.",
                    };
                    let msg = if pk == NodeKind::Conditional {
                        "Unexpected assignment within ConditionalExpression."
                    } else {
                        msg
                    };
                    cx.report_node(node, msg).id("unexpected");
                    return;
                }
                if ast.node(p).is_statement() {
                    return;
                }
                cur = p;
            }
            return;
        }
        let Some(test) = test_of(cx.node(node)) else {
            return;
        };
        // Ternary tests are never excused by parentheses.
        let test = if ast.kind(node) == NodeKind::Conditional {
            skip_parens(ast, test)
        } else {
            test
        };
        if matches!(cx.node(test), Node::Assign { .. }) {
            cx.report_node(
                test,
                "Expected a conditional expression and instead saw an assignment.",
            )
            .id("missing");
        }
    }
}
