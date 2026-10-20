//! Runs `ts-r-check` over tsc 7's command-line scenario baselines (design record 0012, E3,
//! E4): `tsc/` (`tsc -p` style runs, `--incremental`, project references), `tsbuild/`
//! (`tsc -b`), and the `--watch` sessions of `tscWatch/` and `tsbuildWatch/`, under
//! `target/upstream/typescript/tsc/testdata/baselines/reference`.
//!
//! ```text
//! cargo build --release -j 3 -p ts_r_check --bin ts-r-check --example scenario_oracle
//! scripts/limit.sh --mem-mb 4096 --time-s 1800 -- target/release/examples/scenario_oracle \
//!   [--bin target/release/ts-r-check] [--threads 3] [--show] [--verbose] [filter...]
//! ```
//!
//! Each baseline lists a virtual file system (`Input::`), then steps: the initial run and
//! `Edit [n]::` steps that change, delete or touch files first. Every step names the command
//! line (`tsgo <args>`), the exit status, the text printed and every file written, with
//! markers `*new*`, `*modified*`, `*rewrite with same content*`, `*mTime changed*`,
//! `*deleted*` (and `*Lib*` for the fake lib files the harness serves on demand).
//!
//! The harness writes the file system under a fresh directory (`target/e3-scenarios/<n>`),
//! serves the scenario's fake lib (`/home/src/tslibs/TS/Lib`, every lib name) through
//! `TS_R_LIB_DIR`, gives each scenario its own `TS_R_CACHE_DIR`, sets `FORCE_COLOR=1` (tsc's
//! scenario harness prints pretty output, as tsc does with `FORCE_COLOR`) and runs each step's
//! command in the scenario's current directory. A step matches when:
//!
//! - the exit code equals the status (`Success` 0, `DiagnosticsPresent_OutputsSkipped` 1,
//!   `DiagnosticsPresent_OutputsGenerated` 2, `InvalidProject_OutputsSkipped` 3,
//!   `ProjectReferenceCycle_OutputsSkipped` 4);
//! - the printed text is equal after normalisation: the temporary root is removed from
//!   paths, clock times become `HH:MM:SS AM`, `Version 7.0.2 (ts-r x)` becomes tsc's test
//!   version `FakeTSVersion`, and trailing blank lines are ignored;
//! - the set of written files is equal, by path and kind: new, modified (content changed),
//!   touched (same content, newer modification time: tsc's `rewrite with same content` and
//!   `mTime changed`, which a file system cannot tell apart) and deleted;
//! - written files have the same content, except `.tsbuildinfo` files (ts-r writes its own
//!   schema there, record 0012 decision 1): for them only written or deleted counts.
//!
//! A watch scenario (first command line with `-w`) is one session under the stdin driver
//! (`TS_R_WATCH_DRIVER=stdin`): each `Edit [n]::` applies its changes, sends one line (one
//! look for changes) and compares what was printed up to the cycle sentinel and the files
//! written; edits have no exit status. `Watch Registrations::` is not compared. Every wait
//! has a 60 s timeout, and the session is killed at the end.
//!
//! A scenario matches when every step does. Mismatches are classified by their first
//! difference, and scenarios that need features of other phases are counted apart:
//! declaration output (`.d.ts` written, phase E2) and E4's reporting flags
//! (`--explainFiles`, `--traceResolution`, `--listFiles`, ...). Details go to
//! `target/scenario-oracle.txt`; `--show` prints the first mismatch of each scenario.

use std::collections::BTreeMap;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime};

ts_r_testing::cap_memory!(2048);

const LIB_DIR: &str = "/home/src/tslibs/TS/Lib";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Marker {
    New,
    Modified,
    /// `*rewrite with same content*` and `*mTime changed*`.
    Touched,
    Deleted,
    Lib,
}

#[derive(Clone, Debug)]
struct Entry {
    path: String,
    marker: Marker,
    /// A symbolic link to this path (`//// [p] -> target *new*`).
    link: Option<String>,
    content: String,
    /// The content's trailing newlines are unknown (the last entry before the end of a step).
    loose: bool,
}

#[derive(Debug)]
struct Step {
    title: String,
    changes: Vec<Entry>,
    args: Vec<String>,
    exit: String,
    output: String,
    written: Vec<Entry>,
}

#[derive(Debug)]
struct Scenario {
    name: String,
    cwd: String,
    steps: Vec<Step>,
    libs: BTreeMap<String, String>,
    /// A `--watch` session: the first step starts it, each edit is one look for changes.
    watch: bool,
}

fn marker_of(s: &str) -> Option<Marker> {
    Some(match s.trim() {
        "*new*" => Marker::New,
        "*modified*" => Marker::Modified,
        "*rewrite with same content*" | "*mTime changed*" => Marker::Touched,
        "*deleted*" => Marker::Deleted,
        "*Lib*" => Marker::Lib,
        _ => return None,
    })
}

