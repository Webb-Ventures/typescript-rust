//! Design record 0005's differential test extended to emit (0012, E3 gate): random edit
//! sequences (fixed seed and count; `TS_R_DIFF_STEPS`, `TS_R_DIFF_SEED` override) on a copy
//! of `ts_r_history`'s `tests/fixtures/app`, built with `--incremental` (`-p`) and with `-b`
//! in trees carried across the edits. After every edit each carried tree's outputs must be
//! byte-equal to a clean build of the same sources, and the `-p` diagnostics equal the clean
//! build's. A variant with `declaration` runs once `.d.ts` emit exists (no TSR2004).
//!
//! Outputs of sources that no longer exist may stay behind in a carried tree (tsc leaves them
//! too); every other output must match.
//!
//! The same holds for `--watch` sessions (0012, E4): one `ts-r-check -w` process per variant
//! (`-p` without `incremental`, `-p` with it, `-b`) is carried across the edits, driven by the
//! stdin driver (`TS_R_WATCH_DRIVER=stdin`: one look for changes per line, a sentinel line
//! after each), and after every edit its outputs and diagnostics must equal a clean build's.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

ts_r_testing::cap_memory!();

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

const TYPES: [&str; 7] = [
    "number", "string", "boolean", "Point", "Box", "Shape", "unknown",
];
const NAMES: [&str; 12] = [
    "add", "scale", "sum", "pick", "area", "Canvas", "ORIGIN", "banner", "counter", "labels",
    "Settings", "label",
];
// Emit that depends on other files: const enum values inlined across files, imports elided
// when they name only types.
const SNIPPETS: [&str; 24] = [
    "export const extra: number = 1;",
    "export interface Point { z?: number }",
    "declare global { var APP_NAME: number; }",
    "export function area(): void {}",
    "import { add } from \"./math\";",
    "export * from \"./math\";",
    "const local = add(ORIGIN, ORIGIN);",
    "let wrongly: string = 1;",
    "export type Shape = string;",
    "export function made(p: Point) { return { p, n: 1 }; }",
    "import { made } from \"./extra\";",
    "export const usesMade = made({ x: 1, y: 2 }).n;",
    "export const here = import.meta.url;",
    "export enum Mode { A, B = 4, C }",
    "export const enum Flag { On = 1, Off = 0 }",
    "export const flag = Flag.On;",
    "export class Holder { constructor(public value: number, private hidden = 2) {} }",
    "export namespace Space { export const inner = 1; }",
    "export { add as plus } from \"./math\";",
    "export default class Thing { size = 1 }",
    "import Thing from \"./shapes\"; export const thingSize = new Thing().size;",
    "import * as ns from \"./math\"; export const viaNs = ns.add;",
    "import { Flag } from \"./more\"; export const viaFlag = Flag.On;",
    "import { Point } from \"./extra\"; export const typed: Point | undefined = undefined;",
];
const NEW_FILES: [&str; 5] = [
    "import type { Point } from \"./math\";\nexport function made(p: Point) { return { p, n: 1 }; }\n",
    "export const made = (p: number) => ({ n: p });\n",
    "export enum Flag { On = 1, Off = 0 }\nexport const both = Flag.On | Flag.Off;\n",
    "export const enum Flag { On = 1, Off = 0 }\nexport interface Point { x: number }\n",
    "export const enum Flag { On = 7, Off = 9 }\nexport const Point = 1;\n",
];

/// Every write gets its own modification time (a second apart, in the past), so timestamps
/// always see a change.
fn stamp(p: &Path) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let t = std::time::SystemTime::now() - std::time::Duration::from_secs(86_400)
        + std::time::Duration::from_secs(n);
    if let Ok(f) = std::fs::File::options().write(true).open(p) {
        let _ = f.set_modified(t);
    }
}

