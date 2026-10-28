//! `no-template-curly-in-string`: disallow `${...}` in ordinary string literals.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-template-curly-in-string",
        "Disallow template literal placeholder syntax in regular strings",
        NO_PRESET,
        Category::Suspicious,
    ),
    create: create_default::<NoTemplateCurlyInString>,
};

#[derive(Debug, Default)]
struct NoTemplateCurlyInString;

/// `${` followed by at least one character that is not `}` and then a `}`.
fn has_placeholder(s: &str) -> bool {
    let mut rest = s;
    while let Some(i) = rest.find("${") {
        rest = &rest[i + 2..];
        if let Some(j) = rest.find('}')
            && j > 0
        {
            return true;
        }
    }
    false
}

impl Rule for NoTemplateCurlyInString {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::StringLit, NodeKind::Directive]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let (Node::StringLit { value } | Node::Directive { value }) = *ast.node(node) else {
            return;
        };
        if !has_placeholder(ast.atom(value)) {
            return;
        }
        let mut span = cx.span(node);
        // A directive's span includes its `;`.
        if let Some(t) = span.text(cx.src()).strip_suffix(';') {
            span = Span::new(span.start, span.start + t.trim_end().len() as u32);
        }
        cx.report(span, "Unexpected template string expression.")
            .id("unexpectedTemplateExpression");
    }
}
