//! Regression tests for the round-8 import rules: `import/order` on large and odd inputs
//! (many imports, a comparison that is not a total order, non-ASCII names and comments).

use std::path::PathBuf;

use ts_r_lint::config::{ConfigFile, ConfigKind, ResolveOptions, objects_from_eslint_json};
use ts_r_lint::types::{FileKey, NoTypeInfo};
use ts_r_lint::{Registry, lint_and_fix, lint_source};

ts_r_testing::cap_memory!();

fn file_config(
    rule: &str,
    options: serde_json::Value,
    file: &str,
) -> std::sync::Arc<ts_r_lint::FileConfig> {
    let mut entry = vec![serde_json::json!("error")];
    if let serde_json::Value::Array(o) = options {
        entry.extend(o);
    }
    let config = serde_json::json!([{
        "files": ["**/*.{js,mjs,cjs,jsx,ts,tsx,mts,cts}"],
        "rules": { rule: entry },
    }]);
    let objs = objects_from_eslint_json(&config, Registry::global()).expect("config");
    let cfg = ConfigFile::new(ConfigKind::Eslint, None, PathBuf::from("/t"), objs);
    cfg.file_config(file, &ResolveOptions::new())
        .expect("file config")
}

fn messages(rule: &str, options: serde_json::Value, file: &str, code: &str) -> Vec<String> {
    let fc = file_config(rule, options, file);
    let r = lint_source(&format!("/t/{file}"), code, &fc, &NoTypeInfo, FileKey(0));
    assert!(
        !r.diagnostics.iter().any(|d| d.fatal),
        "parse error in {code:?}"
    );
    r.diagnostics.iter().map(|d| d.message.clone()).collect()
}

fn fixed(rule: &str, options: serde_json::Value, file: &str, code: &str) -> String {
    let fc = file_config(rule, options, file);
    let r = lint_and_fix(&format!("/t/{file}"), code, &fc, &NoTypeInfo, FileKey(0));
    r.output.unwrap_or_else(|| code.to_owned())
}

#[test]
fn order_many_imports_is_fast() {
    // 4,000 requires in reverse alphabetical order: every one but the first is out of
    // order, which used to cost a scan of the list per report.
    let mut code = String::new();
    for i in (0..4000).rev() {
        code.push_str(&format!("var m{i} = require('./m{i:05}');\n"));
    }
    let opts = serde_json::json!([{ "alphabetize": { "order": "asc" } }]);
    let start = std::time::Instant::now();
    let m = messages("import/order", opts, "a.js", &code);
    assert_eq!(m.len(), 3999);
    assert!(start.elapsed().as_secs() < 20, "took {:?}", start.elapsed());
}

#[test]
fn order_inconsistent_comparison_does_not_panic() {
    // `./z` ties with `../a`, `../a` sorts before `./a/b`, `./a/b` before `./z`: not a total
    // order, which the sort must survive.
    let code = "import z from './z';\nimport a from '../a';\nimport b from './a/b';\nimport c from '../c/d';\n";
    let opts = serde_json::json!([{
        "groups": [["parent", "sibling"]],
        "alphabetize": { "order": "asc" },
    }]);
    messages("import/order", opts.clone(), "a.js", code);
    fixed("import/order", opts, "a.js", code);
}

#[test]
fn order_non_ascii_names_and_comments() {
    let code = "import { é, a as ü } from './x'; // ü\nimport fs from 'fs'; /* é */\nconst { ß, b } = require('./ö');\n";
    let opts = serde_json::json!([{
        "named": true,
        "alphabetize": { "order": "asc" },
        "newlines-between": "always",
    }]);
    let m = messages("import/order", opts.clone(), "a.js", code);
    assert!(!m.is_empty());
    let out = fixed("import/order", opts, "a.js", code);
    assert!(out.contains("import fs from 'fs'; /* é */"), "{out}");
}

#[test]
fn order_path_groups_with_globstar() {
    // `**` matches no segment at all, so `Internal/lib` is in the internal path group.
    let code =
        "import a from 'react';\n\nimport b from 'Internal/lib';\n\nimport c from '../parent';\n";
    let opts = serde_json::json!([{
        "pathGroups": [{ "pattern": "Internal/**/*", "group": "internal" }],
        "pathGroupsExcludedImportTypes": [],
        "groups": ["builtin", "external", "internal", "parent", "sibling", "index"],
        "newlines-between": "always",
    }]);
    assert_eq!(
        messages("import/order", opts, "a.js", code),
        Vec::<String>::new()
    );
}
