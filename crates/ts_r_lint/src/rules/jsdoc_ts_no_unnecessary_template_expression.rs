//! `jsdoc/ts-no-unnecessary-template-expression`: template literal types without needless
//! `${'string'}` parts or a lone `${T}`.
//!
//! In `typescript` mode only, every tag that may carry a type (but `@import`) and has one is
//! parsed. Each template literal type reports once: for its first interpolation that is a
//! string literal (fixed by merging the string's value and the literals around it), or,
//! when it is just `${T}`, as a lone expression (fixed by putting `T` in its place). Fixes
//! apply to the parsed type as the walk reports, so later reports include earlier ones, and
//! the type is written back like the plugin's `rewireByParsedType`.

use super::jsdoc_ts_no_empty_object_type::might_have_type;
use super::prelude::*;
use crate::jsdoc_type::{self, Mode, N};
use crate::jsdoc_util::{each_block, rewire_type, structured_tags};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/ts-no-unnecessary-template-expression",
        "Catches unnecessary template expressions such as string expressions within a template literal.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<TsNoUnnecessaryTemplateExpression>,
};

#[derive(Debug)]
struct TsNoUnnecessaryTemplateExpression {
    fixer: bool,
}

impl FromOptions for TsNoUnnecessaryTemplateExpression {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(TsNoUnnecessaryTemplateExpression {
            fixer: bool_field(obj_opt(opts, 0), "enableFixer", true),
        })
    }
}

impl Rule for TsNoUnnecessaryTemplateExpression {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let structured = structured_tags(cx);
        each_block(cx, |cx, finder, doc| {
            if finder.settings.mode != "typescript" {
                return;
            }
            for ti in 0..doc.block.tags.len() {
                let tag = &doc.block.tags[ti];
                if tag.tag == "import"
                    || tag.typ.is_empty()
                    || !might_have_type(structured, &tag.tag)
                {
                    continue;
                }
                let Some(mut tree) = jsdoc_type::parse(&tag.typ, Mode::Typescript, false) else {
                    continue;
                };
                let span = doc.line_span(finder, doc.block.tag_line(ti));
                for (id, _, _) in tree.walk() {
                    let N::TemplateLiteral { literals, interps } = tree.nodes[id].clone() else {
                        continue;
                    };
                    let string_at = interps
                        .iter()
                        .position(|&i| matches!(tree.nodes[i], N::Str { .. }));
                    let message = if let Some(at) = string_at {
                        if self.fixer {
                            let N::Str { value, .. } = &tree.nodes[interps[at]] else {
                                continue;
                            };
                            let merged = format!(
                                "{}{value}{}",
                                literals.get(at).map_or("", String::as_str),
                                literals.get(at + 1).map_or("", String::as_str)
                            );
                            let mut literals = literals.clone();
                            let end = (at + 2).min(literals.len());
                            literals.splice(at..end, [merged]);
                            let mut interps = interps.clone();
                            interps.remove(at);
                            tree.nodes[id] = N::TemplateLiteral { literals, interps };
                        }
                        "Found an unnecessary string literal within a template."
                    } else if literals.len() == 2
                        && literals.iter().all(String::is_empty)
                        && let Some(&first) = interps.first()
                    {
                        if self.fixer {
                            tree.nodes[id] = tree.nodes[first].clone();
                        }
                        "Found a lone template expression within a template."
                    } else {
                        continue;
                    };
                    let fix = self.fixer.then(|| {
                        rewire_type(&mut doc.block, ti, &tree.stringify(), &doc.indent, "");
                        doc.fix()
                    });
                    let r = cx.report(span, message);
                    if let Some(f) = fix {
                        r.fix(f);
                    }
                }
            }
        });
    }
}
