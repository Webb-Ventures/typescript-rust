//! `.oxlintrc.json` (JSONC): `rules`, `categories`, `ignorePatterns`, `overrides`, `globals`.

use std::path::Path;

use serde_json::Value;

use super::glob::Glob;
use super::mapping::{UnsupportedReason, map_oxlint_name};
use super::{ConfigFile, ConfigKind, ConfigObject, FilesEntry, RuleEntry, rule_entry};
use crate::diagnostic::Severity;
use crate::env::GlobalAccess;
use crate::registry::Registry;
use crate::rule::Category;

pub const OXLINT_CONFIG_NAMES: &[&str] = &[".oxlintrc.json", "oxlintrc.json"];

/// Loads an oxlint config.
pub fn load(path: &Path, reg: &Registry) -> Result<ConfigFile, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let v = parse_jsonc(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let base = path.parent().unwrap_or(Path::new(".")).to_path_buf();
    Ok(ConfigFile::new(
        ConfigKind::Oxlint,
        Some(path.to_path_buf()),
        base,
        objects_from_oxlint(&v, reg)?,
    ))
}

pub fn objects_from_oxlint(v: &Value, reg: &Registry) -> Result<Vec<ConfigObject>, String> {
    let mut out = Vec::new();
    if let Some(Value::Array(ign)) = v.get("ignorePatterns") {
        out.push(ConfigObject {
            ignores: Some(
                ign.iter()
                    .filter_map(Value::as_str)
                    .map(|s| match s.strip_prefix('!') {
                        Some(r) => (true, Glob::new(r)),
                        None => (false, Glob::new(s)),
                    })
                    .collect(),
            ),
            global_ignores: true,
            ..ConfigObject::default()
        });
    }
    let mut base = ConfigObject::default();
    // Categories first (default: correctness = warn), then explicit rules.
    let mut cats: Vec<(Category, Severity)> = vec![(Category::Correctness, Severity::Warn)];
    if let Some(Value::Object(c)) = v.get("categories") {
        for (k, val) in c {
            if let (Some(cat), Some(sev)) = (Category::parse(k), Severity::from_json(val)) {
                cats.retain(|(c, _)| *c != cat);
                cats.push((cat, sev));
            }
        }
    }
    for (cat, sev) in &cats {
        for d in reg.all() {
            if d.meta.category == *cat && !d.meta.requires_types {
                base.rules.push(RuleEntry {
                    written: d.meta.name.to_owned(),
                    mapped: Ok(d.meta.name),
                    severity: *sev,
                    options: None,
                });
            }
        }
    }
    if let Some(Value::Object(rules)) = v.get("rules") {
        for (name, val) in rules {
            base.rules
                .push(rule_entry(reg, name, val, map_oxlint_name)?);
        }
    }
    if let Some(Value::Object(g)) = v.get("globals") {
        for (k, val) in g {
            if let Some(a) = GlobalAccess::from_json(val) {
                base.language.globals.push((k.clone(), a));
            }
        }
    }
    if v.get("env").is_some() {
        base.notes
            .push("oxlint `env` is not mapped; declare globals with `globals`".to_owned());
    }
    out.push(base);
    if let Some(Value::Array(ovs)) = v.get("overrides") {
        for o in ovs {
            let files: Vec<FilesEntry> = o
                .get("files")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(|s| FilesEntry::One(Glob::new(s)))
                .collect();
            let mut obj = ConfigObject {
                files: Some(files),
                ..ConfigObject::default()
            };
            if let Some(Value::Object(rules)) = o.get("rules") {
                for (name, val) in rules {
                    obj.rules.push(rule_entry(reg, name, val, map_oxlint_name)?);
                }
            }
            out.push(obj);
        }
    }
    Ok(out)
}

/// Rule names in an oxlint config that we cannot run.
pub fn unsupported_names(v: &Value, reg: &Registry) -> Vec<(String, UnsupportedReason)> {
    let mut out = Vec::new();
    if let Some(Value::Object(rules)) = v.get("rules") {
        for name in rules.keys() {
            if let Err(r) = map_oxlint_name(reg, name) {
                out.push((name.clone(), r));
            }
        }
    }
    out
}

/// Parses JSON with comments and trailing commas.
pub fn parse_jsonc(text: &str) -> Result<Value, String> {
    let stripped = strip_jsonc(text);
    serde_json::from_str(&stripped).map_err(|e| e.to_string())
}

/// Removes `//` and `/* */` comments and trailing commas, keeping string contents.
pub fn strip_jsonc(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let mut in_str = false;
    while i < b.len() {
        let c = b[i];
        if in_str {
            if c == b'\\' && i + 1 < b.len() {
                // The escaped character may be multi-byte (an invalid escape such as `\é`,
                // left for the JSON parser to report).
                let end = (i + 1 + utf8_len(b[i + 1])).min(b.len());
                out.push_str(&text[i..end]);
                i = end;
                continue;
            }
            if c == b'"' {
                in_str = false;
            }
            let ch_len = utf8_len(c);
            out.push_str(&text[i..(i + ch_len).min(b.len())]);
            i += ch_len;
            continue;
        }
        match c {
            b'"' => {
                in_str = true;
                out.push('"');
                i += 1;
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i += 2;
            }
            b',' => {
                let mut j = i + 1;
                while j < b.len() && (b[j] as char).is_ascii_whitespace() {
                    j += 1;
                }
                if j < b.len() && (b[j] == b'}' || b[j] == b']') {
                    i += 1;
                } else {
                    out.push(',');
                    i += 1;
                }
            }
            _ => {
                let ch_len = utf8_len(c);
                out.push_str(&text[i..(i + ch_len).min(b.len())]);
                i += ch_len;
            }
        }
    }
    out
}

fn utf8_len(b: u8) -> usize {
    match b {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonc() {
        let v = parse_jsonc("{ // c\n \"a\": [1, 2,], /* x */ \"b\": \"//not\", }").unwrap();
        assert_eq!(v, serde_json::json!({"a": [1, 2], "b": "//not"}));
    }

    #[test]
    fn jsonc_invalid_multibyte_escape_is_an_error() {
        assert_eq!(strip_jsonc("{\"x\":\"\\é\"}"), "{\"x\":\"\\é\"}");
        assert!(parse_jsonc("{\"x\":\"\\é\"}").is_err());
    }

    #[test]
    fn oxlint_rules_map() {
        let reg = Registry::global();
        let v = serde_json::json!({
            "rules": { "no-debugger": "error", "eslint/no-debugger": "warn", "react/jsx-key": "error",
                       "unicorn/escape-case": "error", "jsdoc/check-access": "error" },
            "ignorePatterns": ["dist/"]
        });
        let objs = objects_from_oxlint(&v, reg).unwrap();
        assert!(objs[0].global_ignores);
        let rules = &objs[1].rules;
        let r = rules
            .iter()
            .find(|r| r.written == "eslint/no-debugger")
            .unwrap();
        assert_eq!(r.mapped, Ok("no-debugger"));
        let p = rules.iter().find(|r| r.written == "react/jsx-key").unwrap();
        assert_eq!(p.mapped, Err(UnsupportedReason::Plugin));
        for name in ["unicorn/escape-case", "jsdoc/check-access"] {
            let r = rules.iter().find(|r| r.written == name).unwrap();
            assert_eq!(r.mapped, Ok(name));
        }
    }
}
