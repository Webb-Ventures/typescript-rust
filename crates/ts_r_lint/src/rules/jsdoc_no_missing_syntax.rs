//! `jsdoc/no-missing-syntax`: report when no JSDoc block of the file is found through a
//! `contexts` entry (at least `minimum` times).
//!
//! Every block the contexts select is counted under the context and `comment` it was found
//! through: an object entry counts under its `context` (`any` when missing) and its
//! `comment`; a string entry under its own text with no comment; a block no entry claims
//! under neither. At the end of the file the entries are checked in order and the first one
//! short of its `minimum` reports, at 1:2 (the plugin's fixed location). A string entry
//! never has its count (it is checked with an empty comment, while its blocks are counted
//! with none), so it always reports; an `any` entry counts the blocks found with its
//! `comment` under every context.

use std::collections::BTreeMap;

use serde_json::Map;

use super::jsdoc_no_restricted_syntax::interpolate;
use super::prelude::*;
use crate::diagnostic::Loc;
use crate::jsdoc::{self, Contexts, Settings};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/no-missing-syntax",
        "Reports when certain comment structures are always expected.",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<NoMissingSyntax>,
};

const MISSING: &str = "Rule `no-missing-syntax` is missing a `contexts` option.";

#[derive(Debug, Default)]
struct NoMissingSyntax {
    /// `options[0]`; `None` when the rule has no options.
    opts: Option<Map<String, Value>>,
}

impl FromOptions for NoMissingSyntax {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let opts = match opts.first() {
            None => None,
            Some(Value::Object(o)) => {
                Contexts::from_options(o, &Settings::default_for_validation())?;
                Some(o.clone())
            }
            Some(_) => return Err("jsdoc/no-missing-syntax: expected an object".into()),
        };
        Ok(NoMissingSyntax { opts })
    }
}

/// The key a block (or an entry, `None` = "undefined") is counted under.
type Key = (Option<String>, Option<String>);

impl Rule for NoMissingSyntax {
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
        let report = |cx: &mut Ctx<'_, '_>, msg: String| {
            cx.report(Span::at(0), msg).loc(Loc::at(1, 2));
        };
        if self.opts.is_none() && settings.contexts.is_none() {
            report(cx, MISSING.to_owned());
            return;
        }
        // Blocks are counted only when the rule has options (the plugin's iteration returns
        // early without them, while its end-of-file check falls back to the setting).
        let mut counts: BTreeMap<Key, u32> = BTreeMap::new();
        if let Some(opts) = &self.opts {
            let Ok(mut contexts) = Contexts::from_options(opts, &settings) else {
                return;
            };
            contexts.additive = true;
            jsdoc::for_each_doc(cx, &settings, &contexts, |_cx, _finder, doc| {
                let context = contexts
                    .find(doc)
                    .map(|e| e.ty.clone().unwrap_or_else(|| "any".to_owned()));
                *counts
                    .entry((context, doc.info_comment.clone()))
                    .or_default() += 1;
            });
        }
        let list = match self.opts.as_ref().and_then(|o| o.get("contexts")) {
            Some(Value::Array(l)) => l,
            Some(_) => return,
            None => match &settings.contexts {
                Some(l) => l,
                None => return,
            },
        };
        for entry in list {
            let (context, comment, minimum, message) = match entry {
                Value::String(s) => (s.clone(), String::new(), 1.0, None),
                Value::Object(o) => (
                    match o.get("context") {
                        Some(Value::String(s)) => s.clone(),
                        _ => "any".to_owned(),
                    },
                    o.get("comment")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned(),
                    o.get("minimum").and_then(Value::as_f64).unwrap_or(1.0),
                    Some(o.get("message").and_then(Value::as_str).map(str::to_owned)),
                ),
                _ => continue,
            };
            let string_entry = message.is_none();
            // A string entry looks for an empty comment, which no block is counted with.
            let count = |ctx: &Option<String>| -> f64 {
                if string_entry || comment.is_empty() {
                    return 0.0;
                }
                counts
                    .get(&(ctx.clone(), Some(comment.clone())))
                    .copied()
                    .unwrap_or(0)
                    .into()
            };
            let any = context == "any";
            let key = if any { None } else { Some(context.clone()) };
            let short = count(&key) < minimum
                && (!any || counts.keys().map(|(c, _)| c).all(|c| count(c) < minimum));
            if !short {
                continue;
            }
            let template = match message.flatten() {
                Some(m) => m,
                None if !comment.is_empty() => {
                    "Syntax is required: {{context}} with {{comment}}".to_owned()
                }
                None => "Syntax is required: {{context}}".to_owned(),
            };
            let msg = interpolate(
                &template,
                &[("comment", comment.as_str()), ("context", context.as_str())],
            );
            report(cx, msg);
            return;
        }
    }
}
