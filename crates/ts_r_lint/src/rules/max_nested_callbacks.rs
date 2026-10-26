//! `max-nested-callbacks`: limit how deeply callbacks (function arguments) nest.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "max-nested-callbacks",
        "Enforce a maximum depth that callbacks can be nested",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<MaxNestedCallbacks>,
};

#[derive(Debug, Default)]
struct MaxNestedCallbacks {
    max: u64,
    constructors: bool,
}

impl FromOptions for MaxNestedCallbacks {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(MaxNestedCallbacks {
            max: max_option(opts, 10),
            constructors: bool_field(obj_opt(opts, 0), "checkConstructorCallCallbacks", false),
        })
    }
}

impl MaxNestedCallbacks {
    /// Is the function `f` passed as an argument of a call (or of `new`, when enabled)?
    fn is_callback(&self, cx: &Ctx<'_, '_>, f: NodeId) -> bool {
        let ast = cx.ast();
        let Some(p) = parent_skip_parens(ast, cx.file.parents, f) else {
            return false;
        };
        let args = match *ast.node(p) {
            Node::Call { args, .. } => args,
            Node::New { args, .. } if self.constructors => args,
            _ => return false,
        };
        ast.list(args).iter().any(|&a| skip_parens(ast, a) == f)
    }
}

impl Rule for MaxNestedCallbacks {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::FunctionExpr, NodeKind::Arrow]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        if !self.is_callback(cx, node) {
            return;
        }
        let ast = cx.ast();
        let depth = 1 + cx
            .file
            .parents
            .ancestors(node)
            .filter(|&a| {
                matches!(ast.node(a).kind(), NodeKind::FunctionExpr | NodeKind::Arrow)
                    && self.is_callback(cx, a)
            })
            .count() as u64;
        if depth > self.max {
            let span = function_head_span(ast, cx.src(), cx.file.parents, node);
            cx.report(
                span,
                format!(
                    "Too many nested callbacks ({depth}). Maximum allowed is {}.",
                    self.max
                ),
            )
            .id("exceed");
        }
    }
}