/// Line starts of `//// [` headers in `text`.
fn header_starts(text: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(p) = text[at..].find("//// [") {
        let i = at + p;
        if i == 0 || text.as_bytes()[i - 1] == b'\n' {
            out.push(i);
        }
        at = i + 6;
    }
    out
}

/// Parses `//// [path] marker` entries. `tail_newlines`: how many newlines separate the last
/// entry's content from what follows (`None`: unknown, the content is trimmed).
fn parse_entries(text: &str, tail_newlines: Option<usize>) -> Vec<Entry> {
    let starts = header_starts(text);
    let mut out = Vec::new();
    for (k, &s) in starts.iter().enumerate() {
        let end = starts.get(k + 1).copied().unwrap_or(text.len());
        let chunk = &text[s..end];
        let (header, body) = match chunk.find('\n') {
            Some(n) => (&chunk[..n], &chunk[n + 1..]),
            None => (chunk, ""),
        };
        let Some(close) = header.find("] ").or_else(|| header.rfind(']')) else {
            continue;
        };
        let path = header[6..close].to_owned();
        let mut rest = header[close + 1..].trim();
        let mut link = None;
        if let Some(r) = rest.strip_prefix("-> ") {
            let (target, m) = r.rsplit_once(' ').unwrap_or((r, ""));
            link = Some(target.to_owned());
            rest = m;
        }
        let Some(marker) = marker_of(rest) else {
            continue;
        };
        let last = k + 1 == starts.len();
        let mut content = body.to_owned();
        let mut loose = false;
        let strip = if last { tail_newlines } else { Some(1) };
        match strip {
            Some(n) => {
                for _ in 0..n {
                    if content.ends_with('\n') {
                        content.pop();
                    }
                }
            }
            None => {
                while content.ends_with('\n') {
                    content.pop();
                }
                loose = true;
            }
        }
        out.push(Entry {
            path,
            marker,
            link,
            content,
            loose,
        });
    }
    out
}

fn parse_scenario(name: &str, text: &str) -> Option<Scenario> {
    let text = text.replace("\r\n", "\n");
    let mut lines = text.splitn(4, '\n');
    let cwd = lines.next()?.strip_prefix("currentDirectory::")?.to_owned();
    lines.next()?; // useCaseSensitiveFileNames
    if lines.next()? != "Input::" {
        return None;
    }
    let body = lines.next().unwrap_or("");
    // Step blocks: the first, then one per `Edit [n]::` line.
    let mut blocks: Vec<(String, &str)> = Vec::new();
    let mut title = String::new();
    let mut start = 0;
    let mut at = 0;
    while at < body.len() {
        let line_end = body[at..].find('\n').map_or(body.len(), |n| at + n);
        let line = &body[at..line_end];
        if line.starts_with("Edit [") && line.contains("]:: ") && at > 0 {
            blocks.push((std::mem::take(&mut title), &body[start..at]));
            title = line.split_once("]:: ").map_or("", |x| x.1).to_owned();
            start = (line_end + 1).min(body.len());
        }
        at = line_end + 1;
    }
    blocks.push((title, &body[start.min(body.len())..]));
    let mut steps = Vec::new();
    let mut libs = BTreeMap::new();
    let mut watch = false;
    for (k, (title, block)) in blocks.into_iter().enumerate() {
        // The command line; a watch session's edits have none (the session goes on).
        let cmd_at = if block.starts_with("tsgo") {
            Some(0)
        } else {
            block.find("\ntsgo").map(|n| n + 1)
        };
        let (changes_text, args, exit, rest) = match cmd_at {
            Some(cmd_at) => {
                let rest = &block[cmd_at..];
                let (cmd, rest) = rest.split_once('\n')?;
                let args: Vec<String> = cmd
                    .strip_prefix("tsgo")?
                    .split(' ')
                    .filter(|a| !a.is_empty())
                    .map(str::to_owned)
                    .collect();
                if k == 0 {
                    watch = args.iter().any(|a| {
                        let a = a.to_ascii_lowercase();
                        a == "-w" || a == "--watch"
                    });
                }
                let (exit, rest) = rest.split_once('\n')?;
                let exit = exit.strip_prefix("ExitStatus:: ")?.trim().to_owned();
                (&block[..cmd_at], args, exit, rest)
            }
            None if watch && k > 0 => {
                let at = if block.starts_with("Output::\n") {
                    0
                } else {
                    block.find("\nOutput::\n")? + 1
                };
                (&block[..at], Vec::new(), String::new(), &block[at..])
            }
            None => return None,
        };
        let changes = parse_entries(changes_text, Some(2));
        let rest = rest.strip_prefix("Output::\n").unwrap_or(rest);
        // The per-project trailer (`tsconfig.json::\nSemanticDiagnostics::`), and a watch
        // session's (`Watch Registrations::`) before it. Without a config (`tsc -i a.ts`)
        // there is no project line: the trailer starts at `SemanticDiagnostics::`.
        let trailer = rest.find("\nSemanticDiagnostics::").map(|p| {
            let line = rest[..p].rfind('\n').map_or(0, |n| n + 1);
            if line == p { p + 1 } else { line }
        });
        let registrations = if rest.starts_with("Watch Registrations::") {
            Some(0)
        } else {
            rest.find("\nWatch Registrations::").map(|p| p + 1)
        };
        let trailer = match (trailer, registrations) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let main = &rest[..trailer.unwrap_or(rest.len())];
        let first_header = header_starts(main).first().copied();
        let output = main[..first_header.unwrap_or(main.len())].to_owned();
        let written = match first_header {
            Some(h) => parse_entries(&main[h..], trailer.map(|_| 2)),
            None => Vec::new(),
        };
        for e in &written {
            if e.marker == Marker::Lib {
                libs.insert(e.path.clone(), e.content.clone());
            }
        }
        steps.push(Step {
            title,
            changes,
            args,
            exit,
            output,
            written,
        });
    }
    Some(Scenario {
        name: name.to_owned(),
        cwd,
        steps,
        libs,
        watch,
    })
}

