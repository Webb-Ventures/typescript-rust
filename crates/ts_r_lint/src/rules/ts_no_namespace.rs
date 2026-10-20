//! `@typescript-eslint/no-namespace`: disallow TypeScript namespaces.

use super::prelude::*;
use super::ts_prefer_namespace_keyword::span_without_export;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-namespace",
        "Disallow TypeScript namespaces",
        TS_REC,
        Category::Style,
    ),
    create: create::<NoNamespace>,
};

#[derive(Debug)]
struct NoNamespace {
    allow_declarations: bool,
    allow_definition_files: bool,
}

impl FromOptions for NoNamespace {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(NoNamespace {
            allow_declarations: bool_field(o, "allowDeclarations", false),
            allow_definition_files: bool_field(o, "allowDefinitionFiles", true),
        })
    }
}

impl Rule for NoNamespace {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Module]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Node::Module { kind, name, .. } = *cx.node(node) else {
            return;
        };
        if kind == ModuleKind::Global || matches!(cx.node(name), Node::StringLit { .. }) {
            return;
        }
        if self.allow_definition_files && cx.file.path.ends_with(".d.ts") {
            return;
        }
        if self.allow_declarations
            && ancestors_inclusive(cx.file.parents, node).any(|a| match *cx.node(a) {
                Node::Module {
                    modifiers, kind, ..
                } => modifiers.contains(Modifiers::DECLARE) || kind == ModuleKind::Global,
                _ => false,
            })
        {
            return;
        }
        let span = span_without_export(cx, node);
        cx.report(span, "ES2015 module syntax is preferred over namespaces.")
            .id("moduleSyntaxIsPreferred");
    }
}
