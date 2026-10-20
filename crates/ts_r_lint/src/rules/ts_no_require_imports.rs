//! `@typescript-eslint/no-require-imports`: disallow `require()` imports.

use super::prelude::*;
use crate::regex;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-require-imports",
        "Disallow invocation of `require()`",
        TS_REC,
        Category::Style,
    ),
    create: create::<NoRequireImports>,
};

#[derive(Debug)]
struct NoRequireImports {
    allow: Vec<String>,
    allow_as_import: bool,
}

impl FromOptions for NoRequireImports {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(NoRequireImports {
            allow: str_list(o, "allow"),
            allow_as_import: bool_field(o, "allowAsImport", false),
        })
    }
}

/// A string literal or a template literal without substitutions.
pub(crate) fn plain_string<'a>(cx: &Ctx<'_, 'a>, id: NodeId) -> Option<&'a str> {
    let ast = cx.ast();
    match *ast.node(id) {
        Node::StringLit { value } => Some(ast.atom(value)),
        Node::TemplateLit { parts } => match ast.list(parts) {
            [p] => match *ast.node(*p) {
                Node::TemplateElement { cooked, .. } => Some(ast.atom(cooked)),
                _ => None,
            },
            _ => None,
        },
        _ => None,
    }
}

impl NoRequireImports {
    fn allowed(&self, cx: &Ctx<'_, '_>, arg: Option<NodeId>) -> bool {
        let Some(path) = arg.and_then(|a| plain_string(cx, a)) else {
            return false;
        };
        self.allow
            .iter()
            .any(|p| regex::compile(p).is_some_and(|r| r.is_match(path, false)))
    }
}

impl Rule for NoRequireImports {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call, NodeKind::ImportEquals]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let (report_on, arg) = match *cx.node(node) {
            Node::Call { callee, args, .. } => {
                if !cx.is_global_ref(callee, "require") && !is_unresolved(cx, callee) {
                    return;
                }
                (node, ast.list(args).first().copied())
            }
            Node::ImportEquals { module_ref, .. } => {
                let Node::ExternalModuleRef { expr } = *cx.node(module_ref) else {
                    return;
                };
                if self.allow_as_import {
                    return;
                }
                (module_ref, Some(expr))
            }
            _ => return,
        };
        if self.allowed(cx, arg) {
            return;
        }
        cx.report_node(report_on, "A `require()` style import is forbidden.")
            .id("noRequireImports");
    }
}

/// `require` with no binding in scope (not a configured global either).
fn is_unresolved(cx: &Ctx<'_, '_>, ident: NodeId) -> bool {
    if ident_name(cx.ast(), ident) != Some("require") {
        return false;
    }
    let sem = cx.semantic();
    sem.reference_at(ident)
        .is_some_and(|r| !matches!(sem.reference(r).resolution, Resolution::Symbol(_)))
}
