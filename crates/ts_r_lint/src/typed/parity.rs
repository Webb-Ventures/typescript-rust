//! The parity check for type-aware rules (plan: P8/R13, the same check `ts-r-lsp` runs).
//!
//! typescript-eslint answers type questions with the project's own TypeScript. ts-r-lint
//! answers them with our checker, which is only trusted where it agrees with that
//! TypeScript: a project's type-aware rules run when our diagnostics for the project equal
//! `tsc -p <tsconfig> --noEmit` (the multiset of `file(line,col): TScode`). Otherwise every
//! type-aware rule is reported once as unsupported in that project and none of them runs
//! there, so a disagreement can only lose reports, never add wrong ones.
//!
//! The user's tsc runs once per project (memory- and time-capped, like `scripts/limit.sh`);
//! the result is cached in `<cache>/lint/parity/` under a key of the tsconfig and everything
//! it extends, the nearest lockfile, the TypeScript version and the ts-r version, so it is
//! repeated only when one of those changes (as in the editor: source edits do not rerun it).
//!
//! Our checker follows tsc 7. Where an older user TypeScript reports the same error with a
//! different headline, the comparison pairs the two (observed, TypeScript 6.0.3 vs 7.0.2): an
//! argument missing properties is `TS2345` ("Argument of type ... is not assignable") whose
//! first chain line is the `TS2741`/`TS2739`/`TS2740` message in 6.x, and that message itself
//! at the same position in 7.x. Only an unmatched pair at the same position cancels, and only
//! when the user's TypeScript is older than 7.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// Bumped when the comparison changes.
const PARITY_VERSION: u32 = 2;
/// Prefix of the alternate keys [`keys`] records for a `TS2345` whose first chain line is a
/// missing-property message (`alt:<file>(<pos>): TS2741`); not diagnostics themselves.
const ALT: &str = "alt:";
const TSC_MEM_MB: u64 = 4096;
const TSC_TIME: Duration = Duration::from_secs(600);

/// The outcome of a project's parity check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Parity {
    /// Same diagnostics as the user's TypeScript: type-aware rules run.
    Match,
    /// Different diagnostics: type-aware rules are unsupported in the project.
    Mismatch,
    /// Could not compare (no TypeScript installed, tsc failed): our checker is used, as in
    /// the editor.
    Unknown,
}

impl Parity {
    pub fn as_str(self) -> &'static str {
        match self {
            Parity::Match => "match",
            Parity::Mismatch => "mismatch",
            Parity::Unknown => "unknown",
        }
    }
    fn parse(s: &str) -> Option<Parity> {
        Some(match s {
            "match" => Parity::Match,
            "mismatch" => Parity::Mismatch,
            "unknown" => Parity::Unknown,
            _ => return None,
        })
    }
}

/// A finished (or cached) comparison.
#[derive(Clone, Debug)]
pub struct ParityResult {
    pub result: Parity,
    /// One line for people.
    pub detail: String,
    pub typescript: String,
    pub cached: bool,
    /// Time of the user's tsc (0 when cached).
    pub tsc_ms: f64,
    /// [`keys_hash`] of our diagnostics when the comparison ran. A cached result is reused
    /// only while our diagnostics for the project are unchanged; when they change, tsc runs
    /// again.
    pub ours: String,
}

/// The user's TypeScript for a project.
#[derive(Clone, Debug)]
pub struct TsInstall {
    pub version: String,
    /// `node_modules/typescript/bin/tsc`.
    pub tsc: PathBuf,
}

/// The nearest `node_modules/typescript` at or above `dir`.
pub fn find_typescript(dir: &Path) -> Option<TsInstall> {
    for d in dir.ancestors() {
        let pkg = d.join("node_modules").join("typescript");
        let Ok(text) = std::fs::read_to_string(pkg.join("package.json")) else {
            continue;
        };
        let v: Value = serde_json::from_str(&text).ok()?;
        let version = v["version"].as_str()?.to_owned();
        let tsc = pkg.join("bin").join("tsc");
        if tsc.is_file() {
            return Some(TsInstall { version, tsc });
        }
    }
    None
}

/// `TS_R_LINT_PARITY`: `off` skips the check (projects are `Unknown`, on our checker);
/// `require` treats `Unknown` like a mismatch.
pub fn mode() -> Option<String> {
    std::env::var("TS_R_LINT_PARITY").ok()
}

