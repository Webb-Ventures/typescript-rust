//! `biome.json` / `biome.jsonc` linter section: `linter.enabled`, `linter.rules` (groups,
//! `recommended`), `files.includes`/`files.ignore`, and `overrides`.

use std::path::Path;

use serde_json::Value;

use super::glob::Glob;
use super::mapping::{BIOME_RULES, UnsupportedReason, map_biome_name};
use super::oxlint::parse_jsonc;
use super::{ConfigFile, ConfigKind, ConfigObject, FilesEntry, RuleEntry};
use crate::diagnostic::Severity;
use crate::registry::Registry;

pub const BIOME_CONFIG_NAMES: &[&str] = &["biome.json", "biome.jsonc"];

pub fn load(path: &Path, reg: &Registry) -> Result<ConfigFile, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let v = parse_jsonc(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let base = path.parent().unwrap_or(Path::new(".")).to_path_buf();
    Ok(ConfigFile::new(
        ConfigKind::Biome,
        Some(path.to_path_buf()),
        base,
        objects_from_biome(&v, reg),
    ))
}

fn biome_severity(v: &Value) -> Option<(Severity, Option<Value>)> {
    match v {
        Value::String(s) => Some((Severity::parse(s)?, None)),
        Value::Object(o) => {
            let level = o.get("level").and_then(Value::as_str).unwrap_or("error");
            Some((Severity::parse(level)?, o.get("options").cloned()))
        }
        _ => None,
    }
}

fn rules_from(rules: &Value, reg: &Registry, recommended_default: bool) -> Vec<RuleEntry> {
    let mut out = Vec::new();
    let recommended = rules
        .get("recommended")
        .and_then(Value::as_bool)
        .unwrap_or(recommended_default);
    if recommended {
        for (_, _, ours, rec) in BIOME_RULES {
            if *rec && let Some(d) = reg.find(ours) {
                out.push(RuleEntry {
                    written: d.meta.name.to_owned(),
                    mapped: Ok(d.meta.name),
                    severity: Severity::Error,
                    options: None,
                });
            }
        }
    }
    if let Value::Object(groups) = rules {
        for (group, gv) in groups {
            let Value::Object(entries) = gv else { continue };
            for (name, val) in entries {
                if name == "recommended" || name == "all" {
                    continue;
                }
                let Some((sev, _opts)) = biome_severity(val) else {
                    continue;
                };
                let mapped = map_biome_name(reg, group, name);
                out.push(RuleEntry {
                    written: format!("{group}/{name}"),
                    mapped,
                    severity: sev,
                    // Biome options differ from ESLint's; use our defaults.
                    options: None,
                });
            }
        }
    }
    out
}

pub fn objects_from_biome(v: &Value, reg: &Registry) -> Vec<ConfigObject> {
    let mut out = Vec::new();
    let enabled = v
        .pointer("/linter/enabled")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let mut ignores = Vec::new();
    for key in ["/files/ignore", "/linter/ignore"] {
        for s in v
            .pointer(key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(s) = s.as_str() {
                ignores.push((false, Glob::new(s)));
            }
        }
    }
    for key in ["/files/includes", "/linter/includes"] {
        for s in v
            .pointer(key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(neg) = s.as_str().and_then(|s| s.strip_prefix('!')) {
                let neg = neg.strip_prefix('!').unwrap_or(neg);
                ignores.push((false, Glob::new(neg)));
            }
        }
    }
    if !ignores.is_empty() {
        out.push(ConfigObject {
            ignores: Some(ignores),
            global_ignores: true,
            ..ConfigObject::default()
        });
    }
    if !enabled {
        return out;
    }
    let mut base = ConfigObject::default();
    let rules = v.pointer("/linter/rules").cloned().unwrap_or(Value::Null);
    base.rules = rules_from(&rules, reg, true);
    out.push(base);
    for o in v
        .get("overrides")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let files: Vec<FilesEntry> = o
            .get("includes")
            .or_else(|| o.get("include"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|s| !s.starts_with('!'))
            .map(|s| FilesEntry::One(Glob::new(s)))
            .collect();
        let rules = o.pointer("/linter/rules").cloned().unwrap_or(Value::Null);
        out.push(ConfigObject {
            files: Some(files),
            rules: rules_from(&rules, reg, false),
            ..ConfigObject::default()
        });
    }
    out
}

/// Biome rules configured but not supported.
pub fn unsupported_names(v: &Value, reg: &Registry) -> Vec<(String, UnsupportedReason)> {
    let mut out = Vec::new();
    if let Some(Value::Object(groups)) = v.pointer("/linter/rules") {
        for (group, gv) in groups {
            if let Value::Object(entries) = gv {
                for name in entries.keys() {
                    if let Err(r) = map_biome_name(reg, group, name) {
                        out.push((format!("{group}/{name}"), r));
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn biome_mapping() {
        let reg = Registry::global();
        let v = serde_json::json!({
            "linter": { "enabled": true, "rules": {
                "recommended": false,
                "suspicious": { "noDebugger": "warn", "noExplicitAny": { "level": "error" } },
                "a11y": { "useAltText": "error" }
            }},
            "files": { "includes": ["**", "!dist"] }
        });
        let objs = objects_from_biome(&v, reg);
        assert!(objs[0].global_ignores);
        let rules = &objs[1].rules;
        assert_eq!(rules.len(), 3);
        let d = rules
            .iter()
            .find(|r| r.written == "suspicious/noDebugger")
            .unwrap();
        assert_eq!(d.mapped, Ok("no-debugger"));
        assert_eq!(d.severity, Severity::Warn);
        assert!(
            rules
                .iter()
                .any(|r| r.written == "a11y/useAltText" && r.mapped.is_err())
        );
    }

    #[test]
    fn group_name_overrides_recommended_default() {
        let reg = Registry::global();
        let rule_names = |rules: serde_json::Value| {
            let v = serde_json::json!({ "linter": { "rules": rules } });
            let objs = objects_from_biome(&v, reg);
            let cfg = ConfigFile::new(ConfigKind::Biome, None, "/p".into(), objs);
            let fc = cfg
                .file_config("a.js", &super::super::ResolveOptions::new())
                .unwrap();
            fc.rules
                .rules
                .iter()
                .map(|r| r.def.meta.name)
                .collect::<Vec<_>>()
        };
        assert!(rule_names(serde_json::json!({})).contains(&"no-debugger"));
        let off = rule_names(serde_json::json!({ "suspicious": { "noDebugger": "off" } }));
        assert!(!off.contains(&"no-debugger"));
    }
}
