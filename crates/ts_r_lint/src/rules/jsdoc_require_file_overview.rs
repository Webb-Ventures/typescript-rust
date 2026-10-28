//! `jsdoc/require-file-overview`: whole-file tags (by default `@file`) must exist, appear
//! once, and come before any code.
//!
//! Each `tags` entry (default `file` with all three checks on; a given entry's checks default
//! off) is looked up under its preferred name in every block. After the file: a missing tag
//! (`mustExist`), a tag in more than one block (`preventDuplicates`), and a first tag block
//! that comes after some code (`initialCommentsOnly`) each report at 1:2. "Code" is the first
//! node, in traversal order, that has no JSDoc block of its own and starts past offset 0 (the
//! plugin keeps the start in a falsy-tested number, so code at offset 0 doesn't count). A
//! preference blocking the tag reports that instead.

use serde_json::Map;

use super::prelude::*;
use crate::diagnostic::Loc;
use crate::jsdoc::{self, Finder, Preferred, Settings, Target, preferred_tag};
use crate::jsdoc_util::each_block;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/require-file-overview",
        "Checks that all files have one `@file`, `@fileoverview`, or `@overview` tag at the \
         beginning of the file",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<RequireFileOverview>,
};

#[derive(Debug)]
struct TagCheck {
    name: String,
    initial_comments_only: bool,
    must_exist: bool,
    prevent_duplicates: bool,
}

#[derive(Debug)]
struct RequireFileOverview {
    tags: Vec<TagCheck>,
}

impl Default for RequireFileOverview {
    fn default() -> Self {
        RequireFileOverview {
            tags: vec![TagCheck {
                name: "file".into(),
                initial_comments_only: true,
                must_exist: true,
                prevent_duplicates: true,
            }],
        }
    }
}

impl FromOptions for RequireFileOverview {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let Some(tags) = opts
            .first()
            .and_then(|o| o.get("tags"))
            .and_then(Value::as_object)
        else {
            return Ok(RequireFileOverview::default());
        };
        let flag =
            |o: &Map<String, Value>, k: &str| o.get(k).and_then(Value::as_bool) == Some(true);
        let tags = tags
            .iter()
            .map(|(name, v)| {
                let empty = Map::new();
                let o = v.as_object().unwrap_or(&empty);
                TagCheck {
                    name: name.clone(),
                    initial_comments_only: flag(o, "initialCommentsOnly"),
                    must_exist: flag(o, "mustExist"),
                    prevent_duplicates: flag(o, "preventDuplicates"),
                }
            })
            .collect();
        Ok(RequireFileOverview { tags })
    }
}

/// Where the first node without a JSDoc block of its own starts, if past offset 0.
fn first_code(finder: &Finder<'_>) -> Option<u32> {
    let ast = finder.ast;
    let mut best: Option<u32> = None;
    let mut stack = vec![(ast.root, 0u32)];
    let mut kids = Vec::new();
    while let Some((id, depth)) = stack.pop() {
        if id != ast.root {
            let start = ast.span(id).start;
            let documented = finder.jsdoc_of_target(Target::Node(id)).is_some()
                || (finder.is_exported_decl(id)
                    && finder.jsdoc_of_target(Target::Export(id)).is_some());
            if !documented && start > 0 && best.is_none_or(|b| start < b) {
                best = Some(start);
            }
        }
        if depth >= 1600 {
            continue;
        }
        kids.clear();
        ast.for_each_child(id, |c| kids.push(c));
        for &c in kids.iter().rev() {
            stack.push((c, depth + 1));
        }
    }
    best
}

/// What the blocks showed for one tag.
#[derive(Debug, Default, Clone, Copy)]
struct Seen {
    found: bool,
    duplicate: bool,
    after_code: bool,
}

impl Rule for RequireFileOverview {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        if !jsdoc::settings_load(cx) {
            return;
        }
        let settings = Settings::from_cx(cx);
        let file = cx.file;
        let code = first_code(&Finder {
            ast: file.ast,
            src: file.src,
            parents: file.parents,
            lines: file.lines,
            settings: &settings,
        });
        let preferred: Vec<Preferred> = self
            .tags
            .iter()
            .map(|t| preferred_tag(&settings, &t.name))
            .collect();
        let mut seen = vec![Seen::default(); self.tags.len()];
        each_block(cx, |_, _, doc| {
            for (s, p) in seen.iter_mut().zip(&preferred) {
                let has = matches!(p, Preferred::Name(n) if doc.block.has_tag(n));
                if !has {
                    continue;
                }
                if s.found {
                    s.duplicate = true;
                } else {
                    s.found = true;
                    s.after_code = code.is_some_and(|c| c < doc.comment.start);
                }
            }
        });
        for ((t, p), s) in self.tags.iter().zip(&preferred).zip(&seen) {
            let name = match p {
                Preferred::Name(n) => n,
                Preferred::Blocked(_) => {
                    let msg = format!(
                        "`settings.jsdoc.tagNamePreference` cannot block @{} for the \
                         `require-file-overview` rule",
                        t.name
                    );
                    cx.report(Span::at(0), msg).loc(Loc::at(1, 2));
                    continue;
                }
            };
            let mut msgs = Vec::new();
            if t.must_exist && !s.found {
                msgs.push(format!("Missing @{name}"));
            }
            if t.prevent_duplicates && s.duplicate {
                msgs.push(format!("Duplicate @{name}"));
            }
            if t.initial_comments_only && s.after_code {
                msgs.push(format!("@{name} should be at the beginning of the file"));
            }
            for m in msgs {
                cx.report(Span::at(0), m).loc(Loc::at(1, 2));
            }
        }
    }
}
