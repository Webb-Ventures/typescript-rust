//! `no-multi-str`: disallow string literals continued over a line break with `\`.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-multi-str",
        "Disallow multiline strings",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<NoMultiStr>,
};

#[derive(Debug, Default)]
struct NoMultiStr;

impl Rule for NoMultiStr {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::StringLit, NodeKind::Directive]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let mut span = cx.span(node);
        let mut raw = span.text(cx.src());
        // A directive's span includes its `;`.
        if let Some(t) = raw.strip_suffix(';') {
            raw = t.trim_end();
            span = Span::new(span.start, span.start + raw.len() as u32);
        }
        if !raw.contains(['\n', '\r', '\u{2028}', '\u{2029}']) {
            return;
        }
        if cx
            .parent(node)
            .is_some_and(|p| cx.node(p).kind().name().starts_with("Jsx"))
        {
            return;
        }
        cx.report(
            span,
            "Multiline support is limited to browsers supporting ES5 only.",
        )
        .id("multilineString");
    }
}
