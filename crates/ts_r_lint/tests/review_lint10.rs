//! Regression tests for review findings on the lint10 stack: comment-aware fix boundaries,
//! code-span handling in `jsdoc/escape-inline-tags`, and inputs that used to cost quadratic
//! or exponential work (or memory) in otherwise ordinary rules.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use ts_r_lint::config::{ConfigFile, ConfigKind, ResolveOptions, objects_from_eslint_json};
use ts_r_lint::types::{FileKey, NoTypeInfo};
use ts_r_lint::{Registry, lint_and_fix, lint_source};

ts_r_testing::cap_memory!();

/// Generous: each input below took minutes (or tens of gigabytes) before its fix, and well
/// under a second after it, in a debug build.
const SLOW: Duration = Duration::from_secs(20);

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

/// `(line, message)` of the rule's reports.
fn reports(rule: &str, options: serde_json::Value, file: &str, code: &str) -> Vec<(usize, String)> {
    let fc = file_config(rule, options, file);
    let r = lint_source(&format!("/t/{file}"), code, &fc, &NoTypeInfo, FileKey(0));
    assert!(
        !r.diagnostics.iter().any(|d| d.fatal),
        "parse error in {code:?}"
    );
    // Line starts up front: counting newlines per report would make the helper quadratic.
    let starts: Vec<usize> = std::iter::once(0)
        .chain(code.match_indices('\n').map(|(i, _)| i + 1))
        .collect();
    r.diagnostics
        .iter()
        .filter(|d| d.rule == Some(rule))
        .map(|d| {
            (
                starts.partition_point(|&s| s <= d.span.start as usize),
                d.message.clone(),
            )
        })
        .collect()
}

fn fixed(rule: &str, options: serde_json::Value, file: &str, code: &str) -> String {
    let fc = file_config(rule, options, file);
    let r = lint_and_fix(&format!("/t/{file}"), code, &fc, &NoTypeInfo, FileKey(0));
    r.output.unwrap_or_else(|| code.to_owned())
}

fn timed<T>(what: &str, f: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let v = f();
    assert!(start.elapsed() < SLOW, "{what} took {:?}", start.elapsed());
    v
}

#[test]
fn object_shorthand_arrow_in_comment_is_not_the_arrow() {
    let opts = serde_json::json!(["always", { "avoidExplicitReturnArrows": true }]);
    let code = "({ x: () => /* => */ { return; } });\n";
    assert_eq!(
        fixed("object-shorthand", opts.clone(), "a.js", code),
        "({ x() /* => */ { return; } });\n"
    );
    let code = "({ x: (a) => // =>\n { return a; } });\n";
    assert_eq!(
        fixed("object-shorthand", opts, "a.js", code),
        "({ x(a) // =>\n { return a; } });\n"
    );
}

#[test]
fn require_atomic_updates_many_local_assignments_is_fast() {
    let mut code = String::from("async function f() {\n  let x = 0;\n");
    for _ in 0..30_000 {
        code.push_str("  x += 1;\n");
    }
    code.push_str("}\n");
    let r = timed("require-atomic-updates", || {
        reports(
            "require-atomic-updates",
            serde_json::json!([]),
            "a.js",
            &code,
        )
    });
    assert!(r.is_empty(), "{r:?}");
}

#[test]
fn no_loop_func_many_references_is_fast() {
    let mut code = String::from("let x = 0;\nfor (;;) {\n  cb(() => {\n");
    for _ in 0..30_000 {
        code.push_str("    x;\n");
    }
    code.push_str("  });\n}\n");
    let r = timed("no-loop-func", || {
        reports("no-loop-func", serde_json::json!([]), "a.js", &code)
    });
    assert!(r.is_empty(), "{r:?}");
    // The cached answer is still per binding: a written one is reported, once per function.
    let code = "let y = 0;\nfor (;;) {\n  y++;\n  cb(() => { y; y; x; });\n}\n";
    let r = reports("no-loop-func", serde_json::json!([]), "a.js", code);
    assert_eq!(r.len(), 1, "{r:?}");
    assert!(r[0].1.contains("'y'"), "{r:?}");
}