/// Cache key of a project's parity result (`None` when the config cannot be read).
pub fn cache_key(config: &str, inputs: &[String], ts: Option<&TsInstall>) -> Option<String> {
    let mut k = ts_r_cache::KeyBuilder::new("ts-r-lint parity");
    k.u64(PARITY_VERSION as u64)
        .str(env!("CARGO_PKG_VERSION"))
        .str(config);
    for input in inputs {
        k.str(input);
        k.bytes(&std::fs::read(input).ok()?);
    }
    let dir = Path::new(config).parent()?;
    match lockfile(dir) {
        Some((p, bytes)) => {
            k.str(&p.to_string_lossy());
            k.bytes(&bytes);
        }
        None => {
            k.str("no lockfile");
        }
    }
    if let Some(ts) = ts {
        k.str(&ts.version);
    }
    Some(k.key().to_hex()[..32].to_owned())
}

fn lockfile(dir: &Path) -> Option<(PathBuf, Vec<u8>)> {
    for d in dir.ancestors() {
        for name in [
            "pnpm-lock.yaml",
            "package-lock.json",
            "yarn.lock",
            "bun.lock",
            "bun.lockb",
            "npm-shrinkwrap.json",
        ] {
            let p = d.join(name);
            if let Ok(b) = std::fs::read(&p) {
                return Some((p, b));
            }
        }
    }
    None
}

fn parity_dir() -> PathBuf {
    ts_r_cache::cache_root().join("lint").join("parity")
}

pub fn read_cached(key: &str) -> Option<ParityResult> {
    let text = std::fs::read_to_string(parity_dir().join(format!("{key}.json"))).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    Some(ParityResult {
        result: Parity::parse(v["result"].as_str()?)?,
        detail: v["detail"].as_str().unwrap_or("").to_owned(),
        typescript: v["typescript"].as_str().unwrap_or("").to_owned(),
        cached: true,
        tsc_ms: 0.0,
        ours: v["ours"].as_str().unwrap_or("").to_owned(),
    })
}

pub fn write_cached(key: &str, r: &ParityResult) {
    let dir = parity_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let v = json!({
        "result": r.result.as_str(),
        "detail": r.detail,
        "typescript": r.typescript,
        "ours": r.ours,
    });
    let tmp = dir.join(format!("{key}.json.tmp{}", std::process::id()));
    if std::fs::write(&tmp, v.to_string()).is_ok() {
        let _ = std::fs::rename(&tmp, dir.join(format!("{key}.json")));
    }
}

