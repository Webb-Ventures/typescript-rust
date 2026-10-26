//! `no-script-url`: disallow `javascript:` URLs.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-script-url",
        "Disallow `javascript:` URLs",
        NO_PRESET,
        Category::Restriction,
    ),
    create: create_default::<NoScriptUrl>,
};

#[derive(Debug, Default)]
struct NoScriptUrl;

impl Rule for NoScriptUrl {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::StringLit, NodeKind::TemplateLit]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let value = match *ast.node(node) {
            Node::StringLit { value } => ast.atom(value),
            Node::TemplateLit { parts } => {
                if cx
                    .parent(node)
                    .is_some_and(|p| matches!(ast.node(p), Node::TaggedTemplate { .. }))
                {
                    return;
                }
                match ast.list(parts) {
                    [only] => match *ast.node(*only) {
                        Node::TemplateElement { cooked, .. } => ast.atom(cooked),
                        _ => return,
                    },
                    _ => return,
                }
            }
            _ => return,
        };
        if value
            .get(..11)
            .is_some_and(|p| p.eq_ignore_ascii_case("javascript:"))
        {
            cx.report(cx.span(node), "Script URL is a form of eval.")
                .id("unexpectedScriptURL");
        }
    }
}
