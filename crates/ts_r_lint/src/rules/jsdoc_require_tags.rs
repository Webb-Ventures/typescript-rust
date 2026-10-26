//! `jsdoc/require-tags`: blocks must have the listed tags.
//!
//! Each `tags` entry (a tag name, or `{ tag, context }`) becomes a "forbid" context: a block
//! found through `context` (default `any`) whose `JsdocBlock` has no `JsdocTag` of that name
//! reports `Missing required tag "<tag>"`. Without `tags`, every function block reports the
//! missing option.

use serde_json::{Map, json};

use super::jsdoc_no_restricted_syntax::run_forbid;
use super::prelude::*;
use crate::jsdoc::{Contexts, Settings};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/require-tags",
        "Requires tags be present, optionally for specific contexts",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<RequireTags>,
};

const MISSING: &str = "Rule `require-tags` is missing a `tags` option.";

#[derive(Debug, Default)]
struct RequireTags {
    /// The `contexts` the tags translate to; `None` without a `tags` option.
    opts: Option<Map<String, Value>>,
}

impl FromOptions for RequireTags {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let Some(tags) = opts
            .first()
            .and_then(|o| o.get("tags"))
            .and_then(Value::as_array)
        else {
            return Ok(RequireTags { opts: None });
        };
        let mut contexts = Vec::new();
        for t in tags {
            let (tag, context) = match t {
                Value::String(s) => (s.as_str(), "any"),
                Value::Object(o) => (
                    o.get("tag").and_then(Value::as_str).unwrap_or(""),
                    o.get("context").and_then(Value::as_str).unwrap_or("any"),
                ),
                _ => return Err("jsdoc/require-tags: invalid `tags` entry".into()),
            };
            contexts.push(json!({
                "comment": format!("JsdocBlock:not(*:has(JsdocTag[tag={tag}]))"),
                "context": context,
                "message": format!("Missing required tag \"{tag}\""),
            }));
        }
        let mut map = Map::new();
        map.insert("contexts".into(), Value::Array(contexts));
        Contexts::from_options(&map, &Settings::default_for_validation())?;
        Ok(RequireTags { opts: Some(map) })
    }
}

impl Rule for RequireTags {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        run_forbid(cx, self.opts.as_ref(), MISSING);
    }
}
