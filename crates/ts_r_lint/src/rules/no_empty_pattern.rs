//! `no-empty-pattern`: disallow empty destructuring patterns.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-empty-pattern",
        "Disallow empty destructuring patterns",
        ESLINT_REC,
        Category::Correctness,
    ),
    create: create::<NoEmptyPattern>,
};

#[derive(Debug, Default)]
struct NoEmptyPattern {
    allow_object_params: bool,
}

impl FromOptions for NoEmptyPattern {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(NoEmptyPattern {
            allow_object_params: bool_field(
                obj_opt(opts, 0),
                "allowObjectPatternsAsParameters",
                false,
            ),
        })
    }
}

impl Rule for NoEmptyPattern {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::ObjectPattern,
            NodeKind::ArrayPattern,
            NodeKind::ObjectLit,
            NodeKind::ArrayLit,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let (empty, object, literal) = match *cx.node(node) {
            Node::ObjectPattern { props } => {
                (props.is_empty() || ast.list(props).is_empty(), true, false)
            }
            Node::ArrayPattern { elements } => (ast.list(elements).is_empty(), false, false),
            Node::ObjectLit { props } => (ast.list(props).is_empty(), true, true),
            Node::ArrayLit { elements } => (ast.list(elements).is_empty(), false, true),
            _ => return,
        };
        if !empty {
            return;
        }
        if literal && !is_assignment_target(ast, cx.file.parents, node) {
            return;
        }
        if object && self.allow_object_params && is_param_pattern(cx, node) {
            return;
        }
        let what = if object { "object" } else { "array" };
        cx.report_node(node, format!("Unexpected empty {what} pattern."))
            .id("unexpected");
    }
}

/// `function f({}) {}` or `function f({} = {}) {}`.
fn is_param_pattern(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let Some(p) = cx.parent(node) else {
        return false;
    };
    match *cx.node(p) {
        Node::Param { pattern, init, .. } => {
            pattern == node
                && init.is_none_or(|i| {
                    matches!(cx.node(skip_parens(cx.ast(), i)), Node::ObjectLit { props } if cx.ast().list(*props).is_empty())
                })
        }
        _ => false,
    }
}
