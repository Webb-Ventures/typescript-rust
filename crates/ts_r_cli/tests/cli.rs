//! End-to-end tests of the `ts-r` binary with fake tool scripts (Unix only: the fakes are
//! shell scripts).
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{PoisonError, RwLock};

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Linux refuses to exec a file that any process has open for writing (`ETXTBSY`). Tests run
/// on parallel threads, and a `fork` on one thread copies every open descriptor, including the
/// one another thread is writing a script through, into a child that keeps it until it execs.
/// So executables are written under the write lock and processes are spawned under the read
/// lock: no child can hold a file that is about to be run.
static EXEC_LOCK: RwLock<()> = RwLock::new(());

/// Writes an executable file while no process is being spawned.
fn write_executable(path: &Path, contents: &str) {
    let _writing = EXEC_LOCK.write().unwrap_or_else(PoisonError::into_inner);
    std::fs::write(path, contents).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Like `Command::output`, but spawns under the read lock. `spawn` returns once the child has
/// exec'd, so its copies of other threads' descriptors are closed by then.
fn output(cmd: &mut Command) -> Output {
    let child = {
        let _spawning = EXEC_LOCK.read().unwrap_or_else(PoisonError::into_inner);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    };
    child.wait_with_output().unwrap()
}

/// A sandbox: a copy of the launcher in `bin/`, an empty cache, a project dir, and an
/// environment that sees none of the real machine's ts-r installs.
struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Sandbox {
        static N: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "ts-r-cli-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        for d in ["bin", "cache", "project/src", "path"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        {
            let _writing = EXEC_LOCK.write().unwrap_or_else(PoisonError::into_inner);
            std::fs::copy(env!("CARGO_BIN_EXE_ts-r"), root.join("bin/ts-r")).unwrap();
        }
        Sandbox { root }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    /// Writes an executable shell script.
    fn script(&self, rel: &str, body: &str) {
        let p = self.path(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        write_executable(&p, &format!("#!/bin/sh\n{body}\n"));
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(self.path("bin/ts-r"));
        c.args(args)
            .current_dir(self.path("project/src"))
            .env_clear()
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.path("path").display()),
            )
            .env("HOME", self.path("home"))
            .env("TS_R_CACHE_DIR", self.path("cache"));
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        output(&mut self.cmd(args))
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn version_and_help() {
    let sb = Sandbox::new("version");
    let o = sb.run(&["--version"]);
    assert!(o.status.success());
    assert_eq!(stdout(&o), format!("ts-r {VERSION}\n"));
    let o = sb.run(&["--help"]);
    assert!(o.status.success());
    for cmd in [
        "dev", "build", "check", "lint", "run", "lsp", "ci", "doctor",
    ] {
        assert!(
            stdout(&o).contains(&format!("  {cmd} ")),
            "help lists {cmd}"
        );
    }
    assert_eq!(sb.run(&[]).status.code(), Some(0));
    assert_eq!(sb.run(&["nope"]).status.code(), Some(2));
    assert_eq!(sb.run(&["--nope"]).status.code(), Some(2));
}

#[test]
fn missing_tool_has_a_clear_error() {
    let sb = Sandbox::new("missing");
    let o = sb.run(&["run", "x.ts"]);
    assert_eq!(o.status.code(), Some(127));
    let err = stderr(&o);
    assert!(
        err.contains("ts-r-run isn't installed; it ships in milestone M1."),
        "{err}"
    );
    assert!(err.contains(&sb.path("bin").display().to_string()), "{err}");
    assert!(err.contains("PATH"), "{err}");
}

#[test]
fn passes_args_and_exit_code_through() {
    let sb = Sandbox::new("passthrough");
    sb.script(
        "bin/ts-r-run",
        r#"for a in "$@"; do echo "[$a]"; done; echo "cwd=$(pwd)"; exit 7"#,
    );
    let o = sb.run(&["run", "a b", "--flag", "", "--help"]);
    assert_eq!(o.status.code(), Some(7));
    let out = stdout(&o);
    assert!(out.starts_with("[a b]\n[--flag]\n[]\n[--help]\n"), "{out}");
    assert!(
        out.contains("project/src"),
        "runs in the caller's directory: {out}"
    );
}

#[test]
fn search_order_sibling_cache_path() {
    let sb = Sandbox::new("order");
    sb.script("path/ts-r-lint", "echo from-path");
    assert_eq!(stdout(&sb.run(&["lint"])), "from-path\n");
    sb.script(&format!("cache/bin/{VERSION}/ts-r-lint"), "echo from-cache");
    assert_eq!(stdout(&sb.run(&["lint"])), "from-cache\n");
    sb.script("bin/ts-r-lint", "echo from-sibling");
    assert_eq!(stdout(&sb.run(&["lint"])), "from-sibling\n");
}

#[test]
fn version_file_pins_tools() {
    let sb = Sandbox::new("pin");
    sb.script("bin/ts-r-build", "echo own");
    sb.script("cache/bin/9.9.9/ts-r-build", "echo pinned-9.9.9");
    std::fs::write(sb.path("project/.ts-r-version"), "# team pin\nv9.9.9\n").unwrap();
    // Found walking up from project/src.
    assert_eq!(stdout(&sb.run(&["build"])), "pinned-9.9.9\n");

    // A pin to a version that isn't installed fails clearly, even though a sibling exists.
    std::fs::write(sb.path("project/.ts-r-version"), "1.2.3\n").unwrap();
    let o = sb.run(&["build"]);
    assert_eq!(o.status.code(), Some(127));
    assert!(
        stderr(&o).contains("ts-r-build 1.2.3 (pinned by"),
        "{}",
        stderr(&o)
    );
    assert!(stderr(&o).contains(".ts-r-version"));

    // TS_R_VERSION overrides the file; pinning the launcher's own version uses the sibling.
    let o = output(sb.cmd(&["build"]).env("TS_R_VERSION", "9.9.9"));
    assert_eq!(stdout(&o), "pinned-9.9.9\n");
    std::fs::write(sb.path("project/.ts-r-version"), format!("{VERSION}\n")).unwrap();
    assert_eq!(stdout(&sb.run(&["build"])), "own\n");

    std::fs::write(sb.path("project/.ts-r-version"), "not a version\n").unwrap();
    let o = sb.run(&["build"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(stderr(&o).contains("invalid version"), "{}", stderr(&o));
}

#[test]
fn ci_runs_all_three_and_reports() {
    let sb = Sandbox::new("ci");
    sb.script(
        "bin/ts-r-check",
        "echo checking; echo 'type error' >&2; exit 0",
    );
    sb.script("bin/ts-r-lint", "echo linting; printf 'no newline'; exit 3");
    sb.script(
        "bin/ts-r-build",
        "echo threads=$TS_R_THREADS; sleep 0.2; echo built",
    );
    let o = sb.run(&["ci"]);
    assert_eq!(o.status.code(), Some(1), "lint failed so ci fails");
    let out = stdout(&o);
    let err = stderr(&o);
    assert!(out.contains("check | checking\n"), "{out}");
    assert!(
        err.contains("check | type error\n"),
        "stderr stays on stderr: {err}"
    );
    assert!(out.contains("lint  | linting\n"), "{out}");
    assert!(out.contains("lint  | no newline\n"), "{out}");
    assert!(out.contains("build | built\n"), "{out}");
    assert!(out.contains("build | threads="), "{out}");
    assert!(out.contains("ts-r ci FAILED in"), "{out}");
    // Summary rows: name, status, seconds, detail.
    let row = |name: &str| -> Vec<String> {
        let line = out
            .lines()
            .find(|l| l.starts_with(&format!("  {name} ")))
            .unwrap_or_default();
        line.split_whitespace().map(str::to_owned).collect()
    };
    assert_eq!(row("lint")[1..2], ["FAILED"], "{out}");
    assert_eq!(row("lint")[3..], ["(exit", "3)"], "{out}");
    assert_eq!(row("check")[1], "ok", "{out}");
    assert_eq!(row("build")[1], "ok", "{out}");
    assert!(row("build")[2].ends_with('s'), "{out}");

    sb.script("bin/ts-r-lint", "echo fixed");
    let o = sb.run(&["ci"]);
    assert_eq!(o.status.code(), Some(0), "{}", stdout(&o));
    assert!(stdout(&o).contains("ts-r ci passed in"));
}

#[test]
fn ci_with_missing_tools() {
    let sb = Sandbox::new("ci-missing");
    sb.script("bin/ts-r-lint", "echo linting");
    let o = sb.run(&["ci"]);
    assert_eq!(o.status.code(), Some(1));
    let err = stderr(&o);
    assert!(
        err.contains("can't run check: ts-r-check isn't installed; it ships in milestone M5"),
        "{err}"
    );
    assert!(err.contains("--skip-missing"), "{err}");
    assert!(
        !stdout(&o).contains("linting"),
        "nothing runs unless everything is installed"
    );

    let o = sb.run(&["ci", "--skip-missing"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert!(stdout(&o).contains("lint  | linting"));
    assert!(stdout(&o).contains("check  skipped (not installed)"));
    assert_eq!(sb.run(&["ci", "--bogus"]).status.code(), Some(2));
}

#[test]
fn doctor_reports_tools_cache_and_project() {
    let sb = Sandbox::new("doctor");
    sb.script("bin/ts-r-run", "true");
    std::fs::write(sb.path("project/tsconfig.json"), "{}").unwrap();
    let o = sb.run(&["doctor"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let out = stdout(&o);
    assert!(
        out.contains(&format!(
            "{}  (next to the launcher)",
            sb.path("bin/ts-r-run").display()
        )),
        "{out}"
    );
    assert!(
        out.contains("not installed (ships in milestone M5)"),
        "{out}"
    );
    assert!(
        out.contains(&format!(
            "{} (from TS_R_CACHE_DIR)",
            sb.path("cache").display()
        )),
        "{out}"
    );
    assert!(
        out.contains(&sb.path("project/tsconfig.json").display().to_string()),
        "{out}"
    );
}

#[test]
fn exec_failure_is_reported() {
    let sb = Sandbox::new("execfail");
    // Executable, but its interpreter doesn't exist, so exec itself fails.
    let p = sb.path("bin/ts-r-dev");
    write_executable(&p, "#!/nonexistent/interpreter\n");
    let o = sb.run(&["dev"]);
    assert_eq!(o.status.code(), Some(126));
    assert!(stderr(&o).contains("failed to run"), "{}", stderr(&o));
}
