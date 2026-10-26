//! `unicorn/try-complexity`: a `try` block whose complexity is above `max` (default 1),
//! reported on the `try` statement. Complexity is 1 plus each branching construct in the
//! block outside nested functions and instance field initializers (static fields and
//! static blocks count): `if`, `?:`, `&&`/`||`/`??`, their assignment forms, `for`,
//! `for...in`/`of`, `while`, `do...while`, `case` (not `default`), `catch` clauses, default
//! values in patterns, and optional `?.` member accesses and calls. Nested `try` blocks count
//! for themselves and for every enclosing one in the same function.

use super::prelude::*;
use ts_r_ast::{AssignOp, Modifiers};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/try-complexity",
        "Limit the complexity of `try` blocks.",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<TryComplexity>,
};

#[derive(Debug)]
struct TryComplexity {
    max: u64,
}

impl FromOptions for TryComplexity {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let max = match obj_opt(opts, 0).get("max") {
            None => 1,
            Some(v) => v
                .as_u64()
                .filter(|&n| n >= 1)
                .ok_or_else(|| "`max` must be an integer of at least 1".to_owned())?,
        };
        Ok(TryComplexity { max })
    }
}

fn adds_complexity(node: &Node) -> bool {
    match *node {
        Node::Assign { op, .. } => matches!(
            op,
            AssignOp::OrAssign | AssignOp::AndAssign | AssignOp::CoalesceAssign
        ),
        Node::Binary { op, .. } => matches!(
            op,
            BinaryOp::LogicalOr | BinaryOp::LogicalAnd | BinaryOp::Coalesce
        ),
        Node::Call { optional, .. }
        | Node::Member { optional, .. }
        | Node::Index { optional, .. } => optional,
        Node::SwitchCase { test, .. } => test.is_some(),
        Node::BindingProperty { init, .. } | Node::ShorthandProp { init, .. } => init.is_some(),
        Node::AssignPattern { .. }
        | Node::CatchClause { .. }
        | Node::Conditional { .. }
        | Node::DoWhile { .. }
        | Node::ForIn { .. }
        | Node::ForOf { .. }
        | Node::For { .. }
        | Node::If { .. }
        | Node::While { .. } => true,
        _ => false,
    }
}

impl Rule for TryComplexity {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Try]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Try { block, .. } = *cx.node(node) else {
            return;
        };
        let mut complexity: u64 = 1;
        let mut stack = vec![block];
        // Each node is visited once: the walk ends.
        while let Some(id) = stack.pop() {
            let n = ast.node(id);
            match *n {
                Node::FunctionDecl { .. } | Node::FunctionExpr { .. } | Node::Arrow { .. } => {
                    continue;
                }
                Node::PropertyDef {
                    modifiers, value, ..
                } if !modifiers.contains(Modifiers::STATIC) => {
                    ast.for_each_child(id, |c| {
                        if Some(c) != value {
                            stack.push(c);
                        }
                    });
                    continue;
                }
                _ => {}
            }
            if adds_complexity(n) {
                complexity += 1;
            }
            ast.for_each_child(id, |c| stack.push(c));
        }
        if complexity > self.max {
            let msg = format!(
                "Try block has a complexity of {complexity}. Maximum allowed is {}.",
                self.max
            );
            cx.report_node(node, msg).id("try-complexity");
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::config::{ConfigFile, ConfigKind, ResolveOptions, objects_from_eslint_json};

    fn lint(src: &str) -> usize {
        let reg = crate::Registry::global();
        let json = serde_json::json!([{ "files": ["**/*.js"], "rules": {
            "unicorn/try-complexity": "error" } }]);
        let objs = objects_from_eslint_json(&json, reg).unwrap();
        let file = ConfigFile::new(ConfigKind::Eslint, None, "/p".into(), objs);
        let fc = file.file_config("a.js", &ResolveOptions::new()).unwrap();
        crate::lint_source(
            "/p/a.js",
            src,
            &fc,
            &crate::NoTypeInfo,
            crate::types::FileKey(0),
        )
        .diagnostics
        .len()
    }

    #[test]
    fn nested_tries_and_malformed_input() {
        let open = "try { if (a) {} ".repeat(200);
        let close = "} catch {} ".repeat(200);
        assert_eq!(lint(&format!("{open}{close}")), 200);
        for src in ["try {", "try { if (", "try {} catch", "try { a ??= "] {
            let _ = lint(src);
        }
    }
}