fn exit_code(status: &str) -> Option<i32> {
    Some(match status {
        "Success" => 0,
        "DiagnosticsPresent_OutputsSkipped" => 1,
        "DiagnosticsPresent_OutputsGenerated" => 2,
        "InvalidProject_OutputsSkipped" => 3,
        "ProjectReferenceCycle_OutputsSkipped" => 4,
        _ => return None,
    })
}

/// `HH:MM:SS AM` for clock times like `9:05:03 PM` or `12:00:00 AM`.
fn normalize_times(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        // [d]d:dd:dd [AP]M
        let try_at = |i: usize| -> Option<usize> {
            let mut j = i;
            let d0 = j;
            while j < b.len() && b[j].is_ascii_digit() && j - d0 < 2 {
                j += 1;
            }
            if j == d0 {
                return None;
            }
            for _ in 0..2 {
                if b.get(j) != Some(&b':') {
                    return None;
                }
                j += 1;
                if !(b.get(j)?.is_ascii_digit() && b.get(j + 1)?.is_ascii_digit()) {
                    return None;
                }
                j += 2;
            }
            if b.get(j) != Some(&b' ') {
                return None;
            }
            if !matches!(b.get(j + 1), Some(b'A' | b'P')) || b.get(j + 2) != Some(&b'M') {
                return None;
            }
            Some(j + 3)
        };
        let boundary = i == 0 || !b[i - 1].is_ascii_digit();
        if boundary && let Some(end) = try_at(i) {
            out.push_str("HH:MM:SS AM");
            i = end;
            continue;
        }
        let ch = s[i..].chars().next().unwrap_or(' ');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// `--traceResolution` walks up to the real file system's root, past the scenario's (`root`):
/// drops the lookups outside `root`.
fn drop_outside_lookups(s: &str, root: &str) -> String {
    let outside = |l: &str| {
        (l.starts_with("File '") || l.starts_with("Directory '") || l.starts_with("Found '"))
            && l.split('\'')
                .skip(1)
                .step_by(2)
                .any(|p| p.starts_with('/') && !p.starts_with(root))
    };
    // Type roots (`/a/node_modules/@types,/node_modules/@types`) in the type reference header
    // and the primary search path line.
    let roots = |list: &str, sep: &str| {
        list.split(sep)
            .filter(|r| r.starts_with(root) || !r.starts_with('/'))
            .collect::<Vec<_>>()
            .join(sep)
    };
    s.split('\n')
        .filter(|l| !outside(l))
        .map(|l| {
            if let Some(rest) = l.strip_prefix("Resolving with primary search path '")
                && let Some(list) = rest.strip_suffix("'.")
            {
                format!(
                    "Resolving with primary search path '{}'.",
                    roots(list, ", ")
                )
            } else if l.starts_with("======== Resolving type reference directive")
                && let Some(i) = l.find(", root directory '")
                && let Some(list) = l[i + ", root directory '".len()..].strip_suffix("'. ========")
            {
                format!(
                    "{}, root directory '{}'. ========",
                    &l[..i],
                    roots(list, ",")
                )
            } else {
                l.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn normalize_output(s: &str, root: &str) -> String {
    // Only our output runs in a real directory (the expected side passes no root).
    let s = if root.starts_with('/') {
        drop_outside_lookups(s, root)
    } else {
        s.to_owned()
    };
    let mut t = s.replace(root, "");
    t = normalize_times(&t);
    // `Version 7.0.2 (ts-r 0.0.0)` -> tsc's test version.
    while let Some(p) = t.find("Version 7.0.2") {
        let mut end = p + "Version 7.0.2".len();
        if t[end..].starts_with(" (ts-r ")
            && let Some(close) = t[end..].find(')')
        {
            end += close + 1;
        }
        t.replace_range(p..end, "Version FakeTSVersion");
    }
    // The buildinfo version (`ts-r 0.0.0 (tsc 7.0.2)`) in status messages.
    while let Some(p) = t.find("ts-r ") {
        let Some(close) = t[p..].find("(tsc 7.0.2)") else {
            break;
        };
        t.replace_range(p..p + close + "(tsc 7.0.2)".len(), "FakeTSVersion");
    }
    // `--traceResolution`: the version `typesVersions` entries are matched against.
    t = t.replace(
        "matches compiler version '7.0.2'",
        "matches compiler version 'FakeTSVersion'",
    );
    let trimmed: Vec<&str> = t.lines().map(|l| l.trim_end()).collect();
    let mut t = trimmed.join("\n");
    while t.ends_with('\n') {
        t.pop();
    }
    t
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Snap {
    content: Option<Vec<u8>>,
    link: Option<String>,
    mtime: Option<SystemTime>,
}

fn snapshot(root: &Path) -> BTreeMap<String, Snap> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            let Ok(meta) = std::fs::symlink_metadata(&p) else {
                continue;
            };
            let rel = p
                .strip_prefix(root)
                .map(|r| format!("/{}", r.to_string_lossy().replace('\\', "/")))
                .unwrap_or_default();
            if rel.starts_with(LIB_DIR) {
                continue;
            }
            if meta.file_type().is_symlink() {
                let target = std::fs::read_link(&p)
                    .map(|t| t.to_string_lossy().into_owned())
                    .ok();
                out.insert(
                    rel,
                    Snap {
                        content: None,
                        link: target,
                        mtime: None,
                    },
                );
            } else if meta.is_dir() {
                stack.push(p);
            } else {
                out.insert(
                    rel,
                    Snap {
                        content: std::fs::read(&p).ok(),
                        link: None,
                        mtime: meta.modified().ok(),
                    },
                );
            }
        }
    }
    out
}

fn host(root: &Path, p: &str) -> PathBuf {
    root.join(p.trim_start_matches('/'))
}

fn apply(root: &Path, e: &Entry) -> std::io::Result<()> {
    let path = host(root, &e.path);
    match e.marker {
        Marker::Deleted => {
            let _ = std::fs::remove_file(&path);
            Ok(())
        }
        Marker::Touched => {
            if path.exists() {
                let f = std::fs::OpenOptions::new().write(true).open(&path)?;
                f.set_modified(SystemTime::now())?;
            }
            Ok(())
        }
        _ => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            if let Some(target) = &e.link {
                let _ = std::fs::remove_file(&path);
                #[cfg(unix)]
                std::os::unix::fs::symlink(host(root, target), &path)?;
                // Symlinked scenario files are only recreated on Unix.
                #[cfg(not(unix))]
                let _ = target;
                return Ok(());
            }
            std::fs::write(&path, &e.content)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    New,
    Modified,
    Touched,
    Deleted,
}

fn is_buildinfo(path: &str, expected: &[Entry]) -> bool {
    path.ends_with(".tsbuildinfo") || {
        let readable = format!("{path}.readable.baseline.txt");
        expected.iter().any(|e| e.path == readable)
    }
}

/// The first difference of a step, or `None` when it matches.
fn compare_step(
    step: &Step,
    code: i32,
    stdout: &str,
    before: &BTreeMap<String, Snap>,
    after: &BTreeMap<String, Snap>,
    root: &str,
) -> Option<(String, String)> {
    let want_code = exit_code(&step.exit);
    let want_out = normalize_output(&step.output, "\u{0}");
    let got_out = normalize_output(stdout, root);
    if want_out != got_out {
        return Some((
            "output".into(),
            format!("--- expected\n{want_out}\n--- actual\n{got_out}\n"),
        ));
    }
    // A watch session's edits have no exit status.
    if !step.exit.is_empty() && want_code != Some(code) {
        return Some((
            "exit".into(),
            format!("expected {} ({want_code:?}), got {code}", step.exit),
        ));
    }
    // Written files.
    let mut got: BTreeMap<&str, Kind> = BTreeMap::new();
    for (p, s) in after {
        match before.get(p) {
            None => {
                got.insert(p, Kind::New);
            }
            Some(b) if b.content != s.content || b.link != s.link => {
                got.insert(p, Kind::Modified);
            }
            Some(b) if b.mtime != s.mtime => {
                got.insert(p, Kind::Touched);
            }
            _ => {}
        }
    }
    for p in before.keys() {
        if !after.contains_key(p) {
            got.insert(p, Kind::Deleted);
        }
    }
    let mut want: BTreeMap<&str, (Kind, &Entry)> = BTreeMap::new();
    for e in &step.written {
        let k = match e.marker {
            Marker::Lib => continue,
            _ if e.path.ends_with(".readable.baseline.txt") => continue,
            Marker::New => Kind::New,
            Marker::Modified => Kind::Modified,
            Marker::Touched => Kind::Touched,
            Marker::Deleted => Kind::Deleted,
        };
        want.insert(&e.path, (k, e));
    }
    let mut diffs = Vec::new();
    let keys: std::collections::BTreeSet<&str> =
        got.keys().copied().chain(want.keys().copied()).collect();
    for p in keys {
        let bi = is_buildinfo(p, &step.written);
        let norm = |k: Kind| {
            if bi && k != Kind::Deleted {
                Kind::New
            } else {
                k
            }
        };
        match (want.get(p), got.get(p)) {
            (Some((wk, _)), None) => diffs.push(format!("missing {wk:?} {p}")),
            (None, Some(gk)) => diffs.push(format!("extra {gk:?} {p}")),
            (Some((wk, e)), Some(gk)) => {
                if norm(*wk) != norm(*gk) {
                    diffs.push(format!("kind {p}: expected {wk:?}, got {gk:?}"));
                } else if !bi && matches!(wk, Kind::New | Kind::Modified) && e.link.is_none() {
                    let actual = after
                        .get(p)
                        .and_then(|s| s.content.as_deref())
                        .map(|b| String::from_utf8_lossy(b).into_owned())
                        .unwrap_or_default();
                    let same = if e.loose {
                        actual.trim_end_matches('\n') == e.content
                    } else {
                        actual == e.content
                    };
                    if !same {
                        diffs.push(format!(
                            "content {p}\n--- expected\n{}\n--- actual\n{actual}",
                            e.content
                        ));
                    }
                }
            }
            (None, None) => {}
        }
    }
    if diffs.is_empty() {
        None
    } else {
        let kind = if diffs.iter().any(|d| d.starts_with("content")) {
            "content"
        } else {
            "files"
        };
        Some((kind.into(), diffs.join("\n")))
    }
}

struct Outcome {
    name: String,
    /// `None`: matched; else (step index, kind, detail).
    mismatch: Option<(usize, String, String)>,
    needs_dts: bool,
    needs_e4: bool,
    ms: u128,
}

const E4_FLAGS: &[&str] = &[
    "--explainfiles",
    "--traceresolution",
    "--listfiles",
    "--listfilesonly",
    "--diagnostics",
    "--extendeddiagnostics",
    "--generatetrace",
];

fn run_scenario(
    sc: &Scenario,
    bin: &Path,
    work: &Path,
    fake_lib: &str,
    lib_names: &[String],
) -> Outcome {
    let t = Instant::now();
    let needs_dts = sc.steps.iter().any(|s| {
        s.written.iter().any(|e| {
            matches!(e.marker, Marker::New | Marker::Modified)
                && (e.path.ends_with(".d.ts")
                    || e.path.ends_with(".d.mts")
                    || e.path.ends_with(".d.cts")
                    || e.path.ends_with(".d.ts.map"))
        })
    });
    let needs_e4 = sc.steps.iter().any(|s| {
        s.args
            .iter()
            .any(|a| E4_FLAGS.contains(&a.to_ascii_lowercase().as_str()))
    });
    let mut outcome = Outcome {
        name: sc.name.clone(),
        mismatch: None,
        needs_dts,
        needs_e4,
        ms: 0,
    };
    let _ = std::fs::remove_dir_all(work);
    let root = work.join("root");
    let cache = work.join("cache");
    let setup = || -> std::io::Result<()> {
        std::fs::create_dir_all(&root)?;
        std::fs::create_dir_all(&cache)?;
        let lib_dir = host(&root, LIB_DIR);
        std::fs::create_dir_all(&lib_dir)?;
        for n in lib_names {
            std::fs::write(lib_dir.join(n), fake_lib)?;
        }
        for (p, text) in &sc.libs {
            std::fs::write(host(&root, p), text)?;
        }
        std::fs::create_dir_all(host(&root, &sc.cwd))?;
        Ok(())
    };
    if let Err(e) = setup() {
        outcome.mismatch = Some((0, "harness".into(), e.to_string()));
        return outcome;
    }
    let root_str = std::fs::canonicalize(&root)
        .unwrap_or(root.clone())
        .to_string_lossy()
        .replace('\\', "/");
    let root = PathBuf::from(&root_str);
    if sc.watch {
        outcome.mismatch = run_watch(sc, bin, &root, &root_str, &cache);
        outcome.ms = t.elapsed().as_millis();
        if std::env::var_os("SCENARIO_KEEP").is_none() {
            let _ = std::fs::remove_dir_all(work);
        }
        return outcome;
    }
    for (i, step) in sc.steps.iter().enumerate() {
        for c in &step.changes {
            if let Err(e) = apply(&root, c) {
                outcome.mismatch = Some((i, "harness".into(), format!("{}: {e}", c.path)));
                return outcome;
            }
        }
        let before = snapshot(&root);
        let mut child = match command(bin, &root, &sc.cwd, &step.args, &cache)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                outcome.mismatch = Some((i, "harness".into(), e.to_string()));
                return outcome;
            }
        };
        let mut stdout = child.stdout.take();
        let mut stderr = child.stderr.take();
        let reader = std::thread::spawn(move || {
            let mut s = Vec::new();
            if let Some(o) = stdout.as_mut() {
                let _ = o.read_to_end(&mut s);
            }
            s
        });
        let ereader = std::thread::spawn(move || {
            let mut s = Vec::new();
            if let Some(o) = stderr.as_mut() {
                let _ = o.read_to_end(&mut s);
            }
            s
        });
        let deadline = Instant::now() + Duration::from_secs(60);
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break Some(s),
                Ok(None) if Instant::now() > deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(2)),
                Err(_) => break None,
            }
        };
        let out = reader.join().unwrap_or_default();
        let err = ereader.join().unwrap_or_default();
        let Some(status) = status else {
            outcome.mismatch = Some((i, "timeout".into(), String::new()));
            return outcome;
        };
        let code = status.code().unwrap_or(-1);
        if !(0..=4).contains(&code) {
            outcome.mismatch = Some((
                i,
                "crash".into(),
                format!("exit {code}\n{}", String::from_utf8_lossy(&err)),
            ));
            return outcome;
        }
        let after = snapshot(&root);
        let stdout = String::from_utf8_lossy(&out);
        if let Some((kind, detail)) = compare_step(step, code, &stdout, &before, &after, &root_str)
        {
            outcome.mismatch = Some((i, kind, detail));
            break;
        }
    }
    outcome.ms = t.elapsed().as_millis();
    // `SCENARIO_KEEP=1` leaves the last scenario's tree for debugging.
    if std::env::var_os("SCENARIO_KEEP").is_none() {
        let _ = std::fs::remove_dir_all(work);
    }
    outcome
}

