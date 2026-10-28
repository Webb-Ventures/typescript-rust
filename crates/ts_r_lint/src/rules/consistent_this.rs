//! `consistent-this`: the names that may hold `this` (`that` by default) hold nothing
//! else, and no other name holds it.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "consistent-this",
        "Enforce consistent naming when capturing the current execution context",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<ConsistentThis>,
};

#[derive(Debug, Default)]
struct ConsistentThis {
    aliases: Vec<String>,
}

impl FromOptions for ConsistentThis {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let mut aliases: Vec<String> = opts
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect();
        if opts.is_empty() {
            aliases.push("that".to_owned());
        }
        Ok(ConsistentThis { aliases })
    }
}

impl ConsistentThis {
    fn check(&self, cx: &mut Ctx<'_, '_>, node: NodeId, name: &str, value: NodeId, plain: bool) {
        let is_this = matches!(cx.node(skip_parens(cx.ast(), value)), Node::This {});
        if self.aliases.iter().any(|a| a == name) {
            if !is_this || !plain {
                cx.report_node(
                    node,
                    format!("Designated alias '{name}' is not assigned to 'this'."),
                )
                .id("aliasNotAssignedToThis");
            }
        } else if is_this {
            cx.report_node(node, format!("Unexpected alias '{name}' for 'this'."))
                .id("unexpectedAlias");
        }
    }
}

impl Rule for ConsistentThis {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::VarDeclarator, NodeKind::Assign]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        match *cx.node(node) {
            Node::VarDeclarator {
                name,
                init: Some(init),
                ..
            } => {
                if let Some(n) = ident_name(ast, name) {
                    self.check(cx, node, n, init, true);
                }
            }
            Node::Assign { op, left, right } => {
                if let Some(n) = ident_name(ast, left) {
                    self.check(cx, node, n, right, op == AssignOp::Assign);
                }
            }
            _ => {}
        }
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let sem = cx.semantic();
        let mut hits = Vec::new();
        for sym in sem.symbols() {
            let name = ast.atom(sym.name);
            if !self.aliases.iter().any(|a| a == name) {
                continue;
            }
            // Only the scopes of the program and of `function`s.
            let scope = sem.scope(sym.scope);
            let checked = match scope.kind {
                ScopeKind::Global | ScopeKind::Module => true,
                ScopeKind::Function => matches!(
                    ast.node(scope.node),
                    Node::FunctionDecl { .. } | Node::FunctionExpr { .. }
                ),
                _ => false,
            };
            if !checked
                || sym
                    .decls
                    .iter()
                    .any(|d| matches!(ast.node(d.node), Node::VarDeclarator { init: Some(_), .. }))
            {
                continue;
            }
            let assigned = sym.refs.iter().any(|&r| {
                let r = sem.reference(r);
                r.scope == sym.scope
                    && r.is_write()
                    && r.write_value.is_some_and(|v| {
                        matches!(ast.node(skip_parens(ast, v)), Node::This {})
                            && cx.parent(r.node).is_some_and(|p| {
                                matches!(
                                    ast.node(p),
                                    Node::Assign {
                                        op: AssignOp::Assign,
                                        ..
                                    }
                                )
                            })
                    })
            });
            if !assigned {
                hits.extend(sym.decls.iter().map(|d| (d.node, name.to_owned())));
            }
        }
        hits.sort_by_key(|(n, _)| cx.span(*n).start);
        for (n, name) in hits {
            cx.report_node(
                n,
                format!("Designated alias '{name}' is not assigned to 'this'."),
            )
            .id("aliasNotAssignedToThis");
        }
    }
}
