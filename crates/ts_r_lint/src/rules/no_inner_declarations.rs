//! `no-inner-declarations`: function (and with `both`, `var`) declarations only at the root
//! of the program, a function body or a static block. Block-scoped functions in strict
//! ES2015+ code are allowed unless `blockScopedFunctions` is `disallow`.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-inner-declarations",
        "Disallow variable or `function` declarations in nested blocks",
        NO_PRESET,
        Category::Suspicious,
    ),
    create: create::<NoInnerDeclarations>,
};

#[derive(Debug, Default)]
struct NoInnerDeclarations {
    both: bool,
    allow_block_functions: bool,
}

impl FromOptions for NoInnerDeclarations {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(NoInnerDeclarations {
            both: opts.first().and_then(Value::as_str) == Some("both"),
            allow_block_functions: str_field(obj_opt(opts, 1), "blockScopedFunctions")
                != Some("disallow"),
        })
    }
}

fn is_function(n: &Node) -> bool {
    matches!(
        n,
        Node::FunctionDecl { .. } | Node::FunctionExpr { .. } | Node::Arrow { .. }
    )
}

impl NoInnerDeclarations {
    fn check(&self, cx: &mut Ctx<'_, '_>, node: NodeId, function: bool) {
        let ast = cx.ast();
        let Some(p) = cx.parent(node) else {
            return;
        };
        match ast.node(p) {
            Node::Program { .. } | Node::StaticBlock { .. } => return,
            Node::Block { .. } if cx.parent(p).is_some_and(|g| is_function(ast.node(g))) => {
                return;
            }
            _ => {}
        }
        let body = ancestors_inclusive(cx.file.parents, p)
            .find_map(|a| match ast.node(a) {
                Node::StaticBlock { .. } => Some("class static block body"),
                n if is_function(n) => Some("function body"),
                _ => None,
            })
            .unwrap_or("program");
        let kind = if function { "function" } else { "variable" };
        cx.report_node(node, format!("Move {kind} declaration to {body} root."))
            .id("moveDeclToRoot");
    }
}

impl Rule for NoInnerDeclarations {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::FunctionDecl, NodeKind::VarDecl]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        match *cx.node(node) {
            Node::FunctionDecl { body: Some(_), .. } => {
                if self.allow_block_functions
                    && crate::globals::normalize_ecma_version(cx.env().ecma_version) >= 2015
                    && cx
                        .parent(node)
                        .is_some_and(|p| super::this_binding::is_strict(cx, p))
                {
                    return;
                }
                self.check(cx, node, true);
            }
            Node::VarDecl {
                kind: VarKind::Var, ..
            } if self.both => self.check(cx, node, false),
            _ => {}
        }
    }
}
