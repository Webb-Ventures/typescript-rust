//! `jsdoc/check-alignment`: the `*` of every line of a block sits one column right of the
//! opening `/**`.

use super::prelude::*;
use crate::diagnostic::Fix;
use crate::jsdoc_util::each_block;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/check-alignment",
        "Reports invalid alignment of JSDoc block asterisks.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<CheckAlignment>,
};

#[derive(Debug)]
struct CheckAlignment {
    inner_indent: usize,
}

impl FromOptions for CheckAlignment {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let inner_indent = opts
            .first()
            .and_then(|o| o.get("innerIndent"))
            .and_then(Value::as_u64)
            .map_or(1, |n| n.min(1 << 16) as usize);
        Ok(CheckAlignment { inner_indent })
    }
}

/// The text of a line before its first `*`.
fn before_star(line: &str) -> &str {
    line.split('*').next().unwrap_or("")
}

impl Rule for CheckAlignment {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let src = cx.file.src;
        each_block(cx, |cx, finder, doc| {
            let text = doc.comment.text(src);
            let want = doc.indent.encode_utf16().count() + self.inner_indent;
            let bad = text
                .split('\n')
                .enumerate()
                .skip(1)
                .map(|(i, l)| (i, before_star(l)))
                .find(|(_, pre)| pre.trim_start().is_empty() && pre.encode_utf16().count() != want);
            let Some((rel, _)) = bad else {
                return;
            };
            let pad = " ".repeat(self.inner_indent);
            let fixed: Vec<String> = text
                .split('\n')
                .enumerate()
                .map(|(i, l)| {
                    if i == 0 || !before_star(l).trim_start().is_empty() {
                        l.to_owned()
                    } else {
                        format!("{}{pad}{}", doc.indent, l.trim_start())
                    }
                })
                .collect();
            let span = doc.line_span(finder, rel as i64);
            cx.report(span, "Expected JSDoc block to be aligned.")
                .fix(Fix::replace(doc.comment, fixed.join("\n")));
        });
    }
}
