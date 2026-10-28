//! Regression tests from the review of the round-6 rules: non-ASCII source text where a rule
//! slices by byte offset, `import/no-duplicates` merging quoted names that hold commas, and
//! the per-file indent unit that fixes ask for once per report.

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

/// The messages of `rule` on `code` linted as `/t/<file>`.
fn messages(rule: &str, options: serde_json::Value, file: &str, code: &str) -> Vec<String> {
    let fc = file_config(rule, options, file);
    let r = lint_source(&format!("/t/{file}"), code, &fc, &NoTypeInfo, FileKey(0));
    assert!(
        !r.diagnostics.iter().any(|d| d.fatal),
        "parse error in {code:?}"
    );
    r.diagnostics.iter().map(|d| d.message.clone()).collect()
}

/// The text after every fix pass.
fn fixed(rule: &str, options: serde_json::Value, file: &str, code: &str) -> String {
    let fc = file_config(rule, options, file);
    let r = lint_and_fix(&format!("/t/{file}"), code, &fc, &NoTypeInfo, FileKey(0));
    r.output.unwrap_or_else(|| code.to_owned())
}

#[test]
fn no_unnecessary_parameters_non_ascii_directive() {
    let r = "unicorn/no-unnecessary-parameters";
    let none = serde_json::json!([]);
    // A block-level function in a script makes the rule look for "use strict" directives;
    // the directive's 11th byte falls inside `é`.
    for code in [
        "\"aaaaaaaaaé\"; { function f() {} }",
        "'aaaaaaaaé€'; { function f() {} }",
        "\"é\"; { function f() {} }",
        "function g() { \"aaaaaaaaaé\"; { function f() {} } }",
    ] {
        messages(r, none.clone(), "a.cjs", code);
        fixed(r, none.clone(), "a.cjs", code);
    }
    // A real directive still makes the block function strict (no dynamic scope): the
    // unused parameter is reported, as without the block function.
    let strict = "\"use strict\"; { function f() {} } function h(a) {} h(1);";
    let plain = "function h(a) {} h(1);";
    assert_eq!(
        messages(r, none.clone(), "a.cjs", strict).len(),
        messages(r, none.clone(), "a.cjs", plain).len()
    );
}

#[test]
fn no_duplicates_quoted_names_with_commas() {
    let r = "import/no-duplicates";
    let none = serde_json::json!([]);
    let code = "import { \"a,b\" as ab } from \"m\";\nimport { \"a,c\" as ac } from \"m\";\n";
    assert_eq!(messages(r, none.clone(), "a.mjs", code).len(), 2);
    assert_eq!(
        fixed(r, none.clone(), "a.mjs", code),
        "import { \"a,b\" as ab , \"a,c\" as ac } from \"m\";\n"
    );
    // A name already imported is not added twice, even with a comma in it.
    let dup = "import { \"x,y\" as xy, z } from \"m\";\nimport { \"x,y\" as xy } from \"m\";\n";
    assert_eq!(
        fixed(r, none.clone(), "a.mjs", dup),
        "import { \"x,y\" as xy, z } from \"m\";\n"
    );
    // Plain names keep their spacing, as before.
    assert_eq!(
        fixed(
            r,
            none,
            "a.mjs",
            "import { a } from 'm';\nimport { b, c } from 'm';\n"
        ),
        "import { a , b, c } from 'm';\n"
    );
}

#[test]
fn indent_unit_is_worked_out_once_per_file() {
    // Many multi-line concise arrows in one file: every fix uses the file's indent unit
    // (two spaces here), which is computed once, not once per arrow.
    let r = "unicorn/consistent-arrow-return-style";
    let none = serde_json::json!([]);
    let mut code = String::from("if (x) {\n  y();\n}\n");
    for i in 0..2000 {
        code.push_str(&format!("const f{i} = () => a +\n  {i};\n"));
    }
    let out = fixed(r, none, "a.js", &code);
    assert!(
        out.contains("const f0 = () => {\n  return a +\n    0;\n};\n"),
        "{}",
        &out[..200]
    );
    assert!(out.contains("const f1999 = () => {\n  return a +\n    1999;\n};\n"));
}
