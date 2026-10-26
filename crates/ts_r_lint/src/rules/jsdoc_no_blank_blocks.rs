//! `jsdoc/no-blank-blocks`: no JSDoc block with neither tags nor description.

use super::prelude::*;
use crate::jsdoc_util::{description, each_block, removal_fix};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/no-blank-blocks",
        "Removes empty blocks with nothing but possibly line breaks",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<NoBlankBlocks>,
};

#[derive(Debug)]
struct NoBlankBlocks {
    fixer: bool,
}

impl FromOptions for NoBlankBlocks {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let fixer = opts
            .first()
            .and_then(|o| o.get("enableFixer"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        Ok(NoBlankBlocks { fixer })
    }
}

impl Rule for NoBlankBlocks {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        each_block(cx, |cx, finder, doc| {
            if !doc.block.tags.is_empty() {
                return;
            }
            let d = description(&doc.block);
            if !d.text().trim().is_empty() {
                return;
            }
            let span = doc.line_span(finder, d.last_line as i64);
            let r = cx.report(span, "No empty blocks");
            if self.fixer {
                r.fix(removal_fix(finder.src, doc.comment));
            }
        });
    }
}