#[test]
fn escape_inline_tags_skips_lines_whose_tag_is_in_a_code_span() {
    let code = "/**\n * @param x A `some @tag` reference.\n * An unescaped @tag here.\n */\nfunction f(x) {}\n";
    let r = reports(
        "jsdoc/escape-inline-tags",
        serde_json::json!([]),
        "a.js",
        code,
    );
    assert_eq!(r.len(), 1, "{r:?}");
    assert_eq!(r[0].0, 3);
    let opts = serde_json::json!([{ "enableFixer": true }]);
    assert_eq!(
        fixed("jsdoc/escape-inline-tags", opts, "a.js", code),
        "/**\n * @param x A `some @tag` reference.\n * An unescaped \\@tag here.\n */\nfunction f(x) {}\n"
    );
}

#[test]
fn escape_inline_tags_many_tags_with_fixer_is_fast() {
    let mut code = String::from("/**\n");
    for i in 0..1_000 {
        code.push_str(&format!(" * @param p{i} Description @bad\n"));
    }
    code.push_str(" */\nfunction f() {}\n");
    let opts = serde_json::json!([{ "enableFixer": true }]);
    let r = timed("escape-inline-tags", || {
        reports("jsdoc/escape-inline-tags", opts.clone(), "a.js", &code)
    });
    assert_eq!(r.len(), 1_000);
    // A description that is one long unclosed backtick run.
    let code = format!(
        "/**\n * text @bad {}\n */\nfunction f() {{}}\n",
        "`".repeat(100_000)
    );
    let r = timed("escape-inline-tags backticks", || {
        reports(
            "jsdoc/escape-inline-tags",
            serde_json::json!([]),
            "a.js",
            &code,
        )
    });
    assert_eq!(r.len(), 1);
}

#[test]
fn no_new_array_repeated_const_operands_are_bounded() {
    let mut code = String::from("const a0 = 0;\n");
    for i in 1..=15 {
        let p = i - 1;
        code.push_str(&format!("const a{i} = (a{p}, a{p}, a{p}, a{p});\n"));
    }
    code.push_str("new Array(a15);\nnew Array('x');\n");
    let r = timed("no-new-array", || {
        reports("unicorn/no-new-array", serde_json::json!([]), "a.js", &code)
    });
    assert_eq!(r.len(), 2);
    // A known value is still known: the string argument is fixed, the deep one is not.
    let out = fixed("unicorn/no-new-array", serde_json::json!([]), "a.js", &code);
    assert!(out.ends_with("new Array(a15);\n['x'];\n"), "{out}");
}

#[test]
fn new_for_builtins_many_globals_is_fast() {
    let code = "new Array();\n".repeat(60_000);
    let r = timed("new-for-builtins", || {
        reports(
            "unicorn/new-for-builtins",
            serde_json::json!([]),
            "a.js",
            &code,
        )
    });
    assert!(r.is_empty());
    // A written global is still not the built-in.
    let code = "Array = function () {};\nconst a = Array();\nconst m = Map();\n";
    let r = reports(
        "unicorn/new-for-builtins",
        serde_json::json!([]),
        "a.js",
        code,
    );
    assert_eq!(r.len(), 1, "{r:?}");
    assert_eq!(r[0].0, 3);
}

#[test]
fn empty_brace_spaces_brace_in_line_comment() {
    for code in [
        "function f() { // {\n}\n",
        "class A { // {\n}\n",
        "const o = { /* { */ };\n",
    ] {
        let r = reports(
            "unicorn/empty-brace-spaces",
            serde_json::json!([]),
            "a.js",
            code,
        );
        assert!(r.is_empty(), "{code:?}: {r:?}");
    }
    // A comment before the brace does not hide the gap.
    let code = "class A /* c */ {  }\n";
    assert_eq!(
        fixed(
            "unicorn/empty-brace-spaces",
            serde_json::json!([]),
            "a.js",
            code
        ),
        "class A /* c */ {}\n"
    );
}

