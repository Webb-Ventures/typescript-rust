//! Fixture tests: every file in `tests/fixtures/` is parsed and its AST dump plus diagnostics
//! are compared with `tests/snapshots/<file>.txt`.
//!
//! To accept new output: `UPDATE_SNAPSHOTS=1 cargo test -p ts_r_parser --test fixtures`.

// Real-world or generated input: abort instead of exhausting the machine's memory.
ts_r_testing::cap_memory!();

use std::path::{Path, PathBuf};

use ts_r_diagnostics::format_plain;
use ts_r_parser::ts_r_ast::SourceKind;
use ts_r_span::SourceText;

fn render(path: &Path) -> String {
    let text = std::fs::read_to_string(path).expect("read fixture");
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    let r = ts_r_parser::parse(&text, SourceKind::from_path(&name));
    let mut out = r.ast.dump_with_source(&text);
    out.push_str("---- comments ----\n");
    for c in &r.ast.comments {
        out.push_str(&format!(
            "{:?} {:?} {:?}\n",
            c.kind,
            c.span,
            c.span.text(&text)
        ));
    }
    out.push_str("---- diagnostics ----\n");
    let src = SourceText::new(name, text);
    for d in &r.diagnostics {
        out.push_str(&format_plain(d, &src));
        out.push('\n');
    }
    out
}

/// Compares `actual` with the snapshot file, or rewrites it when `UPDATE_SNAPSHOTS` is set.
/// Returns a description of the mismatch, if any.
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
    let exp_line = expected.lines().nth(first_diff).unwrap_or("<eof>");
    let act_line = actual.lines().nth(first_diff).unwrap_or("<eof>");
    Some(format!(
        "{}: first difference at line {}\n  expected: {exp_line}\n  actual:   {act_line}\n(run with UPDATE_SNAPSHOTS=1 to accept)",
        snapshot.display(),
        first_diff + 1
    ))
}

#[test]
fn fixtures_match_snapshots() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut fixtures: Vec<PathBuf> = std::fs::read_dir(root.join("fixtures"))
        .expect("fixtures dir")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    fixtures.sort();
    assert!(!fixtures.is_empty());
    let mut failures = Vec::new();
    for f in &fixtures {
        let name = f.file_name().unwrap().to_string_lossy().into_owned();
        let snap = root.join("snapshots").join(format!("{name}.txt"));
        if let Some(msg) = check_snapshot(&snap, &render(f)) {
            failures.push(msg);
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// Fixtures named without `error` must parse cleanly.
#[test]
fn valid_fixtures_have_no_diagnostics() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    for e in std::fs::read_dir(root).unwrap().flatten() {
        let p = e.path();
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        if name.contains("error") {
            continue;
        }
        let text = std::fs::read_to_string(&p).unwrap();
        let r = ts_r_parser::parse(&text, SourceKind::from_path(&name));
        assert!(r.diagnostics.is_empty(), "{name}: {:#?}", r.diagnostics);
    }
}
