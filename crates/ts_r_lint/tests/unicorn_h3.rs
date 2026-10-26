//! Regression tests for round-6 unicorn rules, for forms their oracle data does not cover
//! (the ESLint core behaviour `unicorn/operator-assignment` and
//! `unicorn/logical-assignment-operators` wrap).

use std::path::PathBuf;

use ts_r_lint::config::{ConfigFile, ConfigKind, ResolveOptions, objects_from_eslint_json};
use ts_r_lint::types::{FileKey, NoTypeInfo};
use ts_r_lint::{Registry, lint_and_fix, lint_source};

ts_r_testing::cap_memory!();

fn file_config(rule: &str, options: serde_json::Value) -> std::sync::Arc<ts_r_lint::FileConfig> {
    let mut entry = vec![serde_json::json!("error")];
    if let serde_json::Value::Array(o) = options {
        entry.extend(o);
    }
    let config = serde_json::json!([{ "rules": { rule: entry } }]);
    let objs = objects_from_eslint_json(&config, Registry::global()).expect("config");
    let cfg = ConfigFile::new(ConfigKind::Eslint, None, PathBuf::from("/t"), objs);
    cfg.file_config("a.js", &ResolveOptions::new())
        .expect("file config")
}

/// The fixed text (all passes) and the messages left on it.
fn fixed(rule: &str, options: serde_json::Value, code: &str) -> String {
    let fc = file_config(rule, options);
    let r = lint_and_fix("/t/a.js", code, &fc, &NoTypeInfo, FileKey(0));
    r.output.unwrap_or_else(|| code.to_owned())
}

/// The messages and suggestion outputs.
fn report(rule: &str, options: serde_json::Value, code: &str) -> Vec<String> {
    let fc = file_config(rule, options);
    let r = lint_source("/t/a.js", code, &fc, &NoTypeInfo, FileKey(0));
    let mut out = Vec::new();
    for d in &r.diagnostics {
        out.push(d.message.clone());
        for s in &d.suggestions {
            let mut text = code.to_owned();
            let mut edits = s.fix.edits.clone();
            edits.sort_by_key(|e| std::cmp::Reverse(e.span.start));
            for e in edits {
                text.replace_range(e.span.start as usize..e.span.end as usize, &e.text);
            }
            out.push(format!("  {}: {text}", s.desc));
        }
    }
    out
}

#[test]
fn operator_assignment_core_forms() {
    let r = "unicorn/operator-assignment";
    let none = serde_json::json!([]);
    assert_eq!(fixed(r, none.clone(), "a = a * b;"), "a *= b;");
    assert_eq!(fixed(r, none.clone(), "this.x = this.x - 1;"), "this.x -= 1;");
    // Commutative, operand on the right: reported, not fixed.
    assert_eq!(fixed(r, none.clone(), "a = b * a;"), "a = b * a;");
    assert_eq!(report(r, none.clone(), "a = b * a;").len(), 1);
    // Not commutative on the right, and `a.b` vs `a["b"]` differ.
    assert!(report(r, none.clone(), "a = b - a;").is_empty());
    assert!(report(r, none.clone(), "a.b = a['b'] + 1;").is_empty());
    // A computed target that is not static: reported without a fix.
    assert_eq!(fixed(r, none, "a[i] = a[i] + 1;"), "a[i] = a[i] + 1;");
    let never = serde_json::json!(["never"]);
    assert_eq!(fixed(r, never.clone(), "a *= b + c;"), "a = a * (b + c);");
    assert_eq!(fixed(r, never.clone(), "a -= -b;"), "a = a - -b;");
    assert_eq!(fixed(r, never.clone(), "a-=-b;"), "a= a- -b;");
    assert!(report(r, never, "a ||= b;").is_empty());
}

#[test]
fn logical_assignment_core_forms() {
    let r = "unicorn/logical-assignment-operators";
    let none = serde_json::json!([]);
    assert_eq!(fixed(r, none.clone(), "a || (a = b);"), "a ||= b;");
    assert_eq!(fixed(r, none.clone(), "a ?? (a = b);"), "a ??= b;");
    assert_eq!(
        fixed(r, none.clone(), "const x = a && (a = b);"),
        "const x = (a &&= b);"
    );
    // A property target is only suggested.
    assert_eq!(fixed(r, none.clone(), "o.p = o.p || q;"), "o.p = o.p || q;");
    assert_eq!(
        report(r, none.clone(), "o.p = o.p || q;"),
        [
            "Assignment (=) can be replaced with operator assignment (||=).",
            "  Convert this assignment to use the operator ||=.: o.p ||= q;"
        ]
    );
    let ifs = serde_json::json!(["always", { "enforceForIfStatements": true }]);
    assert_eq!(fixed(r, ifs.clone(), "if (a == null) a = b;"), "a ??= b;");
    assert_eq!(
        fixed(r, ifs.clone(), "if (a === undefined || a === null) { a = b }"),
        "a ??= b;"
    );
    // A property behind a logical test is only suggested.
    let code = "if (o.p === undefined || o.p === null) { o.p = b }";
    assert_eq!(fixed(r, ifs.clone(), code), code);
    assert_eq!(
        report(r, ifs.clone(), code),
        [
            "'if' statement can be replaced with a logical operator assignment with operator ??=.",
            "  Replace this 'if' statement with a logical assignment with operator ??=.: o.p ??= b;"
        ]
    );
    assert_eq!(fixed(r, ifs.clone(), "if (o.p) { o.p = q; }"), "o.p &&= q;");
    // `if` with an `else`, or more than one statement, is left alone.
    assert!(report(r, ifs.clone(), "if (a) a = b; else c();").is_empty());
    assert!(report(r, ifs, "if (a) { a = b; c(); }").is_empty());
    let never = serde_json::json!(["never"]);
    assert_eq!(fixed(r, never.clone(), "a ??= b || c;"), "a = a ?? (b || c);");
    assert_eq!(fixed(r, never.clone(), "a &&= b || c;"), "a = a && (b || c);");
    assert_eq!(fixed(r, never, "a ||= b && c;"), "a = a || b && c;");
}
