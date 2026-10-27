//! `no-restricted-exports`: configured exported names (by list or pattern) and, per
//! `restrictDefaultExports`, the ways of exporting `default`.

use super::prelude::*;
use crate::regex_exec;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-restricted-exports",
        "Disallow specified names in exports",
        NO_PRESET,
        Category::Restriction,
    ),
    create: create::<NoRestrictedExports>,
};

#[derive(Debug, Default)]
struct NoRestrictedExports {
    names: Vec<String>,
    pattern: Option<String>,
    /// `restrictDefaultExports` given at all.
    default_opts: bool,
    direct: bool,
    named: bool,
    default_from: bool,
    named_from: bool,
    namespace_from: bool,
}

impl FromOptions for NoRestrictedExports {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        let d = o.get("restrictDefaultExports").and_then(Value::as_object);
        let flag = |k: &str| d.is_some_and(|d| bool_field(d, k, false));
        Ok(NoRestrictedExports {
            names: str_list(o, "restrictedNamedExports"),
            pattern: str_field(o, "restrictedNamedExportsPattern").map(String::from),
            default_opts: d.is_some(),
            direct: flag("direct"),
            named: flag("named"),
            default_from: flag("defaultFrom"),
            named_from: flag("namedFrom"),
            namespace_from: flag("namespaceFrom"),
        })
    }
}

fn export_name(ast: &ts_r_ast::Ast, id: NodeId) -> Option<&str> {
    match *ast.node(id) {
        Node::StringLit { value } => Some(ast.atom(value)),
        _ => ident_name(ast, id),
    }
}

/// How a `default` name is exported.
enum Via {
    Declaration,
    ExportAll,
    /// `export { x as default }`, with `from` and the local name.
    Specifier(bool, bool),
}

impl NoRestrictedExports {
    fn check_name(&self, cx: &mut Ctx<'_, '_>, id: NodeId, via: Via) {
        let Some(name) = export_name(cx.ast(), id) else {
            return;
        };
        let by_pattern = name != "default"
            && self
                .pattern
                .as_ref()
                .is_some_and(|p| regex_exec::test(p, "u", name));
        if by_pattern || self.names.iter().any(|n| n == name) {
            let msg = format!("'{name}' is restricted from being used as an exported name.");
            cx.report_node(id, msg).id("restrictedNamed");
            return;
        }
        if name != "default" {
            return;
        }
        let restricted = match via {
            Via::Declaration => false,
            Via::ExportAll => self.namespace_from,
            Via::Specifier(false, _) => self.named,
            Via::Specifier(true, local_default) => {
                self.default_opts
                    && if local_default {
                        self.default_from
                    } else {
                        self.named_from
                    }
            }
        };
        if restricted {
            cx.report_node(id, "Exporting 'default' is restricted.")
                .id("restrictedDefault");
        }
    }
}

impl Rule for NoRestrictedExports {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::ExportAll,
            NodeKind::ExportNamed,
            NodeKind::ExportDefaultExpr,
            NodeKind::FunctionDecl,
            NodeKind::ClassDecl,
            NodeKind::VarDecl,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        match *ast.node(node) {
            Node::ExportAll {
                exported: Some(e), ..
            } => self.check_name(cx, e, Via::ExportAll),
            Node::ExportNamed {
                specifiers, source, ..
            } => {
                for &s in ast.list(specifiers) {
                    let Node::ExportSpecifier {
                        local, exported, ..
                    } = *ast.node(s)
                    else {
                        continue;
                    };
                    let local_default = export_name(ast, local) == Some("default");
                    let via = Via::Specifier(source.is_some(), local_default);
                    self.check_name(cx, exported.unwrap_or(local), via);
                }
            }
            Node::ExportDefaultExpr { .. } => {
                if self.direct {
                    cx.report_node(node, "Exporting 'default' is restricted.")
                        .id("restrictedDefault");
                }
            }
            Node::FunctionDecl {
                modifiers,
                name,
                body,
                ..
            } if modifiers.contains(Modifiers::EXPORT) => {
                if modifiers.contains(Modifiers::DEFAULT) {
                    if self.direct {
                        cx.report_node(node, "Exporting 'default' is restricted.")
                            .id("restrictedDefault");
                    }
                } else if let (Some(name), Some(_)) = (name, body) {
                    self.check_name(cx, name, Via::Declaration);
                }
            }
            Node::ClassDecl {
                modifiers, name, ..
            } if modifiers.contains(Modifiers::EXPORT) => {
                if modifiers.contains(Modifiers::DEFAULT) {
                    if self.direct {
                        cx.report_node(node, "Exporting 'default' is restricted.")
                            .id("restrictedDefault");
                    }
                } else if let Some(name) = name {
                    self.check_name(cx, name, Via::Declaration);
                }
            }
            Node::VarDecl {
                modifiers, decls, ..
            } if modifiers.contains(Modifiers::EXPORT) => {
                let mut ids = Vec::new();
                for &d in ast.list(decls) {
                    if let Node::VarDeclarator { name, .. } = *ast.node(d) {
                        binding_idents(ast, name, &mut ids);
                    }
                }
                for id in ids {
                    self.check_name(cx, id, Via::Declaration);
                }
            }
            _ => {}
        }
    }
}
