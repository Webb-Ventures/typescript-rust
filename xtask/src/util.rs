//! Shared helpers: errors, paths, subprocesses, HTTP (via `curl`), dates and versions.

use std::cmp::Ordering;
use std::ffi::OsStr;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

pub type Error = Box<dyn std::error::Error>;
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Workspace root (the directory holding the root `Cargo.toml`).
pub fn root() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap_or(manifest).to_path_buf()
}

/// Cargo's target directory for builds started from the repo root, where xtask looks for the
/// binaries cargo built: `CARGO_TARGET_DIR` when it is set and not empty (a relative value
/// resolves against the repo root, as cargo does for a build run there), else the
/// `target_directory` `cargo metadata` reports (which also covers `build.target-dir` in a
/// cargo config), else `<root>/target`. Worked out once per process.
pub fn target_dir() -> PathBuf {
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let root = root();
        target_dir_in(&root, std::env::var_os("CARGO_TARGET_DIR"), || {
            metadata_target_dir(&root)
        })
    })
    .clone()
}

/// [`target_dir`] with its inputs passed in (for tests).
pub fn target_dir_in(
    root: &Path,
    env: Option<std::ffi::OsString>,
    metadata: impl FnOnce() -> Option<PathBuf>,
) -> PathBuf {
    if let Some(v) = env.filter(|v| !v.is_empty()) {
        let p = PathBuf::from(v);
        return if p.is_absolute() { p } else { root.join(p) };
    }
    metadata().unwrap_or_else(|| root.join("target"))
}

/// `target_directory` from `cargo metadata --no-deps`, run in `root`.
fn metadata_target_dir(root: &Path) -> Option<PathBuf> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let out = Command::new(cargo)
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--offline",
        ])
        .current_dir(root)
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    v.get("target_directory")?.as_str().map(PathBuf::from)
}

/// The directory cargo puts a profile's binaries in: `<target>/debug` for `dev`,
/// `<target>/<profile>` otherwise.
pub fn profile_dir(profile: &str) -> PathBuf {
    target_dir().join(if profile == "dev" { "debug" } else { profile })
}

/// `<target>/release`, where `cargo build --release` puts the binaries.
pub fn release_dir() -> PathBuf {
    profile_dir("release")
}

/// Path relative to the workspace root, with `/` separators, for messages and reports.
pub fn rel(path: &Path) -> String {
    let root = root();
    let p = path.strip_prefix(&root).unwrap_or(path);
    p.to_string_lossy().replace('\\', "/")
}

/// Name of an executable, adding `.cmd` for npm shims on Windows.
pub fn npm_tool(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.cmd")
    } else {
        name.to_string()
    }
}

/// Whether `program` can be started (`program --version` exits successfully).
pub fn has_program(program: &str) -> bool {
    Command::new(program)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// `program` under `scripts/limit.sh` with its default caps (4096 MB for the whole process tree,
/// 600 s), as the root AGENTS.md resource limits require for oracle and benchmark runs.
/// limit.sh is a bash script, so on Windows `program` runs directly.
pub fn limited(program: impl AsRef<OsStr>) -> Command {
    if cfg!(windows) {
        return Command::new(program);
    }
    let mut cmd = Command::new(root().join("scripts/limit.sh"));
    cmd.arg("--").arg(program);
    cmd
}

/// Runs a command and returns its stdout. Non-zero exit is an error carrying stderr.
pub fn run_capture(cmd: &mut Command) -> Result<String> {
    let out = cmd
        .output()
        .map_err(|e| format!("failed to start {:?}: {e}", cmd.get_program()))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "{} failed ({}): {}",
            describe(cmd),
            out.status,
            stderr.trim()
        )
        .into());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Runs a command with inherited stdio. Non-zero exit is an error.
pub fn run_status(cmd: &mut Command) -> Result<()> {
    let status = cmd
        .status()
        .map_err(|e| format!("failed to start {:?}: {e}", cmd.get_program()))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{} failed ({status})", describe(cmd)).into())
    }
}