/// Runs the user's tsc on `config` (capped) and returns its comparison keys: absolute,
/// `/`-separated `path(line,col): TScode`, or `TScode` for global diagnostics.
pub fn run_tsc(
    node: &Path,
    wrapper: &[String],
    ts: &TsInstall,
    config: &str,
    incremental: bool,
) -> Result<BTreeMap<String, usize>, String> {
    let dir = Path::new(config).parent().unwrap_or(Path::new("/"));
    let mut argv: Vec<String> = wrapper.to_vec();
    argv.push(node.to_string_lossy().into_owned());
    argv.push(ts.tsc.to_string_lossy().into_owned());
    argv.extend(
        ["-p", config, "--noEmit", "--pretty", "false"]
            .iter()
            .map(|s| s.to_string()),
    );
    let tmp_info = std::env::temp_dir().join(format!(
        "ts-r-lint-parity-{}-{}.tsbuildinfo",
        std::process::id(),
        ts_r_cache::Key::of(config.as_bytes()).to_hex()
    ));
    if incremental {
        // Never write the project's build info.
        argv.push("--tsBuildInfoFile".into());
        argv.push(tmp_info.to_string_lossy().into_owned());
    }
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", argv[0]))?;
    let pid = child.id();
    let mut stdout = child.stdout.take().ok_or("no stdout")?;
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = std::io::Read::read_to_string(&mut stdout, &mut s);
        s
    });
    let start = Instant::now();
    let mut last = Instant::now();
    let status = loop {
        if let Ok(Some(st)) = child.try_wait() {
            break st;
        }
        let over = if start.elapsed() > TSC_TIME {
            Some(format!("ran longer than {} s", TSC_TIME.as_secs()))
        } else if last.elapsed() >= Duration::from_millis(250) {
            last = Instant::now();
            rss_kb(pid)
                .filter(|kb| *kb > TSC_MEM_MB * 1024)
                .map(|_| format!("used more than {TSC_MEM_MB} MB"))
        } else {
            None
        };
        if let Some(why) = over {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_file(&tmp_info);
            return Err(why);
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let _ = std::fs::remove_file(&tmp_info);
    let text = reader.join().unwrap_or_default();
    match status.code() {
        Some(0..=2) => Ok(keys(&text, dir)),
        other => Err(format!("tsc exited with {other:?}")),
    }
}

fn rss_kb(pid: u32) -> Option<u64> {
    if !cfg!(unix) {
        return None;
    }
    let out = Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

/// Comparison keys of tsc's `--pretty false` output; relative paths are resolved against
/// `dir`. Continuation lines (message chains) are skipped, except the first line under a
/// `TS2345`, which may add an [`ALT`] key (see the module docs).
pub fn keys(text: &str, dir: &Path) -> BTreeMap<String, usize> {
    let mut out = BTreeMap::new();
    // The location of the last `TS2345` line while its first chain line is still ahead.
    let mut arg_head: Option<String> = None;
    for line in text.lines() {
        if line.starts_with(' ') {
            if let Some(loc) = arg_head.take()
                && let Some(code) = missing_property_code(line.trim_start())
            {
                *out.entry(format!("{ALT}{loc}: TS{code}")).or_insert(0) += 1;
            }
            continue;
        }
        arg_head = None;
        if line.is_empty() {
            continue;
        }
        let Some(k) = key_of(line, dir) else { continue };
        if let Some(loc) = k.strip_suffix(": TS2345") {
            arg_head = Some(loc.to_owned());
        }
        *out.entry(k).or_insert(0) += 1;
    }
    out
}

/// The code of a missing-property message (`TS2741`, `TS2739`, `TS2740`), by its text.
fn missing_property_code(msg: &str) -> Option<u32> {
    if msg.starts_with("Property '")
        && msg.contains("' is missing in type '")
        && msg.contains("' but required in type '")
    {
        return Some(2741);
    }
    if msg.starts_with("Type '")
        && msg.contains("' is missing the following properties from type '")
    {
        let more = msg.ends_with(" more.") && msg.contains(", and ");
        return Some(if more { 2740 } else { 2739 });
    }
    None
}

/// The major version of a `x.y.z` TypeScript version (`None` when it does not parse).
fn major(version: &str) -> Option<u32> {
    version.split('.').next()?.parse().ok()
}

fn key_of(line: &str, dir: &Path) -> Option<String> {
    let code_at = line.find(" TS")?;
    let code: String = line[code_at + 3..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    if code.is_empty() {
        return None;
    }
    let head = &line[..code_at];
    let loc = ["error", "warning", "message"]
        .iter()
        .find_map(|w| head.strip_suffix(w))
        .unwrap_or(head)
        .trim_end()
        .trim_end_matches(':');
    if loc.is_empty() {
        return Some(format!("TS{code}"));
    }
    let (path, pos) = match loc.rfind('(') {
        Some(i) => (&loc[..i], &loc[i..]),
        None => (loc, ""),
    };
    let abs = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        crate::project::normalize(&dir.join(path))
    };
    Some(format!(
        "{}{pos}: TS{code}",
        abs.to_string_lossy().replace('\\', "/")
    ))
}

/// Compares our keys with tsc's.
pub fn compare(
    ours: &BTreeMap<String, usize>,
    theirs: &BTreeMap<String, usize>,
    version: &str,
) -> ParityResult {
    let excess = |a: &BTreeMap<String, usize>, b: &BTreeMap<String, usize>| {
        a.iter()
            .filter(|(k, _)| !k.starts_with(ALT))
            .filter_map(|(k, &n)| {
                let m = b.get(k).copied().unwrap_or(0);
                (n > m).then(|| (k.clone(), n - m))
            })
            .collect::<BTreeMap<String, usize>>()
    };
    let mut ours_x = excess(ours, theirs);
    let mut theirs_x = excess(theirs, ours);
    let mut paired = 0;
    if major(version).is_some_and(|m| m < 7) {
        for (alt, &n) in theirs.range(ALT.to_owned()..) {
            let Some(key) = alt.strip_prefix(ALT) else {
                break;
            };
            let Some(loc) = key.rsplit_once(": TS").map(|(loc, _)| loc) else {
                continue;
            };
            let arg = format!("{loc}: TS2345");
            let k = n
                .min(ours_x.get(key).copied().unwrap_or(0))
                .min(theirs_x.get(&arg).copied().unwrap_or(0));
            if k > 0
                && let Some(o) = ours_x.get_mut(key)
                && let Some(t) = theirs_x.get_mut(&arg)
            {
                *o -= k;
                *t -= k;
                paired += k;
            }
        }
    }
    let only_ours: usize = ours_x.values().sum();
    let only_theirs: usize = theirs_x.values().sum();
    let first = ours_x
        .iter()
        .find(|(_, n)| **n > 0)
        .map(|(k, _)| format!("only ts-r: {k}"))
        .or_else(|| {
            theirs_x
                .iter()
                .find(|(_, n)| **n > 0)
                .map(|(k, _)| format!("only TypeScript: {k}"))
        });
    if only_ours == 0 && only_theirs == 0 {
        let total: usize = ours
            .iter()
            .filter(|(k, _)| !k.starts_with(ALT))
            .map(|(_, n)| n)
            .sum();
        let older = if paired > 0 {
            format!(" ({paired} with the headline TypeScript 7 gives the same error)")
        } else {
            String::new()
        };
        return ParityResult {
            result: Parity::Match,
            detail: format!("same {total} diagnostic(s) as TypeScript {version}{older}"),
            typescript: version.to_owned(),
            cached: false,
            tsc_ms: 0.0,
            ours: keys_hash(ours),
        };
    }
    ParityResult {
        result: Parity::Mismatch,
        detail: format!(
            "{only_ours} diagnostic(s) only ts-r reports, {only_theirs} only TypeScript {version} reports (first: {})",
            first.unwrap_or_default()
        ),
        typescript: version.to_owned(),
        cached: false,
        tsc_ms: 0.0,
        ours: keys_hash(ours),
    }
}

/// A short hash of a diagnostic key multiset.
pub fn keys_hash(keys: &BTreeMap<String, usize>) -> String {
    let mut k = ts_r_cache::KeyBuilder::new("ts-r-lint parity keys");
    for (key, n) in keys {
        k.str(key).u64(*n as u64);
    }
    k.key().to_hex()[..32].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_resolve_paths() {
        let k = keys(
            "src/a.ts(1,2): error TS2322: Type 'x'\n  chain\nerror TS5023: Unknown option\n",
            Path::new("/p"),
        );
        assert_eq!(k.get("/p/src/a.ts(1,2): TS2322"), Some(&1));
        assert_eq!(k.get("TS5023"), Some(&1));
        assert_eq!(k.len(), 2);
    }

    const TS6_ARG: &str = "a.ts(3,5): error TS2345: Argument of type '{}' is not assignable to parameter of type 'P'.\n  Property 'x' is missing in type '{}' but required in type 'P'.\n";
    const TS7_HEAD: &str =
        "a.ts(3,5): error TS2741: Property 'x' is missing in type '{}' but required in type 'P'.\n";

    #[test]
    fn older_typescript_argument_headline_pairs() {
        let dir = Path::new("/p");
        let theirs = keys(TS6_ARG, dir);
        assert_eq!(theirs.get("alt:/p/a.ts(3,5): TS2741"), Some(&1));
        let ours = keys(TS7_HEAD, dir);
        let r = compare(&ours, &theirs, "6.0.3");
        assert_eq!(r.result, Parity::Match, "{}", r.detail);
        assert!(r.detail.starts_with("same 1 diagnostic(s)"), "{}", r.detail);
        // TypeScript 7 itself must agree exactly.
        assert_eq!(compare(&ours, &theirs, "7.0.2").result, Parity::Mismatch);
        // Elsewhere, or a different missing-property code, is still a mismatch.
        let moved = keys(&TS7_HEAD.replace("(3,5)", "(3,6)"), dir);
        assert_eq!(compare(&moved, &theirs, "6.0.3").result, Parity::Mismatch);
        let other = keys(&TS7_HEAD.replace("TS2741", "TS2739"), dir);
        assert_eq!(compare(&other, &theirs, "6.0.3").result, Parity::Mismatch);
    }

    #[test]
    fn missing_property_codes() {
        let c = missing_property_code;
        assert_eq!(
            c("Type '{}' is missing the following properties from type 'R': a, b"),
            Some(2739)
        );
        assert_eq!(
            c(
                "Type '{}' is missing the following properties from type 'R': a, b, c, d, and 14 more."
            ),
            Some(2740)
        );
        assert_eq!(c("Type 'A' is not assignable to type 'B'."), None);
    }
}