/// The command for `args` in the scenario's working directory (created when missing).
fn command(bin: &Path, root: &Path, cwd: &str, args: &[String], cache: &Path) -> Command {
    let cwd = host(root, cwd);
    let _ = std::fs::create_dir_all(&cwd);
    // Absolute paths in the command line live under the scenario root.
    let args: Vec<String> = args
        .iter()
        .map(|a| {
            if a.starts_with('/') {
                host(root, a).to_string_lossy().into_owned()
            } else {
                a.clone()
            }
        })
        .collect();
    let mut c = Command::new(bin);
    c.args(&args)
        .current_dir(&cwd)
        .env("PWD", &cwd)
        .env("FORCE_COLOR", "1")
        .env_remove("NO_COLOR")
        .env("TS_R_LIB_DIR", host(root, LIB_DIR))
        .env("TS_R_CACHE_DIR", cache)
        .env("TS_R_MEM_CAP_MB", "1024");
    c
}

/// The line `ts-r-check --watch` prints after each look for changes under
/// `TS_R_WATCH_DRIVER=stdin` (followed by the status and a newline).
const CYCLE_SENTINEL: &[u8] = b"\x01ts-r-watch-cycle ";

/// Runs a watch session: the first step starts `ts-r-check` with the stdin driver; each
/// edit applies its changes, then asks for one look (a line on stdin) and waits for the
/// sentinel. Deterministic: no file-system events are involved. Every wait has a timeout
/// and the process is killed when the session ends.
fn run_watch(
    sc: &Scenario,
    bin: &Path,
    root: &Path,
    root_str: &str,
    cache: &Path,
) -> Option<(usize, String, String)> {
    let first = sc.steps.first()?;
    for c in &first.changes {
        if let Err(e) = apply(root, c) {
            return Some((0, "harness".into(), format!("{}: {e}", c.path)));
        }
    }
    let mut before = snapshot(root);
    let mut child = match command(bin, root, &sc.cwd, &first.args, cache)
        .env("TS_R_WATCH_DRIVER", "stdin")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return Some((0, "harness".into(), e.to_string())),
    };
    let mut stdin = child.stdin.take();
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    let mut stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let Some(o) = stdout.as_mut() else { return };
        let mut buf = [0u8; 8192];
        loop {
            match o.read(&mut buf) {
                Ok(0) | Err(_) => return,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        return;
                    }
                }
            }
        }
    });
    let mut stderr = child.stderr.take();
    let ereader = std::thread::spawn(move || {
        let mut s = Vec::new();
        if let Some(o) = stderr.as_mut() {
            let _ = o.read_to_end(&mut s);
        }
        s
    });
    let mut pending: Vec<u8> = Vec::new();
    let mut ended = false;
    // The output up to the next sentinel and its status; `None` status: the process ended.
    let next_cycle = |pending: &mut Vec<u8>,
                      ended: &mut bool|
     -> Result<(Vec<u8>, Option<i32>), String> {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if let Some(at) = pending
                .windows(CYCLE_SENTINEL.len())
                .position(|w| w == CYCLE_SENTINEL)
                && let Some(nl) = pending[at..].iter().position(|&b| b == b'\n')
            {
                let status = String::from_utf8_lossy(&pending[at + CYCLE_SENTINEL.len()..at + nl])
                    .trim()
                    .parse::<i32>()
                    .unwrap_or(-1);
                let out = pending[..at].to_vec();
                pending.drain(..at + nl + 1);
                return Ok((out, Some(status)));
            }
            if *ended {
                return Ok((std::mem::take(pending), None));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err("timeout".into());
            }
            match rx.recv_timeout(left) {
                Ok(chunk) => pending.extend_from_slice(&chunk),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => return Err("timeout".into()),
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => *ended = true,
            }
        }
    };
    let mut result = None;
    for (i, step) in sc.steps.iter().enumerate() {
        if i > 0 {
            for c in &step.changes {
                if let Err(e) = apply(root, c) {
                    result = Some((i, "harness".into(), format!("{}: {e}", c.path)));
                    break;
                }
            }
            if result.is_some() {
                break;
            }
            before = snapshot(root);
            let asked = stdin
                .as_mut()
                .is_some_and(|s| s.write_all(b"\n").and_then(|()| s.flush()).is_ok());
            if !asked {
                result = Some((i, "exited".into(), "the session ended early".into()));
                break;
            }
        }
        let (out, status) = match next_cycle(&mut pending, &mut ended) {
            Ok(x) => x,
            Err(e) => {
                result = Some((i, e, String::new()));
                break;
            }
        };
        let code = match status {
            Some(c) => c,
            None => {
                // The process ended: its exit status stands for the step's.
                let _ = stdin.take();
                let deadline = Instant::now() + Duration::from_secs(10);
                loop {
                    match child.try_wait() {
                        Ok(Some(s)) => break s.code().unwrap_or(-1),
                        Ok(None) if Instant::now() > deadline => {
                            let _ = child.kill();
                            break -1;
                        }
                        Ok(None) => std::thread::sleep(Duration::from_millis(2)),
                        Err(_) => break -1,
                    }
                }
            }
        };
        let after = snapshot(root);
        if !(0..=4).contains(&code) {
            result = Some((i, "crash".into(), format!("exit {code}")));
            break;
        }
        let stdout = String::from_utf8_lossy(&out);
        if let Some((kind, detail)) = compare_step(step, code, &stdout, &before, &after, root_str) {
            result = Some((i, kind, detail));
            break;
        }
        if status.is_none() && i + 1 < sc.steps.len() {
            result = Some((i + 1, "exited".into(), "the session ended early".into()));
            break;
        }
    }
    // End the session: close stdin, wait a little, then kill.
    drop(stdin);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match child.try_wait() {
            Ok(Some(_)) | Err(_) => break,
            Ok(None) if Instant::now() > deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(2)),
        }
    }
    let _ = reader.join();
    let err = ereader.join().unwrap_or_default();
    if let Some((_, kind, detail)) = result.as_mut()
        && kind == "crash"
    {
        detail.push('\n');
        detail.push_str(&String::from_utf8_lossy(&err));
    }
    result
}