fn write(p: &Path, text: &str) {
    let _ = std::fs::write(p, text);
    stamp(p);
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("create dir");
    for e in std::fs::read_dir(from).expect("read dir").flatten() {
        let p = e.path();
        let t = to.join(e.file_name());
        if p.is_dir() {
            copy_dir(&p, &t);
        } else {
            std::fs::copy(&p, &t).expect("copy");
            stamp(&t);
        }
    }
}

fn balanced(line: &str) -> bool {
    let c = |a: char| line.chars().filter(|&x| x == a).count();
    c('{') == c('}') && c('(') == c(')') && c('[') == c(']') && c('`') % 2 == 0
}

/// One random edit of the sources under `src`; returns a description.
fn edit(
    rng: &mut Rng,
    src: &Path,
    originals: &[(PathBuf, String)],
    created: &mut Vec<PathBuf>,
) -> String {
    let op = rng.below(100);
    if op < 6 {
        let k = rng.below(NEW_FILES.len());
        let p = src.join(if rng.below(2) == 0 {
            "extra.ts"
        } else {
            "more.ts"
        });
        write(&p, NEW_FILES[k]);
        if !created.contains(&p) {
            created.push(p.clone());
        }
        return format!("create {} #{k}", name(&p));
    }
    if op < 9 && !created.is_empty() {
        let p = created.remove(rng.below(created.len()));
        let _ = std::fs::remove_file(&p);
        return format!("delete {}", name(&p));
    }
    if op < 12 {
        let mut files: Vec<PathBuf> = originals.iter().map(|(p, _)| p.clone()).collect();
        files.extend(created.iter().cloned());
        let from = files[rng.below(files.len())].clone();
        let to = src.join(["extra.ts", "more.ts", "renamed.ts"][rng.below(3)]);
        if from == to || !from.exists() || to.exists() {
            return "nothing".to_owned();
        }
        let _ = std::fs::rename(&from, &to);
        stamp(&to);
        created.retain(|c| *c != from);
        if !created.contains(&to) {
            created.push(to.clone());
        }
        return format!("rename {} -> {}", name(&from), name(&to));
    }
    let mut files: Vec<PathBuf> = originals.iter().map(|(p, _)| p.clone()).collect();
    files.extend(created.iter().cloned());
    let p = files[rng.below(files.len())].clone();
    if op < 20
        && let Some((_, t)) = originals.iter().find(|(o, _)| *o == p)
    {
        write(&p, t);
        return format!("revert {}", name(&p));
    }
    let Ok(text) = std::fs::read_to_string(&p) else {
        return "nothing".to_owned();
    };
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let desc;
    if op < 45 {
        let set: &[&str] = if rng.below(2) == 0 { &TYPES } else { &NAMES };
        let mut spots: Vec<(usize, usize, &str)> = Vec::new();
        for (li, l) in lines.iter().enumerate() {
            for w in set {
                for (pos, _) in l.match_indices(w) {
                    let before = l[..pos].chars().last();
                    let after = l[pos + w.len()..].chars().next();
                    let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
                    if !word(before) && !word(after) {
                        spots.push((li, pos, w));
                    }
                }
            }
        }
        if spots.is_empty() {
            return "nothing".to_owned();
        }
        let (li, pos, w) = spots[rng.below(spots.len())];
        let to = set[rng.below(set.len())];
        lines[li].replace_range(pos..pos + w.len(), to);
        desc = format!("{}:{} {w} -> {to}", name(&p), li + 1);
    } else if op < 60 {
        let cands: Vec<usize> = (0..lines.len())
            .filter(|&i| {
                let l = &lines[i];
                l.starts_with("export ") && !l.starts_with("export {")
                    || l.starts_with("function ")
                    || l.starts_with("const ")
                    || l.starts_with("interface ")
                    || l.starts_with("type ")
                    || l.starts_with("class ")
            })
            .collect();
        if cands.is_empty() {
            return "nothing".to_owned();
        }
        let i = cands[rng.below(cands.len())];
        if let Some(rest) = lines[i].strip_prefix("export ") {
            lines[i] = rest.to_owned();
        } else {
            lines[i] = format!("export {}", lines[i]);
        }
        desc = format!("{}:{} toggle export", name(&p), i + 1);
    } else if op < 85 {
        let s = SNIPPETS[rng.below(SNIPPETS.len())];
        let tops: Vec<usize> = (0..=lines.len())
            .filter(|&i| {
                i == lines.len() || i == 0 || {
                    let prev = &lines[i - 1];
                    prev.is_empty() || prev == "}" || prev.ends_with(';') && !prev.starts_with(' ')
                }
            })
            .collect();
        let i = tops[rng.below(tops.len())];
        lines.insert(i, s.to_owned());
        desc = format!("{}:{} insert {s}", name(&p), i + 1);
    } else {
        let cands: Vec<usize> = (0..lines.len()).filter(|&i| balanced(&lines[i])).collect();
        if cands.is_empty() {
            return "nothing".to_owned();
        }
        let i = cands[rng.below(cands.len())];
        let gone = lines.remove(i);
        desc = format!("{}:{} delete {}", name(&p), i + 1, gone.trim());
    }
    let mut out = lines.join("\n");
    out.push('\n');
    write(&p, &out);
    desc
}

