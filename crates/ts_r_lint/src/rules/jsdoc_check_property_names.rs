//! `jsdoc/check-property-names`: no duplicate `@property` names in a block, and every
//! nested property path (`a.b`) follows the property it extends.

use super::prelude::*;
use crate::jsdoc_util::{each_block, remove_tag};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/check-property-names",
        "Ensures that property names in JSDoc are not duplicated on the same block and that nested properties have defined roots.",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<CheckPropertyNames>,
};

#[derive(Debug)]
struct CheckPropertyNames {
    fixer: bool,
}

impl FromOptions for CheckPropertyNames {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let fixer = opts
            .first()
            .and_then(|o| o.get("enableFixer"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        Ok(CheckPropertyNames { fixer })
    }
}

impl Rule for CheckPropertyNames {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        each_block(cx, |cx, finder, doc| {
            let Some(name) = doc.preferred(cx, finder, "property") else {
                return;
            };
            let tags = &doc.block.tags;
            let props: Vec<usize> = (0..tags.len()).filter(|&i| tags[i].tag == name).collect();
            if props.is_empty() {
                return;
            }
            // With several typedefs, each one's properties are checked for duplicates apart.
            let typedefs: Vec<usize> = (0..tags.len())
                .filter(|&i| tags[i].tag == "typedef")
                .collect();
            let groups: Vec<(usize, usize)> = if typedefs.len() > 1 {
                (0..typedefs.len())
                    .map(|k| {
                        (
                            typedefs[k],
                            typedefs.get(k + 1).copied().unwrap_or(tags.len()),
                        )
                    })
                    .collect()
            } else {
                vec![(0, tags.len())]
            };
            for (from, to) in groups {
                let group: Vec<usize> = props
                    .iter()
                    .copied()
                    .filter(|&i| i >= from && i < to)
                    .collect();
                for (k, &i) in group.iter().enumerate() {
                    let dupe = group
                        .iter()
                        .enumerate()
                        .find(|&(j, &other)| j != k && tags[other].name == tags[i].name);
                    if let Some((_, &other)) = dupe {
                        let span = doc.line_span(finder, doc.block.tag_line(other));
                        let msg = format!("Duplicate @{name} \"{}\"", tags[i].name);
                        if self.fixer {
                            remove_tag(&mut doc.block, other, 0);
                            let fix = doc.fix();
                            cx.report(span, msg).fix(fix);
                        } else {
                            cx.report(span, msg);
                        }
                        return;
                    }
                }
            }
            let mut last_real: Option<&str> = None;
            for &i in &props {
                let n = tags[i].name.as_str();
                let Some(dot) = n.find('.') else {
                    last_real = Some(n);
                    continue;
                };
                let span = doc.line_span(finder, doc.block.tag_line(i));
                let Some(real) = last_real else {
                    cx.report(
                        span,
                        format!(
                            "@{name} path declaration (\"{n}\") appears before any real property."
                        ),
                    );
                    return;
                };
                let root = &n[..dot];
                let root = root.strip_suffix("[]").unwrap_or(root);
                if root != real {
                    cx.report(
                        span,
                        format!(
                            "@{name} path declaration (\"{n}\") root node name (\"{root}\") does not match previous real property name (\"{real}\")."
                        ),
                    );
                    return;
                }
            }
        });
    }
}
