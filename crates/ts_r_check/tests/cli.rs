//! Runs `ts-r-check` on the projects under `tests/fixtures/` and compares its output and exit
//! code with stored tsc 7 output.
//!
//! Each fixture directory is a project with a `tsconfig.json`. `expected/plain.txt` and
//! `expected/pretty.txt` hold `tsc -p . --pretty false` / `--pretty true` output (the
//! fixture's absolute path stored as `${FIXTURE}`), `expected/exit.txt` tsc's exit code.
//! Fixtures whose program is checked need the TypeScript lib files
//! (`ts_r_checker::LibStore::locate`); without them those fixtures are skipped. Regenerate
//! with `TS_R_TSC=<tsc> cargo test -p ts_r_check --test cli -- --ignored regenerate`.

use std::path::{Path, PathBuf};
use std::process::Command;

ts_r_testing::cap_memory!();

const PLACEHOLDER: &str = "${FIXTURE}";

/// Fixtures whose program is checked (no option error stops it), so they need the libs.
const NEEDS_LIBS: &[&str] = &["config-parse-errors-sorted", "js-type-syntax"];

fn fixtures() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("tsconfig.json").is_file())
        .collect();
    out.sort();
    out
}

fn run(cmd: &mut Command, dir: &Path) -> (String, i32) {
    let out = cmd.current_dir(dir).output().expect("run");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let fixture = dir.to_string_lossy().replace('\\', "/");
    (
        text.replace(&fixture, PLACEHOLDER),
        out.status.code().unwrap_or(-1),
    )
}

/// A fresh copy of fixture `dir` (top-level files and directories, recursively; symbolic
/// links kept as links) under `CARGO_TARGET_TMPDIR`: ts-r-check and tsc emit, and must never
/// write into the source tree.
fn copy_fixture(dir: &Path, tag: &str) -> PathBuf {
    let name = dir.file_name().unwrap().to_string_lossy().into_owned();
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("fixture-{tag}"));
    let copy = root.join(&name);
    let _ = std::fs::remove_dir_all(&copy);
    std::fs::create_dir_all(&copy).unwrap();
    copy_tree(dir, &copy);
    ts_r_testing::canonical(&copy).unwrap()
}

fn copy_tree(from: &Path, to: &Path) {
    for e in std::fs::read_dir(from).unwrap().flatten() {
        let (src, dst) = (e.path(), to.join(e.file_name()));
        if e.file_name() == "expected" {
            continue;
        }
        let ty = e.file_type().unwrap();
        if ty.is_symlink() {
            #[cfg(unix)]
            std::os::unix::fs::symlink(std::fs::read_link(&src).unwrap(), &dst).unwrap();
        } else if ty.is_dir() {
            std::fs::create_dir_all(&dst).unwrap();
            copy_tree(&src, &dst);
        } else {
            std::fs::copy(&src, &dst).unwrap();
        }
    }
}

fn ts_r_check(dir: &Path, pretty: bool) -> (String, i32) {
    run(
        Command::new(env!("CARGO_BIN_EXE_ts-r-check")).args([
            "-p",
            ".",
            "--pretty",
            if pretty { "true" } else { "false" },
            "--no-history",
            "--sound=off",
        ]),
        dir,
    )
}

