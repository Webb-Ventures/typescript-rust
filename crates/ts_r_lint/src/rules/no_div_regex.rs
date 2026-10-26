//! `no-div-regex`: disallow a regex literal that starts with `=`, which reads like `/=`.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-div-regex",
        "Disallow equal signs explicitly at the beginning of regular expressions",
        NO_PRESET,
        Category::Restriction,
    )
    .fixable(),
    create: create_default::<NoDivRegex>,
};

#[derive(Debug, Default)]
struct NoDivRegex;

impl Rule for NoDivRegex {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::RegexLit]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let span = cx.span(node);
        if !span.text(cx.src()).starts_with("/=") {
            return;
        }
        let eq = Span::new(span.start + 1, span.start + 2);
        cx.report(
            span,
            "A regular expression literal can be confused with '/='.",
        )
        .id("unexpected")
        .fix(Fix::replace(eq, "[=]"));
    }
}