#[test]
fn global_object_property_assignment_spread_growth_is_bounded() {
    let mut code = String::from("const a0 = [0];\n");
    for i in 1..=9 {
        let p = i - 1;
        let spreads = vec![format!("...a{p}"); 10].join(", ");
        code.push_str(&format!("const a{i} = [{spreads}];\n"));
    }
    code.push_str("globalThis[a9] = 1;\n");
    timed("no-global-object-property-assignment", || {
        reports(
            "unicorn/no-global-object-property-assignment",
            serde_json::json!([]),
            "a.js",
            &code,
        )
    });
    // Doubling strings through a template.
    let mut code = String::from("const s0 = 'xxxxxxxxxxxxxxxx'.repeat(10000);\n");
    for i in 1..=40 {
        let p = i - 1;
        code.push_str(&format!("const s{i} = `${{s{p}}}${{s{p}}}`;\n"));
    }
    code.push_str("globalThis[s40] = 1;\n");
    timed("no-global-object-property-assignment strings", || {
        reports(
            "unicorn/no-global-object-property-assignment",
            serde_json::json!([]),
            "a.js",
            &code,
        )
    });
}

#[test]
fn restricted_syntax_sibling_selector_is_linear() {
    let code = "var a;\n".repeat(30_000);
    let opts = serde_json::json!(["VariableDeclaration ~ VariableDeclaration"]);
    let r = timed("no-restricted-syntax", || {
        reports("no-restricted-syntax", opts, "a.js", &code)
    });
    assert_eq!(r.len(), 29_999);
    let opts = serde_json::json!(["VariableDeclaration + VariableDeclaration"]);
    assert_eq!(
        reports(
            "no-restricted-syntax",
            opts,
            "a.js",
            "var a;\nvar b;\nvar c;\n"
        )
        .len(),
        2
    );
}

#[test]
fn restricted_syntax_long_combinator_chain_is_a_config_error() {
    let chain = vec!["*"; 200_000].join(" > ");
    let config = serde_json::json!([{
        "files": ["**/*.js"],
        "rules": { "no-restricted-syntax": ["error", chain] },
    }]);
    // Whatever the loader makes of it, it must not overflow the stack.
    if let Ok(objs) = objects_from_eslint_json(&config, Registry::global()) {
        let cfg = ConfigFile::new(ConfigKind::Eslint, None, PathBuf::from("/t"), objs);
        if let Ok(fc) = cfg.file_config("a.js", &ResolveOptions::new()) {
            let _ = lint_source("/t/a.js", "x;\n", &fc, &NoTypeInfo, FileKey(0));
        }
    }
}

#[test]
fn jsdoc_deep_array_suffixes_do_not_overflow() {
    let code = format!("/** @type {{Number{}}} */\nlet x;\n", "[]".repeat(100_000));
    for (rule, opts) in [
        ("jsdoc/check-types", serde_json::json!([])),
        ("jsdoc/valid-types", serde_json::json!([])),
        (
            "jsdoc/require-returns",
            serde_json::json!([{ "contexts": [{ "comment": "JsdocBlock" }] }]),
        ),
    ] {
        let fc = file_config(rule, opts.clone(), "a.js");
        let _ = lint_source("/t/a.js", &code, &fc, &NoTypeInfo, FileKey(0));
        let _ = fixed(rule, opts, "a.js", &code);
    }
}

#[test]
fn prefer_string_slice_wide_escaped_character_in_jsdoc_type() {
    let code = "/** @type {'\\é'} */\nconst text = 'é';\ntext.substring(0);\n";
    let r = reports(
        "unicorn/prefer-string-slice",
        serde_json::json!([]),
        "a.js",
        code,
    );
    assert_eq!(r.len(), 1, "{r:?}");
}
