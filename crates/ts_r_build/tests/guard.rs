//! `parse<T>` end to end through `ts-r-build` (record 0007): the fixture in
//! `crates/ts_r_guard/tests/fixtures/app` bundled and run in Node (under `scripts/limit.sh`)
//! must print exactly what `ts-r-run` prints (`expected.txt`); validators are shared per
//! bundle and tree-shaken; unbundled output inlines them; guard errors fail the build;
//! `--tsconfig` applies to validators too; and the minified size of the runtime and of ten API
//! types is pinned (5% gate).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

ts_r_testing::cap_memory!();

/// Minified bytes measured when the gate was set (runtime alone; runtime + ten API types).
const RUNTIME_MIN: usize = 830;
const TEN_TYPES_MIN: usize = 5557;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn app() -> PathBuf {
    repo().join("crates/ts_r_guard/tests/fixtures/app")
}

fn have_node() -> bool {
    Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn scratch(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("guard-build-{name}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("scratch dir");
    d
}

fn limited(program: &Path) -> ts_r_testing::Limited {
    ts_r_testing::limited(program, 1024, 120)
}

fn build(args: &[&str]) -> Output {
    limited(Path::new(env!("CARGO_BIN_EXE_ts-r-build")))
        .args(args)
        .arg("--no-cache")
        .current_dir(app())
        .output()
        .expect("run ts-r-build")
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[test]
fn bundle_runs_in_node_like_ts_r_run() {
    let out_dir = scratch("bundle");
    let outfile = out_dir.join("main.mjs");
    let b = build(&[
        "main.ts",
        "--bundle",
        "--platform=node",
        "--format=esm",
        &format!("--outfile={}", outfile.display()),
    ]);
    assert!(b.status.success(), "{}", text(&b.stderr));
    let bundle = std::fs::read_to_string(&outfile).expect("bundle");
    // One module per validated declaration, shared by every call site; the runtime once;
    // `ApiError` is only parsed in dead code, so its validator is gone.
    for m in [
        "types.ts/User.js",
        "types.ts/Money.js",
        "types.ts/Page.js",
        "extra.ts/Chain.js",
    ] {
        assert_eq!(
            bundle.matches(&format!("// .ts-r-guard/{m}\n")).count(),
            1,
            "{m}"
        );
    }
    assert_eq!(bundle.matches("// .ts-r-guard/rt.js\n").count(), 1);
    assert!(!bundle.contains("ApiError"));
    assert!(!bundle.contains("@ts-r.dev/guard"));
    if !have_node() {
        eprintln!("skipped running the bundle: node not found");
        return;
    }
    let run = limited(Path::new("node"))
        .arg(&outfile)
        .env("GUARD_PORT", "8080")
        .env("GUARD_DEBUG", "1")
        .env("GUARD_MODE", "dev")
        .output()
        .expect("node");
    assert!(run.status.success(), "{}", text(&run.stderr));
    let expected = std::fs::read_to_string(app().join("expected.txt")).expect("expected.txt");
    let got = text(&run.stdout);
    for (i, (g, e)) in got.lines().zip(expected.lines()).enumerate() {
        assert_eq!(g, e, "line {}", i + 1);
    }
    assert_eq!(got, expected);
    let _ = std::fs::remove_dir_all(&out_dir);
}

#[test]
fn minified_size_is_pinned() {
    let out_dir = scratch("size");
    let mut sizes = Vec::new();
    for entry in ["size-runtime", "size-all"] {
        let outfile = out_dir.join(format!("{entry}.js"));
        let b = build(&[
            &format!("{entry}.ts"),
            "--bundle",
            "--minify",
            "--format=esm",
            &format!("--outfile={}", outfile.display()),
        ]);
        assert!(b.status.success(), "{}", text(&b.stderr));
        let code = std::fs::read_to_string(&outfile).expect("output");
        assert!(
            !code.contains("ExpectError"),
            "unused runtime parts are tree-shaken"
        );
        sizes.push(code.len());
    }
    eprintln!(
        "guard sizes (minified): runtime {} B, runtime + 10 API types {} B ({} B per type, with the walker parts they use)",
        sizes[0],
        sizes[1],
        (sizes[1] - sizes[0]) / 10
    );
    assert!(
        sizes[0] * 100 <= RUNTIME_MIN * 105,
        "runtime grew: {} B",
        sizes[0]
    );
    assert!(
        sizes[1] * 100 <= TEN_TYPES_MIN * 105,
        "validators grew: {} B",
        sizes[1]
    );
    let _ = std::fs::remove_dir_all(&out_dir);
}

#[test]
fn unbundled_output_inlines_validators() {
    let out_dir = scratch("unbundled");
    let b = build(&[
        "main.ts",
        "report.ts",
        "types.ts",
        "--format=esm",
        &format!("--outdir={}", out_dir.display()),
    ]);
    assert!(b.status.success(), "{}", text(&b.stderr));
    let main = std::fs::read_to_string(out_dir.join("main.js")).expect("main.js");
    assert!(main.contains("function $parse("));
    assert!(main.contains("$is_User = ("), "{main}");
    assert!(!main.contains("@ts-r.dev/guard") && !main.contains(".ts-r-guard"));
    let report = std::fs::read_to_string(out_dir.join("report.js")).expect("report.js");
    assert!(report.contains("ParseError = $ParseError"), "{report}");
    let _ = std::fs::remove_dir_all(&out_dir);
}

#[test]
fn guard_errors_fail_the_build() {
    let out_dir = scratch("errors");
    let outfile = out_dir.join("bad.js");
    let b = build(&[
        "errors/bad.ts",
        "--bundle",
        &format!("--outfile={}", outfile.display()),
    ]);
    assert_eq!(b.status.code(), Some(1));
    let err = text(&b.stderr);
    let codes: Vec<&str> = err
        .lines()
        .filter_map(|l| l.split("ERROR: ").nth(1))
        .filter_map(|l| l.split(':').next())
        .collect();
    assert_eq!(codes, ["TSR1501", "TSR1402", "TSR1503"], "{err}");
    assert!(err.contains("errors/bad.ts:5:40:"), "{err}");
    assert!(!outfile.exists(), "no output after an error");
    let _ = std::fs::remove_dir_all(&out_dir);
}

#[test]
fn guard_expansion_uses_the_selected_tsconfig() {
    // `@t/*` is a path alias of `tsconfig.build.json` only: validators must resolve it
    // like the bundler does with `--tsconfig`.
    let dir = scratch("tsconfig");
    let files = [
        ("tsconfig.json", r#"{"compilerOptions":{"strict":true}}"#),
        (
            "tsconfig.build.json",
            r#"{"compilerOptions":{"strict":true,"baseUrl":".","paths":{"@t/*":["types/*"]}}}"#,
        ),
        ("types/user.ts", "export interface User { id: number }\n"),
        (
            "main.ts",
            "import { is } from \"@ts-r.dev/guard\";\nimport type { User } from \"@t/user\";\nconsole.log(is<User>({ id: 1 }));\n",
        ),
    ];
    for (name, text) in files {
        let p = dir.join(name);
        std::fs::create_dir_all(p.parent().expect("parent")).expect("dir");
        std::fs::write(p, text).expect("write");
    }
    let b = limited(Path::new(env!("CARGO_BIN_EXE_ts-r-build")))
        .args([
            "main.ts",
            "--bundle",
            "--tsconfig=tsconfig.build.json",
            "--outfile=out.js",
            "--no-cache",
        ])
        .current_dir(&dir)
        .output()
        .expect("run ts-r-build");
    assert!(b.status.success(), "{}", text(&b.stderr));
    let out = std::fs::read_to_string(dir.join("out.js")).expect("out.js");
    assert!(out.contains("types/user.ts/User.js"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}