fn name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// A project variant: its `compilerOptions` and how it is built.
struct Variant {
    name: &'static str,
    options: &'static str,
    build_mode: bool,
}

const VARIANTS: &[Variant] = &[
    Variant {
        name: "incremental",
        options: r#""incremental": true, "sourceMap": true"#,
        build_mode: false,
    },
    Variant {
        name: "build",
        options: r#""incremental": true, "removeComments": true"#,
        build_mode: true,
    },
    Variant {
        name: "declaration",
        options: r#""composite": true, "declarationMap": true, "sourceMap": true"#,
        build_mode: true,
    },
];

fn tsconfig(options: &str) -> String {
    format!(
        "{{\n  \"compilerOptions\": {{\n    \"strict\": true,\n    \"target\": \"es2022\",\n    \"lib\": [\"es2022\"],\n    \"module\": \"esnext\",\n    \"moduleResolution\": \"bundler\",\n    \"outDir\": \"out\",\n    {options}\n  }},\n  \"include\": [\"src\"]\n}}\n"
    )
}

/// Runs ts-r-check in `dir`, with its own cache under `cache`; returns its stdout.
fn build(dir: &Path, cache: &Path, build_mode: bool) -> String {
    let mut cmd = ts_r_testing::limited(env!("CARGO_BIN_EXE_ts-r-check"), 4096, 30);
    if build_mode {
        cmd.arg("-b");
    } else {
        cmd.args(["-p", "tsconfig.json"]);
    }
    let out = cmd
        .args(["--pretty", "false"])
        .env("TS_R_CACHE_DIR", cache)
        .current_dir(dir)
        .output()
        .expect("run ts-r-check");
    assert!(
        matches!(out.status.code(), Some(0..=2)),
        "ts-r-check failed: {}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    // Messages naming an output file carry the tree's absolute path.
    let tree = dir.to_string_lossy().replace('\\', "/");
    String::from_utf8_lossy(&out.stdout).replace(&tree, "<tree>")
}

/// Every file under `dir/out` by relative path.
fn outputs(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(base: &Path, d: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        let Ok(entries) = std::fs::read_dir(d) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(base, &p, out);
            } else if let Ok(bytes) = std::fs::read(&p) {
                let rel = p.strip_prefix(base).unwrap_or(&p);
                out.insert(rel.to_string_lossy().replace('\\', "/"), bytes);
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(&dir.join("out"), &dir.join("out"), &mut out);
    out
}

/// The source stem an output belongs to (`a.js.map` -> `a`).
fn stem(output: &str) -> &str {
    let mut s = output;
    for ext in [".map", ".js", ".d.ts"] {
        s = s.strip_suffix(ext).unwrap_or(s);
    }
    s
}

/// Compares a carried tree's outputs with the clean build's; `Err` describes the first
/// difference. The buildinfo is compared by neither: its signatures may legitimately differ.
fn compare(
    clean: &BTreeMap<String, Vec<u8>>,
    carried: &BTreeMap<String, Vec<u8>>,
    src: &Path,
) -> Result<(), String> {
    for (path, bytes) in clean {
        if path.ends_with(".tsbuildinfo") {
            continue;
        }
        match carried.get(path) {
            None => return Err(format!("{path}: missing")),
            Some(b) if b != bytes => {
                return Err(format!(
                    "{path} differs.\n--- clean\n{}\n--- carried\n{}",
                    String::from_utf8_lossy(bytes),
                    String::from_utf8_lossy(b)
                ));
            }
            Some(_) => {}
        }
    }
    for path in carried.keys() {
        if path.ends_with(".tsbuildinfo") || clean.contains_key(path) {
            continue;
        }
        if src.join(format!("{}.ts", stem(path))).exists() {
            return Err(format!("{path}: written, but not by a clean build"));
        }
    }
    Ok(())
}

#[test]
fn incremental_emit_equals_clean_build() {
    if ts_r_checker::LibStore::locate().is_none() {
        eprintln!("lib files missing: skipped");
        return;
    }
    let steps: usize = std::env::var("TS_R_DIFF_STEPS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(30);
    let seed: u64 = std::env::var("TS_R_DIFF_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0x5eed_1234_abcd_0005);
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../ts_r_history/tests/fixtures/app");
    let root = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("incremental-emit-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("temp dir");
    let root = ts_r_testing::canonical(&root).expect("canonical");
    // The edited sources.
    let work = root.join("work");
    copy_dir(&fixture, &work);
    let _ = std::fs::remove_file(work.join("tsconfig.json"));
    let src = work.join("src");
    let mut originals: Vec<(PathBuf, String)> = std::fs::read_dir(&src)
        .expect("src")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "ts"))
        .map(|p| {
            let t = std::fs::read_to_string(&p).expect("read");
            (p, t)
        })
        .collect();
    originals.sort();

    // Variants whose output this build can produce (declaration output waits for E2).
    let mut variants: Vec<&Variant> = Vec::new();
    for v in VARIANTS {
        let probe = root.join(format!("probe-{}", v.name));
        copy_dir(&work, &probe);
        write(&probe.join("tsconfig.json"), &tsconfig(v.options));
        let text = build(&probe, &root.join("probe-cache"), v.build_mode);
        let _ = std::fs::remove_dir_all(&probe);
        if text.contains("TSR2004") {
            eprintln!("{}: declaration emit not implemented yet, skipped", v.name);
        } else {
            variants.push(v);
        }
    }
    let carried: Vec<PathBuf> = variants
        .iter()
        .map(|v| root.join(format!("carried-{}", v.name)))
        .collect();
    for (v, dir) in variants.iter().zip(&carried) {
        copy_dir(&work, dir);
        write(&dir.join("tsconfig.json"), &tsconfig(v.options));
    }

    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut created: Vec<PathBuf> = Vec::new();
    let mut log: Vec<String> = Vec::new();
    // Builds compared, output files compared, of them declaration outputs.
    let (mut compared, mut files, mut dts) = (0usize, 0usize, 0usize);
    for step in 0..=steps {
        if step > 0 {
            log.push(edit(&mut rng, &src, &originals, &mut created));
        }
        for (v, dir) in variants.iter().zip(&carried) {
            // Mirror the work tree's sources into the carried tree: only changed files are
            // written, so untouched files keep their timestamps.
            let dst = dir.join("src");
            sync_sources(&src, &dst);
            let carried_text = build(dir, &root.join(format!("cache-{}", v.name)), v.build_mode);
            let clean = root.join("clean");
            let _ = std::fs::remove_dir_all(&clean);
            copy_dir(&work, &clean);
            write(&clean.join("tsconfig.json"), &tsconfig(v.options));
            let clean_cache = root.join("clean-cache");
            let _ = std::fs::remove_dir_all(&clean_cache);
            let clean_text = build(&clean, &clean_cache, v.build_mode);
            let fail = |what: String| -> String {
                format!(
                    "{} variant, step {step} (seed {seed:#x}): {what}\nedits:\n  {}",
                    v.name,
                    log.join("\n  ")
                )
            };
            let clean_out = outputs(&clean);
            if let Err(e) = compare(&clean_out, &outputs(dir), &dst) {
                panic!("{}", fail(e));
            }
            files += clean_out.len();
            dts += clean_out.keys().filter(|p| p.ends_with(".d.ts")).count();
            // `-b` prints the same diagnostics: a project with errors is rebuilt (and
            // reports them again), one without prints nothing.
            if clean_text != carried_text {
                panic!(
                    "{}",
                    fail(format!(
                        "diagnostics differ.\n--- clean\n{clean_text}\n--- carried\n{carried_text}"
                    ))
                );
            }
            compared += 1;
        }
    }
    let _ = std::fs::remove_dir_all(&root);
    eprintln!(
        "{steps} edits, {compared} builds compared ({} variants), {files} outputs ({dts} .d.ts)",
        variants.len()
    );
}

/// Makes `dst` hold exactly `src`'s files, writing only those whose text differs.
fn sync_sources(src: &Path, dst: &Path) {
    let names = |d: &Path| -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(d)
            .map(|r| {
                r.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    };
    let want = names(src);
    for n in names(dst) {
        if !want.contains(&n) {
            let _ = std::fs::remove_file(dst.join(&n));
        }
    }
    for n in want {
        let text = std::fs::read(src.join(&n)).unwrap_or_default();
        if std::fs::read(dst.join(&n)).ok().as_deref() != Some(&text[..]) {
            // The current time, as an editor's save gives it.
            let _ = std::fs::write(dst.join(&n), &text);
        }
    }
}

/// The line `ts-r-check -w` prints after each look under the stdin driver.
const CYCLE_SENTINEL: &[u8] = b"\x01ts-r-watch-cycle ";

/// A `ts-r-check -w` process driven through stdin. Dropping it ends the session (stdin
/// closed, then killed if it has not exited within a few seconds).
struct WatchSession {
    child: std::process::Child,
    stdin: Option<std::process::ChildStdin>,
    rx: std::sync::mpsc::Receiver<Vec<u8>>,
    pending: Vec<u8>,
}

impl WatchSession {
    /// Starts the session; returns it and the first build's output.
    fn start(dir: &Path, cache: &Path, args: &[&str]) -> (WatchSession, String) {
        use std::io::Read;
        let mut child = Command::new(env!("CARGO_BIN_EXE_ts-r-check"))
            .args(args)
            .args(["-w", "--pretty", "false"])
            .env("TS_R_CACHE_DIR", cache)
            .env("TS_R_WATCH_DRIVER", "stdin")
            .current_dir(dir)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("run ts-r-check -w");
        let stdin = child.stdin.take();
        let mut stdout = child.stdout.take().expect("stdout");
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n) = stdout.read(&mut buf) {
                if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                    return;
                }
            }
        });
        let mut s = WatchSession {
            child,
            stdin,
            rx,
            pending: Vec::new(),
        };
        let first = s.cycle();
        (s, first)
    }

    /// Output up to the next sentinel (60 s at most).
    fn cycle(&mut self) -> String {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            if let Some(at) = self
                .pending
                .windows(CYCLE_SENTINEL.len())
                .position(|w| w == CYCLE_SENTINEL)
                && let Some(nl) = self.pending[at..].iter().position(|&b| b == b'\n')
            {
                let out = String::from_utf8_lossy(&self.pending[..at]).into_owned();
                self.pending.drain(..at + nl + 1);
                return out;
            }
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            match self.rx.recv_timeout(left) {
                Ok(chunk) => self.pending.extend_from_slice(&chunk),
                Err(e) => panic!(
                    "watch session: no build finished ({e:?}); output so far:\n{}",
                    String::from_utf8_lossy(&self.pending)
                ),
            }
        }
    }

    /// One look for changes; returns what the build printed (empty: nothing changed).
    fn look(&mut self) -> String {
        use std::io::Write;
        let s = self.stdin.as_mut().expect("stdin");
        s.write_all(b"\n")
            .and_then(|()| s.flush())
            .expect("ask for a look");
        self.cycle()
    }
}

