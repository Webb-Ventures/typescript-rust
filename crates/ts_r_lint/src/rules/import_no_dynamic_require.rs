//! `import/no-dynamic-require`: `require(x)` (and, with `esmodule`, `import(x)`) whose
//! first argument is not a literal or an expression-free template.

use super::prelude::*;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "import/no-dynamic-require",
        "Forbid `require()` calls with expressions.",
        NO_PRESET,
        Category::Restriction,
    ),
    create: create::<NoDynamicRequire>,
};

#[derive(Debug)]
struct NoDynamicRequire {
    esmodule: bool,
}

impl FromOptions for NoDynamicRequire {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(NoDynamicRequire {
            esmodule: bool_field(obj_opt(opts, 0), "esmodule", false),
        })
    }
}

fn is_static(ast: &Ast, id: NodeId) -> bool {
    match ast.node(skip_parens(ast, id)) {
        Node::StringLit { .. }
        | Node::NumberLit { .. }
        | Node::BigIntLit { .. }
        | Node::BoolLit { .. }
        | Node::NullLit {}
        | Node::RegexLit { .. } => true,
        Node::TemplateLit { parts } => ast.list(*parts).len() == 1,
        _ => false,
    }
}

impl Rule for NoDynamicRequire {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call, NodeKind::ImportCall]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        match *cx.node(node) {
            Node::Call { callee, args, .. } => {
                let Some(&first) = ast.list(args).first() else {
                    return;
                };
                if ident_name(ast, skip_parens(ast, callee)) == Some("require")
                    && !is_static(ast, first)
                {
                    cx.report_node(node, "Calls to require() should use string literals");
                }
            }
            Node::ImportCall { arg, .. } => {
                if self.esmodule && !is_static(ast, arg) {
                    cx.report_node(node, "Calls to import() should use string literals");
                }
            }
            _ => {}
        }
    }
}
