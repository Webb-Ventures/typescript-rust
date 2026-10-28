//! `unicorn/no-leading-empty-lines`: blank lines (spaces and tabs only) at the very start of
//! a file are reported as one range and removed by the fix.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-leading-empty-lines",
        "Disallow empty lines at the beginning of files.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<NoLeadingEmptyLines>,
};

#[derive(Debug, Default)]
struct NoLeadingEmptyLines;

/// The length of the leading run of lines holding only spaces and tabs, with their breaks.
fn leading_empty(src: &str) -> usize {
    let b = src.as_bytes();
    let (mut end, mut i) = (0, 0);
    while i < b.len() {
        match b[i] {
            b' ' | b'\t' => i += 1,
            b'\n' | b'\r' => {
                i += 1;
                end = i;
            }
            _ => break,
        }
    }
    end
}

impl Rule for NoLeadingEmptyLines {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Program]
    }
    fn enter(&self, _node: NodeId, cx: &mut Ctx<'_, '_>) {
        // A leading BOM is stripped before linting (`project::lint_one`), as ESLint does.
        let len = leading_empty(cx.src()) as u32;
        if len == 0 {
            return;
        }
        let span = Span::new(0, len);
        cx.report(span, "Do not use empty lines at the beginning of a file.")
            .id("no-leading-empty-lines")
            .fix(Fix::delete(span));
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn leading() {
        assert_eq!(super::leading_empty("\n \n\tx"), 3);
        assert_eq!(super::leading_empty("  x\n"), 0);
        assert_eq!(super::leading_empty("\r\n\r\n"), 4);
        assert_eq!(super::leading_empty(""), 0);
    }
}
