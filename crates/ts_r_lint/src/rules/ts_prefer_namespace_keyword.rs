//! `@typescript-eslint/prefer-namespace-keyword`: require `namespace` over `module` for
//! custom TypeScript modules.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/prefer-namespace-keyword",
        "Require using `namespace` keyword over `module` keyword to declare custom TypeScript modules",
        TS_REC,
        Category::Style,
    )
    .fixable(),
    create: create_default::<PreferNamespaceKeyword>,
};

#[derive(Debug, Default)]
struct PreferNamespaceKeyword;

/// The span of a declaration without a leading `export` keyword (ESTree wraps it).
pub(crate) fn span_without_export(cx: &Ctx<'_, '_>, node: NodeId) -> Span {
    let span = cx.span(node);
    let text = cx.text(node);
    if let Some(rest) = text.strip_prefix("export")
        && rest.starts_with(|c: char| !is_ident_char(c))
    {
        let start = skip_trivia_forward(cx.src(), span.start as usize + 6) as u32;
        return Span::new(start, span.end);
    }
    span
}

impl Rule for PreferNamespaceKeyword {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Module]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Node::Module {
            kind: ModuleKind::Module,
            name,
            ..
        } = *cx.node(node)
        else {
            return;
        };
        if matches!(cx.node(name), Node::StringLit { .. }) {
            return;
        }
        let span = span_without_export(cx, node);
        let head = &cx.src()[span.start as usize..cx.span(name).start as usize];
        let Some(off) = head.find("module") else {
            return;
        };
        let kw = span.start + off as u32;
        cx.report(
            span,
            "Use 'namespace' instead of 'module' to declare custom TypeScript modules.",
        )
        .id("useNamespace")
        .fix(Fix::replace(Span::new(kw, kw + 6), "namespace"));
    }
}
