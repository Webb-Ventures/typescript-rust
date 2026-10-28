//! `no-restricted-syntax`: disallow syntax matched by esquery selectors.
//!
//! Each option is a selector (or `{selector, message}`), matched against the file's ESTree
//! view ([`crate::estree`]) the way ESLint matches listener selectors: every node in
//! traversal order, selectors on one node in ESLint's specificity order.

use serde_json::Value;

use super::{FromOptions, NO_PRESET, create, meta};
use crate::esquery::{self, Selector};
use crate::estree::EsTree;
use crate::rule::{Category, Ctx, Rule, RuleDef};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-restricted-syntax",
        "Disallow specified syntax",
        NO_PRESET,
        Category::Restriction,
    ),
    create: create::<NoRestrictedSyntax>,
};

const DEFAULT_MESSAGE: &str = "Using '{{selector}}' is not allowed.";

#[derive(Debug)]
struct Entry {
    sel: Selector,
    message: String,
}

#[derive(Debug, Default)]
struct NoRestrictedSyntax {
    entries: Vec<Entry>,
}

impl FromOptions for NoRestrictedSyntax {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let mut entries: Vec<Entry> = Vec::new();
        for o in opts {
            let (raw, message) = match o {
                Value::String(s) => (s.as_str(), None),
                Value::Object(m) => match m.get("selector").and_then(Value::as_str) {
                    Some(s) => (s, m.get("message").and_then(Value::as_str)),
                    None => return Err("no-restricted-syntax: missing `selector`".into()),
                },
                _ => return Err("no-restricted-syntax: options are selectors".into()),
            };
            // ESLint keys its listeners by the selector text: a repeated selector keeps the
            // last message.
            // A custom message is used as written; only the default names the selector.
            let message = match message.filter(|m| !m.is_empty()) {
                Some(m) => m.to_string(),
                None => DEFAULT_MESSAGE.replace("{{selector}}", raw),
            };
            if let Some(e) = entries.iter_mut().find(|e| e.sel.raw == raw) {
                e.message = message;
                continue;
            }
            let mut sel = esquery::parse(raw.strip_suffix(":exit").unwrap_or(raw))
                .map_err(|e| format!("no-restricted-syntax: invalid selector `{raw}`: {e}"))?;
            sel.raw = raw.to_string();
            entries.push(Entry { sel, message });
        }
        entries.sort_by(|a, b| a.sel.compare(&b.sel));
        Ok(NoRestrictedSyntax { entries })
    }
}

impl Rule for NoRestrictedSyntax {
    fn has_run_file(&self) -> bool {
        !self.entries.is_empty()
    }

    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let tree = EsTree::build(cx.ast(), cx.src());
        let prepared: Vec<_> = self.entries.iter().map(|e| e.sel.prepare()).collect();
        for n in 0..tree.len() as u32 {
            for (e, p) in self.entries.iter().zip(&prepared) {
                if p.matches_listener(&tree, n) {
                    cx.report(tree.span(n), e.message.clone())
                        .id("restrictedSyntax");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
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
            "rules": { "no-restricted-syntax": rule },
        }]);
        let objs = objects_from_eslint_json(&cfg, Registry::global()).unwrap();
        let cfg = ConfigFile::new(ConfigKind::Eslint, None, "/p".into(), objs);
        let fc = cfg.file_config("a.ts", &ResolveOptions::new()).unwrap();
        lint_source("/p/a.ts", code, &fc, &NoTypeInfo, FileKey(0))
            .diagnostics
            .iter()
            .filter(|d| d.rule == Some("no-restricted-syntax"))
            .map(|d| format!("{}:{}", d.span.start, d.message))
            .collect()
    }

    #[test]
    fn selectors_and_messages() {
        let code = "const a = 1; var b = 2; foo(a, b);";
        let got = messages(
            code,
            serde_json::json!([
                "VariableDeclaration[kind='var']",
                { "selector": "CallExpression[arguments.length=2]", "message": "Two args." },
            ]),
        );
        assert_eq!(
            got,
            vec![
                "13:Using 'VariableDeclaration[kind='var']' is not allowed.".to_string(),
                "24:Two args.".to_string(),
            ]
        );
    }

    #[test]
    fn invalid_selector_is_an_option_error() {
        use crate::rules::FromOptions;
        let r = super::NoRestrictedSyntax::from_options(&[serde_json::json!("Foo[")]);
        assert!(r.is_err());
    }
}