impl Drop for WatchSession {
    fn drop(&mut self) {
        drop(self.stdin.take());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A watch build's diagnostics: its status lines (`<time> - Starting compilation...`,
/// `File change detected...`, `Found N errors. Watching...`) and blank lines removed.
fn watch_diagnostics(text: &str) -> String {
    text.lines()
        .filter(|l| {
            let status = l.ends_with(" - Starting compilation in watch mode...")
                || l.ends_with(" - File change detected. Starting incremental compilation...")
                || (l.contains(" - Found ") && l.ends_with(". Watching for file changes."));
            !l.is_empty() && !status
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn without_blank_lines(text: &str) -> String {
    text.lines()
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

const WATCH_VARIANTS: &[Variant] = &[
    Variant {
        name: "watch",
        options: r#""sourceMap": true"#,
        build_mode: false,
    },
    Variant {
        name: "watch-incremental",
        options: r#""incremental": true"#,
        build_mode: false,
    },
    Variant {
        name: "watch-build",
        options: r#""incremental": true, "removeComments": true"#,
        build_mode: true,
    },
    Variant {
        name: "watch-declaration",
        options: r#""composite": true, "declarationMap": true"#,
        build_mode: true,
    },
];

#[test]
fn watch_build_tracks_persisted_inputs() {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("watch-persisted-inputs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for change in ["dependency", "package", "missing-package"] {
        let dir = root.join(change);
        std::fs::create_dir_all(dir.join("node_modules/pkg")).unwrap();
        write(
            &dir.join("tsconfig.json"),
            r#"{"compilerOptions":{"incremental":true,"types":[],"target":"es2022","module":"nodenext","moduleResolution":"nodenext","outDir":"out"},"files":["main.ts"]}"#,
        );
        write(
            &dir.join("main.ts"),
            "import { value } from './dep.js'; import { pack } from 'pkg'; export const result: number = value; export const installed: number = pack;",
        );
        write(&dir.join("dep.ts"), "export const value = 1;");
        write(
            &dir.join("node_modules/pkg/package.json"),
            r#"{"types":"index.d.ts"}"#,
        );
        write(
            &dir.join("node_modules/pkg/index.d.ts"),
            "export declare const pack: number;",
        );
        write(
            &dir.join("node_modules/pkg/alternate.d.ts"),
            "export declare const pack: string;",
        );
        let cache = root.join(format!("cache-{change}"));
        assert!(build(&dir, &cache, true).is_empty());
        let (mut session, first) = WatchSession::start(&dir, &cache, &["-b", "--verbose"]);
        assert!(first.contains("is up to date because"), "{first}");
        assert!(session.look().is_empty());
        let (input, text) = match change {
            "dependency" => (dir.join("dep.ts"), "export const value = 'changed';"),
            "package" => (
                dir.join("node_modules/pkg/package.json"),
                r#"{"types":"alternate.d.ts"}"#,
            ),
            _ => (dir.join("package.json"), r#"{"type":"module"}"#),
        };
        std::fs::write(&input, text).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&input)
            .unwrap()
            .set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(2))
            .unwrap();
        let rebuilt = session.look();
        assert!(
            rebuilt.contains("File change detected."),
            "{change}: {rebuilt}"
        );
        if change != "missing-package" {
            assert!(rebuilt.contains("error TS2322"), "{change}: {rebuilt}");
        }
        assert!(session.look().is_empty());
        drop(session);
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn watch_emit_equals_clean_build() {
    if ts_r_checker::LibStore::locate().is_none() {
        eprintln!("lib files missing: skipped");
        return;
    }
    let steps: usize = std::env::var("TS_R_DIFF_STEPS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(30);
    let seed: u64 = std::env::var("TS_R_DIFF_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0x5eed_1234_abcd_0e04);
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../ts_r_history/tests/fixtures/app");
    let root =
        Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("watch-emit-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("temp dir");
    let root = ts_r_testing::canonical(&root).expect("canonical");
    let work = root.join("work");
    copy_dir(&fixture, &work);
    let _ = std::fs::remove_file(work.join("tsconfig.json"));
    let src = work.join("src");
    let mut originals: Vec<(PathBuf, String)> = std::fs::read_dir(&src)
        .expect("src")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "ts"))
        .map(|p| {
            let t = std::fs::read_to_string(&p).expect("read");
            (p, t)
        })
        .collect();
    originals.sort();

    let mut variants: Vec<&Variant> = Vec::new();
    for v in WATCH_VARIANTS {
        let probe = root.join(format!("probe-{}", v.name));
        copy_dir(&work, &probe);
        write(&probe.join("tsconfig.json"), &tsconfig(v.options));
        let text = build(&probe, &root.join("probe-cache"), v.build_mode);
        let _ = std::fs::remove_dir_all(&probe);
        if text.contains("TSR2004") {
            eprintln!("{}: declaration emit not implemented yet, skipped", v.name);
        } else {
            variants.push(v);
        }
    }
    // One session per variant, carried across the edits.
    let mut sessions: Vec<(PathBuf, WatchSession, String)> = Vec::new();
    for v in &variants {
        let dir = root.join(format!("carried-{}", v.name));
        copy_dir(&work, &dir);
        write(&dir.join("tsconfig.json"), &tsconfig(v.options));
        let args: &[&str] = if v.build_mode {
            &["-b"]
        } else {
            &["-p", "tsconfig.json"]
        };
        let (s, first) = WatchSession::start(&dir, &root.join(format!("cache-{}", v.name)), args);
        let tree = dir.to_string_lossy().replace('\\', "/");
        sessions.push((dir, s, watch_diagnostics(&first.replace(&tree, "<tree>"))));
    }

    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut created: Vec<PathBuf> = Vec::new();
    let mut log: Vec<String> = Vec::new();
    let (mut compared, mut rebuilt) = (0usize, 0usize);
    for step in 0..=steps {
        if step > 0 {
            log.push(edit(&mut rng, &src, &originals, &mut created));
        }
        for (v, (dir, session, last)) in variants.iter().zip(sessions.iter_mut()) {
            let dst = dir.join("src");
            if step > 0 {
                sync_sources(&src, &dst);
                let text = session.look();
                // A look that found no change prints nothing: the last build's
                // diagnostics stand.
                if !text.is_empty() {
                    let tree = dir.to_string_lossy().replace('\\', "/");
                    *last = watch_diagnostics(&text.replace(&tree, "<tree>"));
                    rebuilt += 1;
                }
            }
            let clean = root.join("clean");
            let _ = std::fs::remove_dir_all(&clean);
            copy_dir(&work, &clean);
            write(&clean.join("tsconfig.json"), &tsconfig(v.options));
            let clean_cache = root.join("clean-cache");
            let _ = std::fs::remove_dir_all(&clean_cache);
            let clean_text = without_blank_lines(&build(&clean, &clean_cache, v.build_mode));
            let fail = |what: String| -> String {
                format!(
                    "{} variant, step {step} (seed {seed:#x}): {what}\nedits:\n  {}",
                    v.name,
                    log.join("\n  ")
                )
            };
            if let Err(e) = compare(&outputs(&clean), &outputs(dir), &dst) {
                panic!("{}", fail(e));
            }
            if clean_text != *last {
                panic!(
                    "{}",
                    fail(format!(
                        "diagnostics differ.\n--- clean\n{clean_text}\n--- watch\n{last}"
                    ))
                );
            }
            compared += 1;
        }
    }
    drop(sessions);
    let _ = std::fs::remove_dir_all(&root);
    eprintln!(
        "{steps} edits, {compared} watch builds compared ({} variants, {rebuilt} rebuilds)",
        variants.len()
    );
}