/// The command line for error messages, with `Authorization` headers (the GitHub token that
/// `http_get` passes to curl) redacted.
fn describe(cmd: &Command) -> String {
    let mut s = cmd.get_program().to_string_lossy().into_owned();
    for a in cmd.get_args() {
        let a = a.to_string_lossy();
        s.push(' ');
        if a.get(..14)
            .is_some_and(|h| h.eq_ignore_ascii_case("authorization:"))
        {
            s.push_str("Authorization: <redacted>");
        } else {
            s.push_str(&a);
        }
    }
    s
}

/// GET a URL with `curl` and return the body. Adds `GITHUB_TOKEN` for api.github.com.
pub fn http_get(url: &str, accept: Option<&str>) -> Result<String> {
    let mut cmd = Command::new("curl");
    cmd.args([
        "-fsSL",
        "--retry",
        "2",
        "--max-time",
        "60",
        "-A",
        "ts-r-xtask",
    ]);
    if let Some(accept) = accept {
        cmd.args(["-H", &format!("Accept: {accept}")]);
    }
    if url.starts_with("https://api.github.com/") {
        let token = std::env::var("GITHUB_TOKEN").or_else(|_| std::env::var("GH_TOKEN"));
        if let Ok(token) = token
            && !token.is_empty()
        {
            cmd.args(["-H", &format!("Authorization: Bearer {token}")]);
        }
    }
    cmd.arg(url);
    run_capture(&mut cmd).map_err(|e| format!("GET {url}: {e}").into())
}

pub fn http_json(url: &str, accept: Option<&str>) -> Result<serde_json::Value> {
    let body = http_get(url, accept)?;
    serde_json::from_str(&body).map_err(|e| format!("GET {url}: bad JSON: {e}").into())
}

/// Writes `text` to `out` (creating parent dirs), or to stdout when `out` is `None`.
pub fn write_output(out: Option<&str>, text: &str) -> Result<()> {
    match out {
        Some(path) => {
            let path = Path::new(path);
            if let Some(parent) = path.parent()
                && !parent.as_os_str().is_empty()
            {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(path, text)?;
            eprintln!("wrote {}", path.display());
            Ok(())
        }
        None => {
            print!("{text}");
            Ok(())
        }
    }
}

pub fn read_to_string(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()).into())
}

/// Today's UTC date as `YYYY-MM-DD`.
pub fn today() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    civil_date((secs / 86_400) as i64)
}

/// Converts days since 1970-01-01 to `YYYY-MM-DD` (proleptic Gregorian calendar).
pub fn civil_date(days: i64) -> String {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// A semver version (`7.0.2`, `7.0.1-rc`, `7.1.0-dev.20261014.1`). Build metadata is ignored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub pre: Vec<String>,
}