#[test]
fn fixtures_match_tsc() {
    let have_libs = ts_r_checker::LibStore::locate().is_some();
    let mut failures = Vec::new();
    for dir in fixtures() {
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        if !have_libs && NEEDS_LIBS.contains(&name.as_str()) {
            eprintln!("skipping {name}: no lib files");
            continue;
        }
        let read = |f: &str| std::fs::read_to_string(dir.join("expected").join(f)).unwrap();
        let exit: i32 = read("exit.txt").trim().parse().unwrap();
        for (pretty, file) in [(false, "plain.txt"), (true, "pretty.txt")] {
            let want = read(file);
            let copy = copy_fixture(&dir, "check");
            let (got, code) = ts_r_check(&copy, pretty);
            let _ = std::fs::remove_dir_all(&copy);
            if got != want || code != exit {
                failures.push(format!(
                    "{name} ({file}): exit {code}, want {exit}\n--- tsc\n{want}\n--- ts-r-check\n{got}"
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// A command-line `--noEmit` counts for TS5096 like one in the config (tsc 7.0.2: `tsc -p .
/// --noEmit` on trpc's config checks; without it, TS5096 stops the check).
#[test]
fn command_line_no_emit_allows_ts_extensions() {
    if ts_r_checker::LibStore::locate().is_none() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("ts-r-check-noemit-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let write = |f: &str, s: &str| std::fs::write(dir.join(f), s).unwrap();
    write(
        "a.ts",
        "import { b } from \"./b.ts\";\nexport const a: number = b;\n",
    );
    write("b.ts", "export const b = 1;\n");
    write(
        "tsconfig.json",
        "{ \"compilerOptions\": { \"strict\": true, \"module\": \"nodenext\", \"allowImportingTsExtensions\": true } }\n",
    );
    let check = |extra: &[&str]| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_ts-r-check"));
        cmd.args([
            "-p",
            ".",
            "--pretty",
            "false",
            "--no-history",
            "--sound=off",
        ])
        .args(extra);
        run(&mut cmd, &dir)
    };
    let (out, code) = check(&["--noEmit"]);
    assert_eq!((out.as_str(), code), ("", 0));
    let (out, code) = check(&[]);
    assert!(out.contains("error TS5096"), "{out}");
    // An option error does not stop emit: tsc writes the outputs and exits 2.
    assert_eq!(code, 2);
    assert!(dir.join("a.js").is_file() && dir.join("b.js").is_file());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A command-line `--noEmit` counts for the option checks like one in the config (tsc 7:
/// `tsc -p . --noEmit` reports neither TS5055 for `allowJs` without `outDir` nor TS5096 for
/// `allowImportingTsExtensions`, and checks the program).
#[test]
fn command_line_no_emit_counts_for_option_checks() {
    if ts_r_checker::LibStore::locate().is_none() {
        return;
    }
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("ts-r-check-cli-noemit");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let write = |f: &str, s: &str| std::fs::write(dir.join(f), s).unwrap();
    write(
        "a.ts",
        "import { b } from \"./b.ts\";\nexport const a: string = b;\n",
    );
    write("b.ts", "export const b = 1;\n");
    write("c.js", "export const c = 1;\n");
    write(
        "tsconfig.json",
        "{ \"compilerOptions\": { \"strict\": true, \"module\": \"nodenext\", \"allowImportingTsExtensions\": true, \"allowJs\": true } }\n",
    );
    let check = |extra: &[&str]| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_ts-r-check"));
        cmd.args([
            "-p",
            ".",
            "--pretty",
            "false",
            "--no-history",
            "--sound=off",
        ])
        .args(extra);
        run(&mut cmd, &dir)
    };
    let (out, code) = check(&["--noEmit"]);
    assert_eq!(
        (out.as_str(), code),
        (
            "a.ts(2,14): error TS2322: Type 'number' is not assignable to type 'string'.\n",
            1
        )
    );
    let (out, _) = check(&[]);
    assert!(
        out.contains("error TS5055") && out.contains("error TS5096"),
        "{out}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A temporary project: `files` written under a fresh directory in `CARGO_TARGET_TMPDIR`.
fn temp_project(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (f, text) in files {
        let path = dir.join(f);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    ts_r_testing::canonical(&dir).unwrap()
}

fn ts_r_check_args(dir: &Path, args: &[&str]) -> (String, i32) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ts-r-check"));
    cmd.args(["--no-history", "--sound=off"]).args(args);
    run(&mut cmd, dir)
}

#[test]
fn command_line_options_control_discovery() {
    let dir = temp_project(
        "ts-r-check-cli-discovery",
        &[
            ("tsconfig.json", "{}\n"),
            ("a.ts", "export {};\n"),
            ("bad.js", "const x: number = 'oops';\n"),
            ("dist/old.ts", "export {};\n"),
        ],
    );
    let (out, code) = ts_r_check_args(
        &dir,
        &["--showConfig", "--allowJs", "--checkJs", "--outDir", "dist"],
    );
    assert_eq!(code, 0);
    assert!(out.contains("\"./bad.js\""), "{out}");
    assert!(!out.contains("\"./dist/old.ts\""), "{out}");
    std::fs::remove_file(dir.join("a.ts")).unwrap();
    std::fs::remove_file(dir.join("dist/old.ts")).unwrap();
    let (out, code) = ts_r_check_args(&dir, &["--showConfig", "--allowJs"]);
    assert_eq!(code, 0);
    assert!(out.contains("\"./bad.js\""), "{out}");
    assert!(!out.contains("TS18003"), "{out}");
    let (out, code) = ts_r_check_args(
        &dir,
        &["--pretty", "false", "--allowJs", "--checkJs", "--noEmit"],
    );
    assert_eq!(code, 1);
    assert!(out.contains("bad.js(1,10): error TS8010"), "{out}");
    assert!(!out.contains("TS18003"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// `-v` prints tsc's `Version x.y.z` line plus the ts-r version, and wins over `--help`.
#[test]
fn version_and_help() {
    let dir = temp_project("ts-r-check-cli-version", &[("tsconfig.json", "{}\n")]);
    let want = format!("Version 7.0.2 (ts-r {})\n", env!("CARGO_PKG_VERSION"));
    for args in [
        &["-v"][..],
        &["--version"],
        &["-V", "--help"],
        &["--all", "-v"],
    ] {
        assert_eq!(ts_r_check_args(&dir, args), (want.clone(), 0), "{args:?}");
    }
    let (help, code) = ts_r_check_args(&dir, &["--help"]);
    assert_eq!(code, 0);
    assert!(
        help.contains("--project, -p") && help.contains("--threads N"),
        "{help}"
    );
    let (all, code) = ts_r_check_args(&dir, &["--all"]);
    assert_eq!(code, 0);
    for f in [
        "--stopBuildOnErrors",
        "--watchInterval",
        "--isolatedDeclarations",
        "\nEmit:\n",
    ] {
        assert!(all.contains(f), "--all lists {f}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Command-line errors print like tsc's (plain and pretty, in argument order) and stop
/// everything else with exit 1.
#[test]
fn command_line_errors() {
    let dir = temp_project("ts-r-check-cli-errors", &[("tsconfig.json", "{}\n")]);
    let (out, code) = ts_r_check_args(
        &dir,
        &[
            "--pretty",
            "false",
            "--foo",
            "--target=es2020",
            "--module",
            "x",
            "-v",
        ],
    );
    assert_eq!(code, 1);
    assert_eq!(
        out,
        "error TS5023: Unknown compiler option '--foo'.\n\
         error TS5023: Unknown compiler option '--target=es2020'.\n\
         error TS6046: Argument for '--module' option must be: 'commonjs', 'es6', 'es2015', 'es2020', 'es2022', 'esnext', 'node16', 'node18', 'node20', 'nodenext', 'preserve'.\n"
    );
    let (out, code) = ts_r_check_args(&dir, &["--pretty", "true", "--foo"]);
    assert_eq!(code, 1);
    assert_eq!(
        out,
        "\u{1b}[91merror\u{1b}[0m\u{1b}[90m TS5023: \u{1b}[0mUnknown compiler option '--foo'.\n"
    );
    let (out, code) = ts_r_check_args(&dir, &["--pretty", "false", "-p", "nope"]);
    assert_eq!(code, 1);
    assert_eq!(
        out,
        "error TS5058: The specified path does not exist: '${FIXTURE}/nope'.\n"
    );
    std::fs::create_dir_all(dir.join("empty")).unwrap();
    let (out, _) = ts_r_check_args(&dir, &["--pretty", "false", "-p", "empty"]);
    assert_eq!(
        out,
        "error TS5081: Cannot find a tsconfig.json file at the current directory: ${FIXTURE}/empty/tsconfig.json.\n"
    );
    let (out, code) = ts_r_check_args(&dir, &["--pretty", "false", "a.ts"]);
    assert_eq!(code, 1);
    assert!(
        out.starts_with("error TS5112: tsconfig.json is present"),
        "{out}"
    );
    let (out, _) = ts_r_check_args(&dir, &["--pretty", "false", "-p", ".", "a.ts"]);
    assert!(out.starts_with("error TS5042: "), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// `--build` (record 0012, E3): status lines without colour are `HH:MM:SS AM - message`; an
/// incremental project is up to date on the second run and its outputs are left alone.
#[test]
fn build_mode() {
    let dir = temp_project(
        "ts-r-check-cli-build",
        &[
            ("a.ts", "export const a = 1;\n"),
            (
                "tsconfig.json",
                r#"{"compilerOptions":{"incremental":true}}"#,
            ),
        ],
    );
    // Drop the clock (`05:39:25 PM - `) from status lines.
    let untimed = |out: &str| -> String {
        out.split('\n')
            .map(|l| match l.split_once(" - ") {
                Some((time, rest)) if time.ends_with('M') && time.len() == 11 => rest,
                _ => l,
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let (out, code) = ts_r_check_args(&dir, &["-b", "--pretty", "false", "--verbose"]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(
        untimed(&out),
        "Projects in this build: \r\n    * tsconfig.json\n\n\
         Project 'tsconfig.json' is out of date because output file 'tsconfig.tsbuildinfo' does not exist\n\n\
         Building project 'tsconfig.json'...\n\n"
    );
    assert!(dir.join("a.js").exists() && dir.join("tsconfig.tsbuildinfo").exists());
    let (out, code) = ts_r_check_args(&dir, &["-b", "--pretty", "false", "--verbose"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("Project 'tsconfig.json' is up to date because newest input 'a.ts' is older than output 'tsconfig.tsbuildinfo'"),
        "{out}"
    );
    assert!(!out.contains("Building project"), "{out}");
    let (out, code) = ts_r_check_args(&dir, &["-b", "--clean"]);
    assert_eq!(code, 0, "{out}");
    assert!(!dir.join("a.js").exists() && !dir.join("tsconfig.tsbuildinfo").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn regression_buildinfo_protects_inputs() {
    for no_emit in [false, true] {
        for link in [false, true] {
            for mode in ["-p", "-b"] {
                let source = "export const a = 1;\n";
                let buildinfo = if link { "state.tsbuildinfo" } else { "a.ts" };
                let config = format!(
                    r#"{{"compilerOptions":{{"incremental":true,"target":"es2022","noCheck":true,"noEmit":{no_emit},"tsBuildInfoFile":"./{buildinfo}"}},"files":["a.ts"]}}"#
                );
                let dir = temp_project(
                    "ts-r-check-buildinfo-input",
                    &[("a.ts", source), ("tsconfig.json", &config)],
                );
                if link {
                    #[cfg(unix)]
                    std::os::unix::fs::symlink("a.ts", dir.join(buildinfo)).unwrap();
                    #[cfg(not(unix))]
                    continue;
                }
                let (out, code) = ts_r_check_args(&dir, &[mode, ".", "--pretty", "false"]);
                assert_eq!(std::fs::read_to_string(dir.join("a.ts")).unwrap(), source);
                assert_eq!(code, 1, "{out}");
                assert!(out.contains("error TS5033"), "{out}");
                let _ = std::fs::remove_dir_all(&dir);
            }
        }
    }
}

#[test]
fn regression_failed_emit_is_retried() {
    let dir = temp_project(
        "ts-r-check-failed-emit",
        &[
            ("a.ts", "export const a = 1;\n"),
            (
                "tsconfig.json",
                r#"{"compilerOptions":{"incremental":true,"target":"es2022","noCheck":true,"removeComments":true,"outDir":"dist"},"files":["a.ts"]}"#,
            ),
        ],
    );
    let args = ["-b", "--pretty", "false"];
    for _ in 0..2 {
        std::fs::create_dir_all(dir.join("dist/a.js")).unwrap();
        let (out, code) = ts_r_check_args(&dir, &args);
        assert_eq!(code, 1, "{out}");
        assert!(out.contains("error TS5033"), "{out}");
        assert!(dir.join("dist/tsconfig.tsbuildinfo").is_file());
        std::fs::remove_dir(dir.join("dist/a.js")).unwrap();
        let (out, code) = ts_r_check_args(&dir, &args);
        assert_eq!(code, 0, "{out}");
        assert_eq!(
            std::fs::read_to_string(dir.join("dist/a.js")).unwrap(),
            "export const a = 1;\n"
        );
        std::fs::remove_file(dir.join("dist/a.js")).unwrap();
        // Force a build with existing state and an output whose bytes differ on disk. The edit
        // can land in the same timestamp tick as the buildinfo just written, and an input no
        // newer than the output is up to date (as in tsc), so date it after the buildinfo.
        std::fs::write(dir.join("a.ts"), "export const a = 1;\n// changed\n").unwrap();
        let built = std::fs::metadata(dir.join("dist/tsconfig.tsbuildinfo"))
            .unwrap()
            .modified()
            .unwrap();
        std::fs::File::options()
            .write(true)
            .open(dir.join("a.ts"))
            .unwrap()
            .set_modified(built + std::time::Duration::from_secs(2))
            .unwrap();
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn regression_command_line_emit_uses_common_source_dir() {
    let dir = temp_project(
        "ts-r-check-command-line-emit",
        &[("src/a.ts", "export const a = 1;\n")],
    );
    for no_emit_on_error in ["false", "true"] {
        let (out, code) = ts_r_check_args(
            &dir,
            &[
                "src/a.ts",
                "--outDir",
                "dist",
                "--target",
                "es2022",
                "--noCheck",
                "--noEmitOnError",
                no_emit_on_error,
                "--pretty",
                "false",
            ],
        );
        assert_eq!(code, 0, "{out}");
        assert!(out.is_empty(), "{out}");
        assert!(dir.join("dist/a.js").is_file());
        assert!(!dir.join("dist/src/a.js").exists());
        std::fs::remove_dir_all(dir.join("dist")).unwrap();
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn regression_no_check_keeps_changed_options_pending() {
    let config = |strict| {
        format!(
            r#"{{"compilerOptions":{{"incremental":true,"target":"es2022","noLib":true,"strict":{strict}}},"files":["a.ts","lib.d.ts"]}}"#
        )
    };
    let dir = temp_project(
        "ts-r-check-pending-options",
        &[
            ("a.ts", "export function f(x) {}\n"),
            ("tsconfig.json", &config(false)),
            (
                "lib.d.ts",
                "interface Array<T> {}\ninterface Boolean {}\ninterface Function {}\ninterface CallableFunction {}\ninterface NewableFunction {}\ninterface IArguments {}\ninterface Number {}\ninterface Object {}\ninterface RegExp {}\ninterface String {}\n",
            ),
        ],
    );
    let args = ["-b", "--pretty", "false"];
    let (out, code) = ts_r_check_args(&dir, &args);
    assert_eq!(code, 0, "{out}");
    std::fs::write(dir.join("tsconfig.json"), config(true)).unwrap();
    let (out, code) = ts_r_check_args(&dir, &["-b", "--pretty", "false", "--noCheck"]);
    assert_eq!(code, 0, "{out}");
    let (out, code) = ts_r_check_args(&dir, &args);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("error TS7006"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn regression_unchanged_config_build_advances_buildinfo_time() {
    for extended in [false, true] {
        let config = format!(
            r#"{{"compilerOptions":{{"incremental":true,"target":"es2022","noCheck":true}},"files":["a.ts"]{}}}"#,
            if extended {
                r#", "extends":"./base.json""#
            } else {
                ""
            }
        );
        let dir = temp_project(
            "ts-r-check-config-time",
            &[
                ("a.ts", "export const a = 1;\n"),
                ("tsconfig.json", &config),
                ("base.json", "{}"),
            ],
        );
        let args = ["-b", "--pretty", "false", "--verbose"];
        let (out, code) = ts_r_check_args(&dir, &args);
        assert_eq!(code, 0, "{out}");
        let buildinfo = dir.join("tsconfig.tsbuildinfo");
        let text = std::fs::read(&buildinfo).unwrap();
        let now = std::time::SystemTime::now();
        std::fs::File::options()
            .write(true)
            .open(&buildinfo)
            .unwrap()
            .set_modified(now - std::time::Duration::from_secs(10))
            .unwrap();
        let config_path = dir.join(if extended {
            "base.json"
        } else {
            "tsconfig.json"
        });
        let mut text_with_comment = std::fs::read_to_string(&config_path).unwrap();
        text_with_comment.push_str("\n// comment\n");
        std::fs::write(&config_path, text_with_comment).unwrap();
        if extended {
            std::fs::write(dir.join("a.js"), "// stale\n").unwrap();
        }
        std::fs::File::options()
            .write(true)
            .open(&config_path)
            .unwrap()
            .set_modified(now - std::time::Duration::from_secs(5))
            .unwrap();
        let (out, code) = ts_r_check_args(&dir, &args);
        assert_eq!(code, 0, "{out}");
        assert!(out.contains("Building project"), "{out}");
        assert_eq!(std::fs::read(&buildinfo).unwrap(), text);
        let (out, code) = ts_r_check_args(&dir, &args);
        assert_eq!(code, 0, "{out}");
        assert!(!out.contains("Building project"), "{out}");
        assert!(out.contains("is up to date"), "{out}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Profiling flags are a TSR2005 warning (accepted and ignored).
#[test]
fn ignored_flags() {
    let dir = temp_project("ts-r-check-cli-unimplemented", &[("tsconfig.json", "{}\n")]);
    let (out, _) = ts_r_check_args(
        &dir,
        &["--pretty", "false", "--generateTrace", "t", "--showConfig"],
    );
    assert!(
        out.starts_with("warning TSR2005: '--generateTrace' is accepted and ignored"),
        "{out}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// `--listFiles` prints every program file (absolute), `--explainFiles` adds why each one
/// is there, and `--listFilesOnly` lists without checking or emitting.
#[test]
fn list_and_explain_files() {
    let dir = temp_project(
        "ts-r-check-cli-explain",
        &[
            (
                "tsconfig.json",
                "{ \"compilerOptions\": { \"noEmit\": true, \"lib\": [\"es5\"] }, \"include\": [\"src\"] }\n",
            ),
            ("src/a.ts", "import { b } from './b';\nlet x: number = b;\n"),
            ("src/b.ts", "export const b = 1;\n"),
        ],
    );
    let (out, code) = ts_r_check_args(&dir, &["--pretty", "false", "--listFiles"]);
    assert_eq!(code, 0, "{out}");
    // `run` shows the project directory as `${FIXTURE}`.
    let a = "${FIXTURE}/src/a.ts";
    assert!(out.lines().any(|l| l == a), "{out}");
    assert!(out.lines().any(|l| l.ends_with("lib.es5.d.ts")), "{out}");

    let (out, code) = ts_r_check_args(&dir, &["--pretty", "false", "--explainFiles"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("src/a.ts\n   Matched by include pattern 'src' in 'tsconfig.json'\n"),
        "{out}"
    );
    assert!(
        out.contains("src/b.ts\n   Imported via './b' from file 'src/a.ts'\n"),
        "{out}"
    );
    assert!(
        out.contains("   Library 'lib.es5.d.ts' specified in compilerOptions\n"),
        "{out}"
    );

    let (out, code) = ts_r_check_args(&dir, &["--pretty", "false", "--listFilesOnly"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.lines().any(|l| l == a), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// `--traceResolution` (or `traceResolution` in the config) logs every module resolution,
/// before the diagnostics.
#[test]
fn trace_resolution() {
    let dir = temp_project(
        "ts-r-check-cli-trace",
        &[
            (
                "tsconfig.json",
                "{ \"compilerOptions\": { \"noEmit\": true, \"lib\": [\"es5\"] }, \"include\": [\"src\"] }\n",
            ),
            ("src/a.ts", "import { b } from './b';\nlet x: string = b;\n"),
            ("src/b.ts", "export const b = 1;\n"),
        ],
    );
    let header = "======== Resolving module './b' from '${FIXTURE}/src/a.ts'. ========";
    let footer =
        "======== Module name './b' was successfully resolved to '${FIXTURE}/src/b.ts'. ========";
    let (out, code) = ts_r_check_args(&dir, &["--pretty", "false", "--traceResolution"]);
    assert_eq!(code, 1, "{out}");
    let lines: Vec<&str> = out.lines().collect();
    let start = lines.iter().position(|l| *l == header).expect(&out);
    let end = lines.iter().position(|l| *l == footer).expect(&out);
    let error = lines.iter().position(|l| l.contains("TS2322")).expect(&out);
    assert!(start < end && end < error, "{out}");
    assert!(
        out.contains("File '${FIXTURE}/src/b.ts' exists - use it as a name resolution result.\n"),
        "{out}"
    );

    let (listed, code) = ts_r_check_args(&dir, &["--pretty", "false", "--listFilesOnly"]);
    assert_eq!(code, 0, "{listed}");
    let assert_list_trace = |out: &str, code| {
        assert_eq!(code, 0, "{out}");
        let start = out.find(header).expect(out);
        let end = out.find(footer).expect(out);
        let list = out.find(&listed).expect(out);
        assert!(start < end && end < list, "{out}");
        assert!(out.ends_with(&listed), "{out}");
        assert!(!out.contains("TS2322"), "{out}");
    };
    let (out, code) = ts_r_check_args(
        &dir,
        &["--pretty", "false", "--listFilesOnly", "--traceResolution"],
    );
    assert_list_trace(&out, code);

    std::fs::write(
        dir.join("tsconfig.json"),
        "{ \"compilerOptions\": { \"noEmit\": true, \"lib\": [\"es5\"], \"traceResolution\": true }, \"include\": [\"src\"] }\n",
    )
    .unwrap();
    let (out, _) = ts_r_check_args(&dir, &["--pretty", "false"]);
    assert!(out.lines().any(|l| l == header), "{out}");
    let (out, code) = ts_r_check_args(&dir, &["--pretty", "false", "--listFilesOnly"]);
    assert_list_trace(&out, code);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Exit codes: 0 without errors, 2 with errors when the outputs were written, 1 when they
/// were not (`--noEmit`); `--quiet` prints nothing but keeps the code; command-line options
/// merge into `--showConfig`.
#[test]
fn exit_codes_quiet_and_show_config() {
    let dir = temp_project(
        "ts-r-check-cli-exit",
        &[
            (
                "tsconfig.json",
                "{ \"compilerOptions\": { \"strict\": true } }\n",
            ),
            ("a.ts", "export const a: number = 1;\n"),
        ],
    );
    let (out, code) = ts_r_check_args(
        &dir,
        &[
            "--pretty",
            "false",
            "--showConfig",
            "--outDir",
            "dist",
            "--types",
            "node",
        ],
    );
    assert_eq!(code, 0);
    assert!(out.contains("\"outDir\": \"./dist\""), "{out}");
    assert!(
        out.contains("\"types\": [\n            \"node\"\n        ]"),
        "{out}"
    );
    if ts_r_checker::LibStore::locate().is_none() {
        return;
    }
    assert_eq!(
        ts_r_check_args(&dir, &["--pretty", "false"]),
        (String::new(), 0)
    );
    std::fs::write(dir.join("a.ts"), "export const a: number = \"x\";\n").unwrap();
    let (out, code) = ts_r_check_args(&dir, &["--pretty", "false"]);
    assert_eq!(code, 2);
    assert!(out.contains("error TS2322"), "{out}");
    assert_eq!(
        std::fs::read_to_string(dir.join("a.js")).unwrap(),
        "export const a = \"x\";\n"
    );
    assert_eq!(
        ts_r_check_args(&dir, &["--pretty", "true", "--quiet"]),
        (String::new(), 2)
    );
    assert_eq!(
        ts_r_check_args(&dir, &["--pretty", "false", "--quiet", "--noEmit"]),
        (String::new(), 1)
    );
    // A command-line option changes the check like a tsconfig one.
    std::fs::write(dir.join("a.ts"), "export function f(x) { return x; }\n").unwrap();
    let (out, code) = ts_r_check_args(&dir, &["--pretty", "false"]);
    assert!(out.contains("error TS7006") && code == 2, "{out}");
    assert_eq!(
        ts_r_check_args(&dir, &["--pretty", "false", "--noImplicitAny", "false"]),
        (String::new(), 0)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Emit as tsc 7.0.2 does it (each expectation is a tsc run on the same project): outputs,
/// `.map` files and `--listEmittedFiles` after the diagnostics, TS5011 without stopping emit
/// (exit 2), `noEmitOnError` writing nothing (exit 1).
#[test]
fn emit_outputs_listing_and_no_emit_on_error() {
    if ts_r_checker::LibStore::locate().is_none() {
        return;
    }
    let files = [
        ("src/a.ts", "export const a: number = \"x\";\n"),
        ("src/sub/b.ts", "export const b = 2;\n"),
        (
            "tsconfig.json",
            r#"{"compilerOptions":{"outDir":"out","sourceMap":true,"target":"es2022","module":"esnext","types":[]}}"#,
        ),
    ];
    let dir = temp_project("ts-r-check-cli-emit", &files);
    let (out, code) = ts_r_check_args(&dir, &["--pretty", "false", "--listEmittedFiles"]);
    assert_eq!(
        out,
        "tsconfig.json(1,21): error TS5011: The common source directory of 'tsconfig.json' is './src'. The 'rootDir' setting must be explicitly set to this or another path to adjust your output's file layout.\n  Visit https://aka.ms/ts6 for migration information.\n\
         TSFILE: ${FIXTURE}/out/src/a.js.map\nTSFILE: ${FIXTURE}/out/src/a.js\n\
         TSFILE: ${FIXTURE}/out/src/sub/b.js.map\nTSFILE: ${FIXTURE}/out/src/sub/b.js\n"
    );
    assert_eq!(code, 2);
    let read = |f: &str| std::fs::read_to_string(dir.join(f)).unwrap();
    assert_eq!(
        read("out/src/a.js"),
        "export const a = \"x\";\n//# sourceMappingURL=a.js.map"
    );
    assert!(read("out/src/a.js.map").starts_with(
        r#"{"version":3,"file":"a.js","sourceRoot":"","sources":["../../src/a.ts"],"names":[],"mappings":""#
    ));
    std::fs::remove_dir_all(dir.join("out")).unwrap();
    let (out, code) = ts_r_check_args(
        &dir,
        &["--pretty", "false", "--noEmitOnError", "--listEmittedFiles"],
    );
    assert!(
        out.contains("error TS5011") && !out.contains("TSFILE"),
        "{out}"
    );
    assert_eq!(code, 1);
    assert!(!dir.join("out").exists());
    // With rootDir set the layout is under it and there is no option error.
    let with_root = [
        "--pretty",
        "false",
        "--rootDir",
        "src",
        "--listEmittedFiles",
    ];
    let (out, code) = ts_r_check_args(&dir, &[&with_root[..], &["--noEmitOnError"]].concat());
    assert!(out.starts_with("src/a.ts(1,14): error TS2322"), "{out}");
    assert_eq!(code, 1);
    std::fs::write(dir.join("src/a.ts"), "export const a: number = 1;\n").unwrap();
    let (out, code) = ts_r_check_args(&dir, &with_root);
    assert_eq!(
        (out.as_str(), code),
        (
            "TSFILE: ${FIXTURE}/out/a.js.map\nTSFILE: ${FIXTURE}/out/a.js\n\
             TSFILE: ${FIXTURE}/out/sub/b.js.map\nTSFILE: ${FIXTURE}/out/sub/b.js\n",
            0
        )
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// `rootDir` and `outDir` outside the project directory (tsc 7.0.2: the layout follows
/// `rootDir`, so `rootDir: ".."` nests the project's own directory under `outDir`), and
/// TS6059 for an imported file outside `rootDir`, which is then emitted next to itself.
#[test]
fn emit_root_dir_outside_the_project() {
    if ts_r_checker::LibStore::locate().is_none() {
        return;
    }
    let dir = temp_project(
        "ts-r-check-cli-rootdir",
        &[
            ("proj/src/b.ts", "export const b = 2;\n"),
            (
                "proj/tsconfig.json",
                r#"{"compilerOptions":{"outDir":"out","rootDir":"..","target":"es2022","module":"esnext","types":[]}}"#,
            ),
            ("other/src/c.ts", "export const c = 3;\n"),
            (
                "other/tsconfig.json",
                r#"{"compilerOptions":{"outDir":"../elsewhere/out","rootDir":"src","target":"es2022","module":"esnext","types":[]}}"#,
            ),
            (
                "imp/src/a.ts",
                "import { l } from \"../lib/l\";\nexport const a = l;\n",
            ),
            ("imp/lib/l.ts", "export const l = 1;\n"),
            (
                "imp/tsconfig.json",
                r#"{"compilerOptions":{"outDir":"out","rootDir":"src","target":"es2022","module":"esnext","types":[]},"include":["src"]}"#,
            ),
        ],
    );
    let list = ["--pretty", "false", "--listEmittedFiles"];
    let (out, code) = ts_r_check_args(&dir.join("proj"), &list);
    assert_eq!(
        (out.as_str(), code),
        ("TSFILE: ${FIXTURE}/out/proj/src/b.js\n", 0)
    );
    let (out, code) = ts_r_check_args(&dir.join("other"), &list);
    let root = dir.to_string_lossy().replace('\\', "/");
    assert_eq!(
        (out, code),
        (format!("TSFILE: {root}/elsewhere/out/c.js\n"), 0)
    );
    assert!(dir.join("elsewhere/out/c.js").is_file());
    let (out, code) = ts_r_check_args(&dir.join("imp"), &list);
    assert_eq!(
        out,
        "src/a.ts(1,19): error TS6059: File '${FIXTURE}/lib/l.ts' is not under 'rootDir' '${FIXTURE}/src'. 'rootDir' is expected to contain all source files.\n\
         TSFILE: ${FIXTURE}/lib/l.js\nTSFILE: ${FIXTURE}/out/a.js\n"
    );
    assert_eq!(code, 2);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Writes are irreversible: an output that is an input is never written. TS5055 when the
/// paths say so (as tsc), and, unlike tsc 7.0.2 (which overwrites the input through the
/// link), a TS5033 write error when an `outDir` symbolic link leads back to the sources.
#[test]
fn emit_never_overwrites_inputs() {
    if ts_r_checker::LibStore::locate().is_none() {
        return;
    }
    let input = "export const a     =     1;\n";
    let dir = temp_project(
        "ts-r-check-cli-overwrite",
        &[
            ("src/a.js", input),
            ("src/b.ts", "export const b = 2;\n"),
            (
                "tsconfig.json",
                r#"{"compilerOptions":{"allowJs":true,"outDir":"out","rootDir":"src","target":"es2022","module":"esnext","types":[]},"include":["src"]}"#,
            ),
        ],
    );
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("src", dir.join("out")).unwrap();
        let (out, code) = ts_r_check_args(&dir, &["--pretty", "false", "--listEmittedFiles"]);
        assert!(
            out.starts_with(
                "error TS5033: Could not write file '${FIXTURE}/out/a.js': it is the input file"
            ),
            "{out}"
        );
        assert!(out.ends_with("TSFILE: ${FIXTURE}/out/b.js\n"), "{out}");
        assert_eq!(code, 1);
        assert_eq!(
            std::fs::read_to_string(dir.join("src/a.js")).unwrap(),
            input
        );
        std::fs::remove_file(dir.join("out")).unwrap();
    }
    // An outDir equal to the sources: TS5055 (as tsc), the input untouched, b.js written. The
    // inputs are listed in `files`: a command-line outDir also joins the default excludes, so
    // with `include: ["src"]` there would be no inputs (TS18003, as tsc 7.0.2).
    std::fs::write(
        dir.join("tsconfig.json"),
        r#"{"compilerOptions":{"allowJs":true,"outDir":"out","rootDir":"src","target":"es2022","module":"esnext","types":[]},"files":["src/a.js","src/b.ts"]}"#,
    )
    .unwrap();
    let (out, code) = ts_r_check_args(&dir, &["--pretty", "false", "--outDir", "src"]);
    assert!(
        out.contains("error TS5055: Cannot write file '${FIXTURE}/src/a.js' because it would overwrite input file."),
        "{out}"
    );
    assert_eq!(code, 1);
    assert_eq!(
        std::fs::read_to_string(dir.join("src/a.js")).unwrap(),
        input
    );
    assert!(dir.join("src/b.js").is_file());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[ignore = "runs the real tsc; use to regenerate expected outputs"]
fn regenerate() {
    let tsc = std::env::var("TS_R_TSC").expect("set TS_R_TSC to a tsc 7 binary");
    for dir in fixtures() {
        // tsc emits, so it runs on a fresh copy per run; its path is stored as the placeholder.
        let exp = dir.join("expected");
        std::fs::create_dir_all(&exp).unwrap();
        let mut exit = 0;
        for (pretty, file) in [("false", "plain.txt"), ("true", "pretty.txt")] {
            let copy = copy_fixture(&dir, "tsc");
            let (text, code) = run(
                Command::new(&tsc).args(["-p", ".", "--pretty", pretty]),
                &copy,
            );
            let _ = std::fs::remove_dir_all(&copy);
            exit = code;
            std::fs::write(exp.join(file), text).unwrap();
        }
        std::fs::write(exp.join("exit.txt"), format!("{exit}\n")).unwrap();
    }
}

/// Runs `ts-r-check -w ...` under the stdin driver: `looks` lines are sent (one look for
/// changes each), then stdin is closed, which ends the session. Killed after 60 s.
fn ts_r_check_watch(dir: &Path, args: &[&str], looks: usize) -> (String, i32) {
    use std::io::{Read, Write};
    let mut child = Command::new(env!("CARGO_BIN_EXE_ts-r-check"))
        .args(["--no-history", "--sound=off"])
        .args(args)
        .env("TS_R_WATCH_DRIVER", "stdin")
        .current_dir(dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("run");
    let mut stdout = child.stdout.take().expect("stdout");
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        s
    });
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all("\n".repeat(looks).as_bytes());
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let code = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s.code().unwrap_or(-1),
            Ok(None) if std::time::Instant::now() > deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("ts-r-check {args:?} did not end its watch session");
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(10)),
            Err(e) => panic!("{e}"),
        }
    };
    (reader.join().unwrap_or_default(), code)
}

/// `--watch` (record 0012, E4): tsc's status lines, the screen cleared before each build
/// only with pretty output and without `--preserveWatchOutput`; the session's exit status is
/// 0 for `-p` and the first build's for `-b`; a look that finds no change prints nothing.
#[test]
fn watch_sessions() {
    if ts_r_checker::LibStore::locate().is_none() {
        eprintln!("lib files missing: skipped");
        return;
    }
    let dir = temp_project(
        "ts-r-check-cli-watch",
        &[
            ("tsconfig.json", "{}\n"),
            ("a.ts", "const a: number = \"x\";\n"),
        ],
    );
    let sentinel = "\u{1}ts-r-watch-cycle";
    let (out, code) = ts_r_check_watch(&dir, &["-w", "--pretty", "false"], 2);
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("\x1b[2J"), "{out}");
    assert!(
        out.contains(" - Starting compilation in watch mode...\n\n"),
        "{out}"
    );
    assert!(out.contains("a.ts(1,7): error TS2322:"), "{out}");
    assert!(
        out.contains(" - Found 1 error. Watching for file changes.\n\n"),
        "{out}"
    );
    // Three looks (the first build, then two without changes), one build.
    assert_eq!(out.matches(sentinel).count(), 3, "{out}");
    assert_eq!(out.matches("Starting compilation").count(), 1, "{out}");
    assert!(!out.contains("File change detected"), "{out}");
    assert!(dir.join("a.js").is_file());

    let (out, _) = ts_r_check_watch(&dir, &["-w", "--pretty"], 0);
    assert!(out.starts_with("\x1b[2J\x1b[3J\x1b[H["), "{out}");
    assert!(out.contains("Found 1 error in a.ts"), "{out}");
    let (out, _) = ts_r_check_watch(&dir, &["-w", "--pretty", "--preserveWatchOutput"], 0);
    assert!(!out.contains("\x1b[2J"), "{out}");

    let (out, code) = ts_r_check_watch(&dir, &["-b", "-w", "--pretty", "false"], 0);
    assert_eq!(code, 2, "{out}");
    assert!(
        out.contains(" - Found 1 error. Watching for file changes."),
        "{out}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Octal and `\8` escapes in strings (`ParseResult::lenient`) are syntax errors to tsc: the
/// default run (cold history, warm history, and after an edit), `--no-history` and
/// `--noCheck` all report them and nothing from the checker, as tsc 7 does (observed: the
/// two errors below, exit 1).
#[test]
fn regression_octal_escapes_in_strings_are_syntax_errors_on_every_path() {
    let lib = "interface Array<T> {}\ninterface Boolean {}\ninterface Function {}\ninterface CallableFunction {}\ninterface NewableFunction {}\ninterface IArguments {}\ninterface Number {}\ninterface Object {}\ninterface RegExp {}\ninterface String {}\n";
    let src = "let s = \"\\033\";\nlet n: number = \"x\";\nlet t = \"\\8\";\n";
    let dir = temp_project(
        "ts-r-check-octal-escapes",
        &[
            ("a.ts", src),
            ("lib.d.ts", lib),
            (
                "tsconfig.json",
                r#"{"compilerOptions":{"target":"es2022","noLib":true,"noEmit":true},"files":["a.ts","lib.d.ts"]}"#,
            ),
        ],
    );
    let cache = dir.join(".cache");
    let tsc = "a.ts(1,10): error TS1487: Octal escape sequences are not allowed. Use the syntax '\\x1b'.\na.ts(3,10): error TS1488: Escape sequence '\\8' is not allowed.\n";
    let with_history = |args: &[&str]| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_ts-r-check"));
        cmd.env("TS_R_CACHE_DIR", &cache)
            .args(["--sound=off", "-p", ".", "--pretty", "false"])
            .args(args);
        run(&mut cmd, &dir)
    };
    for (what, (out, code)) in [
        ("cold history", with_history(&[])),
        ("warm history", with_history(&[])),
        ("noCheck", with_history(&["--noCheck"])),
        (
            "no history",
            ts_r_check_args(&dir, &["-p", ".", "--pretty", "false"]),
        ),
    ] {
        assert_eq!((out.as_str(), code), (tsc, 1), "{what}");
    }
    // An edit goes through the history's fresh-file path.
    std::fs::write(dir.join("a.ts"), format!("{src}let u = \"\\01\";\n")).unwrap();
    let (out, code) = with_history(&[]);
    assert_eq!(
        (out.as_str(), code),
        (
            format!("{tsc}a.ts(4,10): error TS1487: Octal escape sequences are not allowed. Use the syntax '\\x01'.\n").as_str(),
            1
        )
    );
    // Without them the checker runs.
    std::fs::write(dir.join("a.ts"), "let n: number = \"x\";\n").unwrap();
    let (out, code) = with_history(&[]);
    assert_eq!(
        (out.as_str(), code),
        (
            "a.ts(1,5): error TS2322: Type 'string' is not assignable to type 'number'.\n",
            1
        )
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A `rootDir` no longer keeps the check history out: with every emittable file under it
/// (a declaration file outside does not count) a warm run reuses the history, and a file
/// outside it still gets tsc's TS6059 on every path (tsc 7.0.2, `noEmit`: the TS6059 at the
/// import, then the checker's errors, exit 1).
#[test]
fn regression_root_dir_uses_history_and_keeps_ts6059() {
    let lib = "interface Array<T> {}\ninterface Boolean {}\ninterface Function {}\ninterface CallableFunction {}\ninterface NewableFunction {}\ninterface IArguments {}\ninterface Number {}\ninterface Object {}\ninterface RegExp {}\ninterface String {}\n";
    let dir = temp_project(
        "ts-r-check-rootdir-history",
        &[
            (
                "src/a.ts",
                "import { d } from \"../lib/d\";\nexport const a: string = d;\n",
            ),
            ("lib/d.d.ts", "export declare const d: number;\n"),
            ("lib/l.ts", "export const l = 1;\n"),
            ("types/lib.d.ts", lib),
            (
                "tsconfig.json",
                r#"{"compilerOptions":{"noEmit":true,"rootDir":"src","target":"es2022","module":"esnext","noLib":true,"types":[]},"files":["src/a.ts","types/lib.d.ts"]}"#,
            ),
        ],
    );
    let cache = dir.join(".cache");
    let check = |stats: bool| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_ts-r-check"));
        cmd.env("TS_R_CACHE_DIR", &cache)
            .args(["--sound=off", "-p", ".", "--pretty", "false"]);
        if stats {
            cmd.env("TS_R_CHECK_STATS", "1");
        }
        run(&mut cmd, &dir)
    };
    let ts2322 =
        "src/a.ts(2,14): error TS2322: Type 'number' is not assignable to type 'string'.\n";
    assert_eq!(check(false), (ts2322.to_owned(), 1), "cold history");
    let (out, code) = check(true);
    assert_eq!(code, 1);
    assert!(
        // Files this new are inside the history's racy window: re-hashed, not re-checked.
        out.starts_with(ts2322) && out.contains(", 0 checked, 3 reused"),
        "warm history: {out}"
    );
    // A source outside `rootDir`: TS6059 at the import, cold, warm and without history.
    std::fs::write(
        dir.join("src/a.ts"),
        "import { l } from \"../lib/l\";\nexport const a: string = l;\n",
    )
    .unwrap();
    let both = format!(
        "src/a.ts(1,19): error TS6059: File '${{FIXTURE}}/lib/l.ts' is not under 'rootDir' '${{FIXTURE}}/src'. 'rootDir' is expected to contain all source files.\n{ts2322}"
    );
    for (what, (out, code)) in [
        ("after the edit", check(false)),
        ("warm", check(false)),
        (
            "no history",
            ts_r_check_args(&dir, &["--sound=off", "-p", ".", "--pretty", "false"]),
        ),
    ] {
        assert_eq!((out.as_str(), code), (both.as_str(), 1), "{what}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
