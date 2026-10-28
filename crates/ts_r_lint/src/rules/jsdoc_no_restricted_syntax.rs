//! `jsdoc/no-restricted-syntax`: report JSDoc blocks found through a `contexts` entry (an
//! AST context, a `comment` selector on the block, or both).
//!
//! The rule iterates its own `contexts` option only (`settings.jsdoc.contexts` is ignored;
//! with no options the four function types are visited and each block reports the missing
//! option). A block reports when an entry applies to it: a string entry whose selector matches
//! the node, or an object entry whose `context` matches the node (or is `any`) and whose
//! `comment` is the one the block was found through.

use serde_json::Map;

use super::prelude::*;
use crate::jsdoc::{self, Contexts, Settings};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/no-restricted-syntax",
        "Reports when certain comment structures are present.",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<NoRestrictedSyntax>,
};

const MISSING: &str = "Rule `no-restricted-syntax` is missing a `contexts` option.";

#[derive(Debug, Default)]
struct NoRestrictedSyntax {
    /// `options[0]`; `None` when the rule has no options.
    opts: Option<Map<String, Value>>,
}

impl FromOptions for NoRestrictedSyntax {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let opts = match opts.first() {
            None => None,
            Some(Value::Object(o)) => {
                // Reject bad selectors when the config loads.
                Contexts::from_options(o, &Settings::default_for_validation())?;
                Some(o.clone())
            }
            Some(_) => return Err("jsdoc/no-restricted-syntax: expected an object".into()),
        };
        Ok(NoRestrictedSyntax { opts })
    }
}

/// ESLint's message placeholders: `{{ key }}` becomes the value, unknown keys stay.
fn interpolate(msg: &str, data: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(msg.len());
    let mut rest = msg;
    while let Some(open) = rest.find("{{") {
        out.push_str(&rest[..open]);
        let after = &rest[open + 2..];
        let Some(close) = after.find("}}") else {
            out.push_str(&rest[open..]);
            return out;
        };
        let inner = &after[..close];
        let key = inner.trim();
        match data.iter().find(|(k, _)| *k == key) {
            Some((_, v)) if !inner.contains(['{', '}']) => out.push_str(v),
            _ => out.push_str(&rest[open..open + 2 + close + 2]),
        }
        rest = &after[close + 2..];
    }
    out.push_str(rest);
    out
}

impl Rule for NoRestrictedSyntax {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let mut settings = Settings::from_cx(cx);
        settings.contexts = None;
        let empty = Map::new();
        let Ok(contexts) = Contexts::from_options(self.opts.as_ref().unwrap_or(&empty), &settings)
        else {
            return;
        };
        let has_contexts = self
            .opts
            .as_ref()
            .is_some_and(|o| o.contains_key("contexts"));
        jsdoc::for_each_doc(cx, &settings, &contexts, |cx, _finder, doc| {
            if self.opts.is_none() {
                cx.report(doc.comment, MISSING);
                return;
            }
            if !has_contexts {
                return;
            }
            let Some(entry) = contexts.find(doc) else {
                return;
            };
            let context = entry.ty.as_deref().unwrap_or("any");
            let comment = doc.info_comment.as_deref();
            let template = match (&entry.message, comment) {
                (Some(m), _) => m.clone(),
                (None, Some(_)) => "Syntax is restricted: {{context}} with {{comment}}".into(),
                (None, None) => "Syntax is restricted: {{context}}".into(),
            };
            let mut data = vec![("context", context)];
            if let Some(c) = comment {
                data.push(("comment", c));
            }
            cx.report(doc.comment, interpolate(&template, &data));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::interpolate;
    use crate::config::{ConfigFile, ConfigKind, ResolveOptions, objects_from_eslint_json};
    use crate::engine::lint_source;
    use crate::registry::Registry;
    use crate::types::{FileKey, NoTypeInfo};

    fn messages(code: &str, options: serde_json::Value) -> Vec<String> {
        let mut rule = vec![serde_json::json!("error")];
        rule.extend(options.as_array().cloned().unwrap_or_default());
        let cfg = serde_json::json!([{
            "files": ["**/*.ts"],
            "languageOptions": { "parser": "typescript-eslint/parser" },
            "rules": { "jsdoc/no-restricted-syntax": rule },
        }]);
        let objs = objects_from_eslint_json(&cfg, Registry::global()).unwrap();
        let cfg = ConfigFile::new(ConfigKind::Eslint, None, "/p".into(), objs);
        let fc = cfg.file_config("a.ts", &ResolveOptions::new()).unwrap();
        lint_source("/p/a.ts", code, &fc, &NoTypeInfo, FileKey(0))
            .diagnostics
            .iter()
            .filter(|d| d.rule == Some("jsdoc/no-restricted-syntax"))
            .map(|d| format!("{}:{}", d.span.start, d.message))
            .collect()
    }

    const CODE: &str = "/** @param {Foo|Bar} x */\nfunction f(x) {}\n";

    #[test]
    fn found_through_a_comment() {
        let sel = "JsdocBlock:has(JsdocTypeUnion > JsdocTypeName[value=\"Foo\"]:nth-child(1))";
        let got = messages(
            CODE,
            serde_json::json!([{ "contexts": [{ "comment": sel, "context": "FunctionDeclaration" }] }]),
        );
        assert_eq!(
            got,
            vec![format!(
                "0:Syntax is restricted: FunctionDeclaration with {sel}"
            )]
        );
        let got = messages(
            CODE,
            serde_json::json!([{ "contexts": [{ "comment": sel, "message": "No {{context}}." }] }]),
        );
        assert_eq!(got, vec!["0:No any.".to_string()]);
    }

    #[test]
    fn plain_any_has_no_comment() {
        // Under a plain `any` the block is found with no comment, so an entry that needs one
        // does not apply (and the plain `any` string itself matches no node).
        let got = messages(
            CODE,
            serde_json::json!([{ "contexts": ["any", { "comment": "JsdocBlock", "context": "any" }] }]),
        );
        assert!(got.is_empty(), "{got:?}");
    }

    #[test]
    fn missing_contexts() {
        let got = messages(CODE, serde_json::json!([]));
        assert_eq!(got, vec![format!("0:{}", super::MISSING)]);
    }

    #[test]
    fn placeholders() {
        let data = [
            ("context", "FunctionDeclaration"),
            ("comment", "JsdocBlock"),
        ];
        assert_eq!(
            interpolate("Oops: `{{context}}` / {{ comment }}.", &data),
            "Oops: `FunctionDeclaration` / JsdocBlock."
        );
        assert_eq!(
            interpolate("{{other}} {{context", &data),
            "{{other}} {{context"
        );
    }
}