impl Version {
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().trim_start_matches('v');
        let s = s.split('+').next()?;
        let (core, pre) = match s.split_once('-') {
            Some((core, pre)) => (core, pre.split('.').map(String::from).collect()),
            None => (s, Vec::new()),
        };
        let mut nums = core.split('.').map(|n| n.parse::<u64>().ok());
        let v = Version {
            major: nums.next()??,
            minor: nums.next()??,
            patch: nums.next()??,
            pre,
        };
        if nums.next().is_some() {
            return None;
        }
        Some(v)
    }

    pub fn is_stable(&self) -> bool {
        self.pre.is_empty()
    }

    /// Release candidates and betas (not nightlies / dev builds / insiders).
    pub fn is_candidate(&self) -> bool {
        matches!(self.pre.first().map(String::as_str), Some("rc" | "beta"))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => cmp_pre(&self.pre, &other.pre),
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn cmp_pre(a: &[String], b: &[String]) -> Ordering {
    for (x, y) in a.iter().zip(b) {
        let ord = match (x.parse::<u64>(), y.parse::<u64>()) {
            (Ok(x), Ok(y)) => x.cmp(&y),
            (Ok(_), Err(_)) => Ordering::Less,
            (Err(_), Ok(_)) => Ordering::Greater,
            (Err(_), Err(_)) => x.cmp(y),
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    a.len().cmp(&b.len())
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if !self.pre.is_empty() {
            write!(f, "-{}", self.pre.join("."))?;
        }
        Ok(())
    }
}

/// Collects `.md` files under `dir` recursively, sorted, skipping `target` and `node_modules`.
pub fn walk_files(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if path.is_dir() {
            if name != "target" && name != "node_modules" && name != ".git" {
                walk_files(&path, ext, out);
            }
        } else if path.extension().and_then(|e| e.to_str()) == Some(ext) {
            out.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_dir_follows_cargo_target_dir_then_metadata() {
        let root = Path::new("/repo");
        let none = || None;
        assert_eq!(target_dir_in(root, None, none), Path::new("/repo/target"));
        assert_eq!(
            target_dir_in(root, Some("".into()), none),
            Path::new("/repo/target")
        );
        assert_eq!(
            target_dir_in(root, Some("build/out".into()), none),
            Path::new("/repo/build/out")
        );
        let abs = std::env::temp_dir().join("ts-r-target");
        assert_eq!(target_dir_in(root, Some(abs.clone().into()), none), abs);
        // No environment variable: cargo's reported directory wins over `<root>/target`.
        let reported = std::env::temp_dir().join("ts-r-cargo-config-target");
        assert_eq!(
            target_dir_in(root, None, || Some(reported.clone())),
            reported
        );
        // The variable wins over what cargo reports.
        assert_eq!(
            target_dir_in(root, Some("t".into()), || Some(reported.clone())),
            Path::new("/repo/t")
        );
    }

    #[test]
    fn release_dir_is_inside_the_reported_target_dir() {
        // This test runs under cargo, so the reported directory is the one it was built in.
        let real = |p: PathBuf| std::fs::canonicalize(&p).unwrap_or(p);
        let exe = real(std::env::current_exe().unwrap());
        let target = target_dir();
        let real_target = real(target.clone());
        assert!(
            exe.starts_with(&real_target),
            "{} not in {}",
            exe.display(),
            real_target.display()
        );
        assert_eq!(profile_dir("dev"), target.join("debug"));
        assert_eq!(release_dir(), target.join("release"));
    }

    #[test]
    #[cfg(unix)]
    fn limited_runs_under_limit_sh() {
        let mut cmd = limited("sh");
        assert_eq!(cmd.get_program(), root().join("scripts/limit.sh"));
        let status = cmd.args(["-c", "exit 3"]).status().unwrap();
        assert_eq!(status.code(), Some(3));
    }

    #[test]
    fn errors_never_show_the_token() {
        let mut cmd = Command::new("curl");
        cmd.args(["-H", "Authorization: Bearer ghp_secret", "-H", "Accept: x"]);
        assert_eq!(
            describe(&cmd),
            "curl -H Authorization: <redacted> -H Accept: x"
        );
        let err = run_capture(cmd.arg("--definitely-not-a-curl-flag")).unwrap_err();
        assert!(!err.to_string().contains("ghp_secret"), "{err}");
    }

    #[test]
    fn dates() {
        assert_eq!(civil_date(0), "1970-01-01");
        assert_eq!(civil_date(11_016), "2000-02-29");
        assert_eq!(civil_date(20_740), "2026-10-14");
    }

    #[test]
    fn version_parse_and_order() {
        let v = |s| Version::parse(s).unwrap();
        assert_eq!(v("v7.0.2").to_string(), "7.0.2");
        assert!(v("7.0.2") > v("7.0.1"));
        assert!(v("7.0.1") > v("7.0.1-rc"));
        assert!(v("7.0.1-rc") > v("7.0.1-beta"));
        assert!(v("7.1.0-dev.20261014.1") > v("7.1.0-dev.20261013.2"));
        assert!(v("7.1.0-dev.20261014.1") > v("7.0.2"));
        assert!(v("7.0.1-rc").is_candidate());
        assert!(!v("7.1.0-dev.20261014.1").is_candidate());
        assert!(!v("4.6.2-insiders.20220225").is_stable());
        assert!(Version::parse("7.0").is_none());
        assert!(Version::parse("latest").is_none());
    }
}
