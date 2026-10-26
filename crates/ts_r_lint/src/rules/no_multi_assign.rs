//! `no-multi-assign`: disallow chained assignments such as `a = b = c`.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-multi-assign",
        "Disallow use of chained assignment expressions",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<NoMultiAssign>,
};

#[derive(Debug, Default)]
struct NoMultiAssign {
    ignore_non_declaration: bool,
}

impl FromOptions for NoMultiAssign {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(NoMultiAssign {
            ignore_non_declaration: bool_field(obj_opt(opts, 0), "ignoreNonDeclaration", false),
        })
    }
}

impl Rule for NoMultiAssign {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Assign]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some(p) = parent_skip_parens(ast, cx.file.parents, node) else {
            return;
        };
        let is_value = |v: Option<NodeId>| v.is_some_and(|v| skip_parens(ast, v) == node);
        let chained = match *ast.node(p) {
            Node::VarDeclarator { init, .. } => is_value(init),
            Node::PropertyDef { value, .. } => is_value(value),
            Node::Assign { right, .. } => !self.ignore_non_declaration && is_value(Some(right)),
            _ => false,
        };
        if chained {
            cx.report(cx.span(node), "Unexpected chained assignment.")
                .id("unexpectedChain");
        }
    }
}
