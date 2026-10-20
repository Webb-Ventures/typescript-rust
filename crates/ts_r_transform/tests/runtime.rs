//! Lowered output behaves like the original: each `tests/runtime/<name>.ts` is transpiled for
//! ESNext and for every lowered target, Node runs each output (under `ts_r_testing::limited`),
//! and all must print the same. Skipped when `node` is not installed. When this Node cannot
//! parse the ESNext output (Node 22 has no `using`), the lowered targets are compared with each
//! other instead.

use std::path::{Path, PathBuf};
use std::process::Command;

use ts_r_codegen::CodegenOptions;
use ts_r_transform::{TransformOptions, transform, transpile};

const TARGETS: &[&str] = &[
    "es2015", "es2016", "es2017", "es2018", "es2019", "es2020", "es2021",
];

fn node_available() -> bool {
    Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Runs `file` with Node under the repository's resource limits; returns stdout, or stderr
/// as the error.
fn try_node(file: &Path) -> Result<String, String> {
    let out = ts_r_testing::limited("node", 512, 60)
        .arg(file)
        .output()
        .expect("node runs under the resource limits");
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).into_owned())
    }
}

fn run_node(file: &Path) -> String {
    try_node(file).unwrap_or_else(|e| panic!("{} failed:\n{e}", file.display()))
}

fn check(name: &str) {
    if !node_available() {
        eprintln!("runtime/{name}: node not found, skipped");
        return;
    }
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/runtime/{name}.ts"));
    let src = std::fs::read_to_string(&path).unwrap();
    let path = path.to_string_lossy().to_string();
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("runtime");
    std::fs::create_dir_all(&dir).unwrap();
    let emit = |target: &str| -> PathBuf {
        let opts = TransformOptions::from_json(
            &format!(r#"{{"target":"{target}","module":"esnext"}}"#),
            &path,
        );
        // Everything must be lowered (no TSR2003 refusal).
        let mut parsed = ts_r_parser::parse(&src, ts_r_ast::SourceKind::from_path(&path));
        let r = transform(&mut parsed.ast, &src, &opts);
        assert!(
            r.unlowered.is_empty(),
            "{name} at {target}: not lowered: {:?}",
            r.unlowered
        );
        let code = transpile(&path, &src, &opts, &CodegenOptions::default()).code;
        let file = dir.join(format!("{name}.{target}.mjs"));
        std::fs::write(&file, code).unwrap();
        file
    };
    let (want, targets) = match try_node(&emit("esnext")) {
        Ok(out) => (out, TARGETS),
        // This Node predates some ESNext syntax: the newest lowered target is the reference.
        Err(e) if e.contains("SyntaxError") => {
            let (last, rest) = TARGETS.split_last().expect("targets");
            eprintln!("runtime/{name}: node cannot run the ESNext output; comparing with {last}");
            (run_node(&emit(last)), rest)
        }
        Err(e) => panic!("{name} at esnext failed:\n{e}"),
    };
    assert!(
        !want.trim().is_empty(),
        "{name}: the original prints nothing"
    );
    for t in targets {
        let got = run_node(&emit(t));
        assert_eq!(got, want, "{name}: output at {t} differs from ESNext");
    }
}

#[test]
fn operators() {
    check("operators");
}

#[test]
fn async_and_rest() {
    check("async");
}

#[test]
fn classes() {
    check("classes");
}

#[test]
fn using() {
    check("using");
}

#[test]
fn private_names() {
    check("private");
}