fn main() {
    let mut args = std::env::args().skip(1);
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut bin = repo.join("target/release/ts-r-check");
    let mut threads = 3usize;
    let mut show = false;
    let mut verbose = false;
    let mut filters: Vec<String> = Vec::new();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--bin" => bin = PathBuf::from(args.next().expect("--bin PATH")),
            "--threads" => {
                threads = args
                    .next()
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(3)
                    .clamp(1, 4)
            }
            "--show" => show = true,
            "--verbose" => verbose = true,
            _ => filters.push(a),
        }
    }
    let reference = repo.join("target/upstream/typescript/tsc/testdata/baselines/reference");
    let lib_src = repo.join("target/upstream/typescript/tsc/internal/bundled/libs");
    let mut lib_names: Vec<String> = std::fs::read_dir(&lib_src)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.ends_with(".d.ts"))
                .collect()
        })
        .unwrap_or_default();
    lib_names.sort();
    if lib_names.is_empty() || !bin.is_file() {
        eprintln!(
            "need the upstream checkout ({}) and the binary ({})",
            lib_src.display(),
            bin.display()
        );
        std::process::exit(2);
    }
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    for top in ["tsc", "tsbuild", "tscWatch", "tsbuildWatch"] {
        let Ok(rd) = std::fs::read_dir(reference.join(top)) else {
            continue;
        };
        let mut dirs: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
        dirs.sort();
        for d in dirs {
            let Ok(rd) = std::fs::read_dir(&d) else {
                continue;
            };
            let mut fs: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
            fs.sort();
            for f in fs {
                let name = format!(
                    "{top}/{}/{}",
                    d.file_name().unwrap_or_default().to_string_lossy(),
                    f.file_stem().unwrap_or_default().to_string_lossy()
                );
                if filters.is_empty() || filters.iter().any(|x| name.contains(x.as_str())) {
                    files.push((name, f));
                }
            }
        }
    }
    let mut scenarios = Vec::new();
    let mut unparsed = Vec::new();
    for (name, f) in &files {
        let text = std::fs::read_to_string(f).unwrap_or_default();
        match parse_scenario(name, &text) {
            Some(s) => scenarios.push(s),
            None => unparsed.push(name.clone()),
        }
    }
    // The fake lib every scenario serves (the most common `*Lib*` text).
    let mut lib_counts: BTreeMap<&str, usize> = BTreeMap::new();
    for s in &scenarios {
        for t in s.libs.values() {
            *lib_counts.entry(t.as_str()).or_default() += 1;
        }
    }
    let fake_lib = lib_counts
        .iter()
        .max_by_key(|(_, n)| **n)
        .map(|(t, _)| t.to_string())
        .unwrap_or_default();
    let work_root = repo.join("target/e3-scenarios");
    let _ = std::fs::create_dir_all(&work_root);
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<Outcome>> = Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for t in 0..threads {
            let (next, results, scenarios, bin, fake_lib, lib_names, work_root) = (
                &next, &results, &scenarios, &bin, &fake_lib, &lib_names, &work_root,
            );
            s.spawn(move || {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(sc) = scenarios.get(i) else { break };
                    let work = work_root.join(format!("w{t}"));
                    let o = run_scenario(sc, bin, &work, fake_lib, lib_names);
                    if verbose {
                        eprintln!(
                            "{} {} ({} ms)",
                            if o.mismatch.is_none() { "ok  " } else { "FAIL" },
                            o.name,
                            o.ms
                        );
                    }
                    results.lock().unwrap_or_else(|e| e.into_inner()).push(o);
                }
            });
        }
    });
    let mut results = results.into_inner().unwrap_or_else(|e| e.into_inner());
    results.sort_by(|a, b| a.name.cmp(&b.name));
    if std::env::var_os("SCENARIO_KEEP").is_none() {
        let _ = std::fs::remove_dir_all(&work_root);
    }
    let mut report = String::new();
    let mut by_dir: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for o in &results {
        let dir = o.name.rsplit_once('/').map_or("", |x| x.0).to_owned();
        let e = by_dir.entry(dir).or_default();
        e.0 += 1;
        if o.mismatch.is_none() {
            e.1 += 1;
            continue;
        }
        let Some((step, kind, detail)) = o.mismatch.as_ref() else {
            continue;
        };
        let step = *step;
        let sc = scenarios.iter().find(|s| s.name == o.name);
        let cmd = sc
            .and_then(|s| s.steps.get(step))
            .map(|s| format!("[{}] tsgo {}", s.title, s.args.join(" ")))
            .unwrap_or_default();
        let tags = format!(
            "{}{}",
            if o.needs_dts { " [E2: .d.ts]" } else { "" },
            if o.needs_e4 { " [E4 flags]" } else { "" }
        );
        let entry = format!("=== {} step {step} {kind}{tags}\n{cmd}\n{detail}\n", o.name);
        if show {
            print!("{entry}");
        }
        report.push_str(&entry);
    }
    let _ = std::fs::write(repo.join("target/scenario-oracle.txt"), &report);
    println!("Scenarios: {} ({} unparsed)", results.len(), unparsed.len());
    for u in &unparsed {
        println!("  unparsed: {u}");
    }
    for (dir, (n, ok)) in &by_dir {
        println!("  {dir:<55} {ok:>3} / {n:>3}");
    }
    let pct = |a: usize, b: usize| {
        if b == 0 {
            0.0
        } else {
            a as f64 * 100.0 / b as f64
        }
    };
    // tsc/ and tsbuild/ (gate 85%, E3), then tscWatch/ and tsbuildWatch/ (gate 80%, E4).
    for (label, watch, gate) in [("Matched", false, 85), ("Watch matched", true, 80)] {
        let group: Vec<&Outcome> = results
            .iter()
            .filter(|o| o.name.contains("Watch/") == watch)
            .collect();
        if group.is_empty() {
            continue;
        }
        let total = group.len();
        let matched = group.iter().filter(|o| o.mismatch.is_none()).count();
        let mut by_kind: BTreeMap<String, usize> = BTreeMap::new();
        let (mut dts_blocked, mut e4_blocked, mut dts_ok) = (0, 0, 0);
        for o in &group {
            match &o.mismatch {
                None => dts_ok += usize::from(o.needs_dts),
                Some(_) if o.needs_dts => dts_blocked += 1,
                Some(_) if o.needs_e4 => e4_blocked += 1,
                Some((_, kind, _)) => *by_kind.entry(kind.clone()).or_default() += 1,
            }
        }
        println!(
            "{label}: {matched} / {total} ({:.1}%; gate >= {gate}%)",
            pct(matched, total)
        );
        println!(
            "Not matched: {} (needs .d.ts output, E2: {dts_blocked}; uses E4 flags: {e4_blocked}; other: {})",
            total - matched,
            total - matched - dts_blocked - e4_blocked
        );
        println!("  matched with .d.ts output: {dts_ok}");
        for (k, n) in &by_kind {
            println!("  other, first difference {k}: {n}");
        }
    }
    let _ = std::io::stdout().flush();
}
