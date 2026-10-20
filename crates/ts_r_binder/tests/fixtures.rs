//! Fixture tests: every file in `tests/fixtures/` is parsed and bound (with
//! `allowUnreachableCode: false` and `allowUnusedLabels: false`), and the binder dump is
//! compared with `tests/snapshots/<file>.txt`. Files named `flow*` / `unreachable*` also
//! get the flow-graph dump. The diagnostics in every snapshot were checked against
//! tsc 7.0.2 (see AGENTS.md for the exceptions, which are program-level or checker codes).
//!
//! To accept new output: `UPDATE_SNAPSHOTS=1 cargo test -p ts_r_binder --test fixtures`.

ts_r_testing::cap_memory!();

use std::path::{Path, PathBuf};

use ts_r_ast::SourceKind;
use ts_r_binder::{BindOptions, bind};

fn render(path: &Path) -> String {
    let src = std::fs::read_to_string(path).expect("read fixture");
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    let parsed = ts_r_parser::parse(&src, SourceKind::from_path(&name));
    let opts = BindOptions {
        allow_unreachable_code: Some(false),
        allow_unused_labels: Some(false),
        ..BindOptions::default()
    };
    let bound = bind(&parsed.ast, &src, &opts);
    let mut out = String::new();
    for d in &parsed.diagnostics {
        out.push_str(&format!("parse: TS{} {}\n", d.code, d.message));
    }
    out.push_str(&bound.dump(&parsed.ast, &src));
    if name.starts_with("flow") || name.starts_with("unreachable") {
        out.push_str("---- flow ----\n");
        out.push_str(&bound.dump_flow(&parsed.ast, &src));
    }
    out
}

fn check_snapshot(snapshot: &Path, actual: &str) -> Option<String> {
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
        std::fs::write(snapshot, actual).expect("write snapshot");
        return None;
    }
    let expected = std::fs::read_to_string(snapshot).unwrap_or_default();
    if expected == actual {
        return None;
    }
    let first_diff = expected
        .lines()
        .zip(actual.lines())
        .position(|(a, b)| a != b)
        .unwrap_or_else(|| expected.lines().count().min(actual.lines().count()));
    Some(format!(
        "{}: first difference at line {}\n  expected: {}\n  actual:   {}\n(run with UPDATE_SNAPSHOTS=1 to accept)",
        snapshot.display(),
        first_diff + 1,
        expected.lines().nth(first_diff).unwrap_or("<eof>"),
        actual.lines().nth(first_diff).unwrap_or("<eof>"),
    ))
}

#[test]
fn fixtures() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir.join("fixtures"))
        .expect("fixtures dir")
        .map(|e| e.expect("entry").path())
        .collect();
    files.sort();
    assert!(!files.is_empty());
    let mut failures = Vec::new();
    for f in &files {
        let name = f.file_name().unwrap().to_string_lossy().into_owned();
        let snap = dir.join("snapshots").join(format!("{name}.txt"));
        if let Some(msg) = check_snapshot(&snap, &render(f)) {
            failures.push(msg);
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
