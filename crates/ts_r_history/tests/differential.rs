//! Incremental equals fresh: random edit sequences (fixed seed, fixed count) applied to a
//! copy of the fixture project; after each edit the program is checked fresh
//! (`check_program`) and through a [`History`] carried across edits, and the diagnostics must
//! be identical. `TS_R_DIFF_STEPS` overrides the step count, `TS_R_DIFF_SEED` the seed.

use std::path::{Path, PathBuf};

use ts_r_checker::{CheckRun, Checker, LibStore, ProgramFiles, check_program, load_program};
use ts_r_config::{MemoryFs, RealFs, load_tsconfig};
use ts_r_diagnostics::Diagnostic;
use ts_r_history::incremental::History;
use ts_r_history::persist;

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
const NAMES: [&str; 20] = [
    "add", "scale", "sum", "pick", "area", "Canvas", "ORIGIN", "banner", "counter", "labels",
    "pages", "describe", "lastItem", "APP_NAME", "Settings", "label", "radius", "kind", "title",
    "Event",
];
const SNIPPETS: [&str; 30] = [
    "export const extra: number = 1;",
    "export interface Point { z?: number }",
    "declare global { var APP_NAME: number; }",
    "export function area(): void {}",
    "import { add } from \"./math\";",
    "export * from \"./math\";",
    "const local = add(ORIGIN, ORIGIN);",
    "let wrongly: string = 1;",
    "export type Shape = string;",
    "interface Settings { extra: boolean }",
    "declare module \"./shapes\" { interface Box { depth: number } }",
    "export const inferred = [1, 2, 3].lastItem();",
    "export function made(p: Point) { return { p, n: 1 }; }",
    "import { made } from \"./extra\";",
    "export const usesMade = made({ x: 1, y: 2 }).n;",
    "declare global { interface Array<T> { lastItem(): string } }",
    // DOM globals: the fast path parses the DOM libs only when a re-checked file sees them.
    "export const title = document.title;",
    "export const here = import.meta.url;",
    "declare global { interface Window { extra: number } }",
    "export const fromWindow = window.extra;",
    "let name = 1;",
    "export function onEvent(e: Event) { return e.type; }",
    // Per-export precision: re-exports, type-only exports, shadowed stars, defaults,
    // namespace imports.
    "export type { Point } from \"./math\";",
    "export { add as plus } from \"./math\";",
    "import { plus } from \"./shapes\"; export const viaPlus = plus;",
    "export const add = 5;",
    "export default class Thing { size = 1 }",
    "import Thing from \"./shapes\"; export const thingSize = new Thing().size;",
    "export * from \"./shapes\";",
    "import * as ns from \"./math\"; export const viaNs = ns.add;",
];
const NEW_FILES: [&str; 4] = [
    "import type { Point } from \"./math\";\nexport function made(p: Point) { return { p, n: 1 }; }\n",
    "export const made = (p: number) => ({ n: p });\n",
    "declare var APP_NAME: number;\nexport {};\n",
    "declare const v: { made: (p: number) => { n: number } };\nexport = v;\n",
];

/// Every write gets its own modification time (a second apart, in the past), so stat
/// fingerprints (trusted at once: the racy window is 0 in this test) always see a change.
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

fn lines(program: &ProgramFiles, diags: &[Diagnostic]) -> Vec<String> {
    lines_by(
        &|i| match program.files.get(i as usize) {
            Some(f) if f.is_lib => "<lib>".to_owned(),
            Some(f) => f.path.clone(),
            None => "<global>".to_owned(),
        },
        diags,
    )
}

fn lines_by(path_of: &dyn Fn(u32) -> String, diags: &[Diagnostic]) -> Vec<String> {
    let path = |f: ts_r_diagnostics::FileId| path_of(f.0);
    diags
        .iter()
        .map(|d| {
            let rel: Vec<String> = d
                .related
                .iter()
                .map(|r| format!("{}@{}:{}", path(r.file), r.span.start, r.span.end))
                .collect();
            format!(
                "{}@{}:{} TS{} {} [{}]",
                path(d.file),
                d.span.start,
                d.span.end,
                d.code,
                d.message,
                rel.join("; ")
            )
        })
        .collect()
}

fn balanced(line: &str) -> bool {
    let c = |a: char| line.chars().filter(|&x| x == a).count();
    c('{') == c('}') && c('(') == c(')') && c('[') == c(']') && c('`') % 2 == 0
}

/// One random edit; returns a description.
fn edit(
    rng: &mut Rng,
    src: &Path,
    originals: &[(PathBuf, String)],
    created: &mut Vec<PathBuf>,
) -> String {
    let op = rng.below(100);
    if op < 6 {
        // Create a file.
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
        return format!("create {} #{k}", p.display());
    }
    if op < 9 && !created.is_empty() {
        let p = created.remove(rng.below(created.len()));
        let _ = std::fs::remove_file(&p);
        return format!("delete {}", p.display());
    }
    if op < 12 {
        // Rename a file (an original one comes back with its next revert).
        let mut files: Vec<PathBuf> = originals.iter().map(|(p, _)| p.clone()).collect();
        files.extend(created.iter().cloned());
        let from = files[rng.below(files.len())].clone();
        let to = src.join(["extra.ts", "more.ts", "renamed.ts"][rng.below(3)]);
        if from == to || !from.exists() || to.exists() {
            return "nothing".to_owned();
        }
        let _ = std::fs::rename(&from, &to);
        created.retain(|c| *c != from);
        if !created.contains(&to) {
            created.push(to.clone());
        }
        return format!("rename {} -> {}", from.display(), to.display());
    }
    let mut files: Vec<PathBuf> = originals.iter().map(|(p, _)| p.clone()).collect();
    files.extend(created.iter().cloned());
    let p = files[rng.below(files.len())].clone();
    if op < 20
        && let Some((_, t)) = originals.iter().find(|(o, _)| *o == p)
    {
        write(&p, t);
        return format!("revert {}", p.display());
    }
    let Ok(text) = std::fs::read_to_string(&p) else {
        return "nothing".to_owned();
    };
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let desc;
    if op < 45 {
        // Swap a word.
        let (from_set, to_set): (&[&str], &[&str]) = if rng.below(2) == 0 {
            (&TYPES, &TYPES)
        } else {
            (&NAMES, &NAMES)
        };
        let mut spots: Vec<(usize, usize, &str)> = Vec::new();
        for (li, l) in lines.iter().enumerate() {
            for w in from_set {
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
        let to = to_set[rng.below(to_set.len())];
        lines[li].replace_range(pos..pos + w.len(), to);
        desc = format!("{}:{} {w} -> {to}", p.display(), li + 1);
    } else if op < 60 {
        // Toggle `export`.
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
        desc = format!("{}:{} toggle export", p.display(), i + 1);
    } else if op < 85 {
        // Insert a snippet at a top-level line start.
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
        desc = format!("{}:{} insert {s}", p.display(), i + 1);
    } else {
        // Delete a balanced line.
        let cands: Vec<usize> = (0..lines.len()).filter(|&i| balanced(&lines[i])).collect();
        if cands.is_empty() {
            return "nothing".to_owned();
        }
        let i = cands[rng.below(cands.len())];
        let gone = lines.remove(i);
        desc = format!("{}:{} delete {}", p.display(), i + 1, gone.trim());
    }
    let mut out = lines.join("\n");
    out.push('\n');
    write(&p, &out);
    desc
}

#[test]
fn incremental_equals_fresh() {
    let Some(libs) = LibStore::locate() else {
        eprintln!("lib files missing: skipped");
        return;
    };
    let steps: usize = std::env::var("TS_R_DIFF_STEPS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(80);
    let seed: u64 = std::env::var("TS_R_DIFF_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0x5eed_1234_abcd_0001);
    ts_r_history::stat::set_racy_window(std::time::Duration::ZERO);
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/app");
    let dir = std::env::temp_dir().join(format!("ts-r-history-diff-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    copy_dir(&fixture, &dir);
    let src = dir.join("src");
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
    let config = dir
        .join("tsconfig.json")
        .to_string_lossy()
        .replace('\\', "/");
    let fs = RealFs::new();
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut history = History::default();
    // The persisted history, in a temp cache.
    let cache_dir = dir.with_extension("cache");
    let _ = std::fs::remove_dir_all(&cache_dir);
    let cache = ts_r_cache::Cache::open_in(&cache_dir, persist::NAMESPACE, persist::FORMAT_VERSION)
        .expect("cache");
    let mut modes = [0usize; 3];
    // Incremental runs on the fast path, and on the full load.
    let (mut fast, mut full) = (0usize, 0usize);
    let mut created: Vec<PathBuf> = Vec::new();
    let mut log: Vec<String> = Vec::new();
    let (mut checked, mut total, mut blocked) = (0usize, 0usize, 0usize);
    let mut failure: Option<String> = None;
    for step in 0..=steps {
        if step > 0 {
            log.push(edit(&mut rng, &src, &originals, &mut created));
        }
        let cfg = load_tsconfig(&fs, &config);
        let program = load_program(&fs, &cfg.file_names, &cfg.options, Some(&libs), 1);
        let run = CheckRun { threads: 1 };
        let fresh = check_program(&program, run);
        let (inc, stats) = history.check(&program, run, Checker::new);
        let want = lines(&program, &fresh.diagnostics);
        let got = lines(&program, &inc);
        let saved = persist::check(&fs, Some(&cache), &cfg, Some(&libs), 1);
        modes[saved.mode as usize] += 1;
        if saved.mode == persist::Mode::Incremental {
            if saved.phases.iter().any(|(n, _)| *n == "fallback") {
                full += 1;
            } else {
                fast += 1;
            }
        }
        if saved.mode != persist::Mode::Unchanged {
            let want_files: Vec<(String, bool)> = program
                .files
                .iter()
                .filter(|f| !f.is_lib)
                .map(|f| (f.path.clone(), f.is_external))
                .collect();
            if saved.files != want_files {
                failure = Some(format!(
                    "step {step}: program files differ (seed {seed:#x}).\nedits:\n  {}\nwant {want_files:?}\ngot  {:?}",
                    log.join("\n  "),
                    saved.files
                ));
                break;
            }
        }
        let got_saved = lines_by(
            &|i| match saved.sources.get(i as usize) {
                Some(Some((p, _))) => p.clone(),
                Some(None) => "<lib>".to_owned(),
                None => "<global>".to_owned(),
            },
            &saved.diagnostics,
        );
        if want != got_saved {
            let missing: Vec<&String> = want.iter().filter(|l| !got_saved.contains(l)).collect();
            let extra: Vec<&String> = got_saved.iter().filter(|l| !want.contains(l)).collect();
            failure = Some(format!(
                "step {step}: persisted history ({:?}) differs (seed {seed:#x}).\nedits:\n  {}\nmissing:\n  {}\nextra:\n  {}",
                saved.mode,
                log.join("\n  "),
                missing
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join("\n  "),
                extra
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join("\n  ")
            ));
            break;
        }
        if want != got {
            let missing: Vec<&String> = want.iter().filter(|l| !got.contains(l)).collect();
            let extra: Vec<&String> = got.iter().filter(|l| !want.contains(l)).collect();
            failure = Some(format!(
                "step {step} differs (seed {seed:#x}).\nedits:\n  {}\nmissing (fresh only):\n  {}\nextra (incremental only):\n  {}",
                log.join("\n  "),
                missing
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join("\n  "),
                extra
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join("\n  ")
            ));
            break;
        }
        if stats.blocked {
            blocked += 1;
        } else if step > 0 {
            checked += stats.checked;
            total += stats.checked + stats.reused;
        }
    }
    drop(cache);
    // `TS_R_DIFF_KEEP=1` keeps the project and cache for a look.
    if std::env::var_os("TS_R_DIFF_KEEP").is_some() {
        eprintln!("kept {} and {}", dir.display(), cache_dir.display());
    } else {
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&cache_dir);
    }
    if let Some(f) = failure {
        panic!("{f}");
    }
    eprintln!(
        "{steps} edits: re-checked {checked} of {total} file checks ({blocked} steps stopped at syntax errors); persisted: {} cold, {} unchanged, {} incremental ({fast} without a full load)",
        modes[0], modes[1], modes[2]
    );
    assert!(modes[1] > 0 && modes[2] > 0, "persisted modes: {modes:?}");
    // Full loads: blocking syntax errors and the run after one.
    assert!(
        fast > 0 && fast + 2 * blocked >= full,
        "fast path {fast}, full loads {full}"
    );
    assert!(
        checked * 10 < total * 9,
        "incremental checks should skip files: {checked} of {total}"
    );
}

/// A `/// <reference lib>` added by an edit brings in a lib the saved program did not have
/// (and removing it drops the lib again): the persisted history agrees with a fresh check.
#[test]
fn source_lib_references_follow_edits() {
    let Some(libs) = LibStore::locate() else {
        eprintln!("lib files missing: skipped");
        return;
    };
    ts_r_history::stat::set_racy_window(std::time::Duration::ZERO);
    let dir = std::env::temp_dir().join(format!("ts-r-history-libref-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).expect("create dir");
    write(
        &dir.join("tsconfig.json"),
        r#"{ "compilerOptions": { "lib": ["es5"], "types": [], "strict": true }, "include": ["src"] }"#,
    );
    let a = dir.join("src/a.ts");
    let b = dir.join("src/b.ts");
    write(&b, "export const b = 1;\n");
    let config = dir
        .join("tsconfig.json")
        .to_string_lossy()
        .replace('\\', "/");
    let cache_dir = dir.with_extension("cache");
    let _ = std::fs::remove_dir_all(&cache_dir);
    let cache = ts_r_cache::Cache::open_in(&cache_dir, persist::NAMESPACE, persist::FORMAT_VERSION)
        .expect("cache");
    let fs = RealFs::new();
    let map = "export const m = new Map<string, number>();\n";
    let texts = [
        "export const a = 1;\n".to_owned(),
        format!("/// <reference lib=\"es2015.collection\" />\n{map}"),
        map.to_owned(),
    ];
    let mut failure = None;
    for (step, text) in texts.iter().enumerate() {
        write(&a, text);
        let cfg = load_tsconfig(&fs, &config);
        let program = load_program(&fs, &cfg.file_names, &cfg.options, Some(&libs), 1);
        let want = lines(
            &program,
            &check_program(&program, CheckRun { threads: 1 }).diagnostics,
        );
        let saved = persist::check(&fs, Some(&cache), &cfg, Some(&libs), 1);
        let got = lines_by(
            &|i| match saved.sources.get(i as usize) {
                Some(Some((p, _))) => p.clone(),
                Some(None) => "<lib>".to_owned(),
                None => "<global>".to_owned(),
            },
            &saved.diagnostics,
        );
        if want != got {
            failure = Some(format!(
                "step {step} ({:?}):\nwant {want:#?}\ngot {got:#?}",
                saved.mode
            ));
            break;
        }
    }
    drop(cache);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&cache_dir);
    if let Some(f) = failure {
        panic!("{f}");
    }
}

/// A small project in a temp dir with a persisted history in a temp cache: `step` checks it
/// fresh, through a carried [`History`] and through `persist::check`, and returns the three
/// diagnostic lists (each must match the fresh one).
struct Project {
    dir: PathBuf,
    cache_dir: PathBuf,
    cache: ts_r_cache::Cache,
    history: History,
    libs: LibStore,
}

impl Project {
    fn new(name: &str, files: &[(&str, &str)]) -> Option<Project> {
        let libs = LibStore::locate()?;
        ts_r_history::stat::set_racy_window(std::time::Duration::ZERO);
        let dir = std::env::temp_dir().join(format!("ts-r-history-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (p, text) in files {
            let p = dir.join(p);
            std::fs::create_dir_all(p.parent().expect("parent")).expect("dir");
            write(&p, text);
        }
        let cache_dir = dir.with_extension("cache");
        let _ = std::fs::remove_dir_all(&cache_dir);
        let cache =
            ts_r_cache::Cache::open_in(&cache_dir, persist::NAMESPACE, persist::FORMAT_VERSION)
                .expect("cache");
        Some(Project {
            dir,
            cache_dir,
            cache,
            history: History::default(),
            libs,
        })
    }

    fn write(&self, path: &str, text: &str) {
        write(&self.dir.join(path), text);
    }

    /// Fresh diagnostics; panics when the history or the persisted run differs.
    fn step(&mut self, what: &str) -> Vec<String> {
        let fs = RealFs::new();
        let config = self
            .dir
            .join("tsconfig.json")
            .to_string_lossy()
            .replace('\\', "/");
        let cfg = load_tsconfig(&fs, &config);
        let program = load_program(&fs, &cfg.file_names, &cfg.options, Some(&self.libs), 1);
        let run = CheckRun { threads: 1 };
        let want = lines(&program, &check_program(&program, run).diagnostics);
        let (inc, _) = self.history.check(&program, run, Checker::new);
        assert_eq!(lines(&program, &inc), want, "{what}: History::check");
        let saved = persist::check(&fs, Some(&self.cache), &cfg, Some(&self.libs), 1);
        let got = lines_by(
            &|i| match saved.sources.get(i as usize) {
                Some(Some((p, _))) => p.clone(),
                Some(None) => "<lib>".to_owned(),
                None => "<global>".to_owned(),
            },
            &saved.diagnostics,
        );
        assert_eq!(got, want, "{what}: persist::check ({:?})", saved.mode);
        want
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
        let _ = std::fs::remove_dir_all(&self.cache_dir);
    }
}

#[test]
fn ambient_jsx_runtime_follows_edits() {
    let runtime = "declare module \"react/jsx-runtime\" { export namespace JSX { interface IntrinsicElements { div: {} } } }\n";
    let Some(mut p) = Project::new(
        "ambient-jsx-runtime",
        &[
            (
                "tsconfig.json",
                r#"{"compilerOptions":{"jsx":"react-jsx","types":[],"skipLibCheck":true},"include":["src"]}"#,
            ),
            ("src/runtime.d.ts", runtime),
            ("src/main.tsx", "export const x = <div />;\n"),
        ],
    ) else {
        eprintln!("lib files missing: skipped");
        return;
    };
    assert!(p.step("cold").is_empty());
    assert!(p.step("unchanged").is_empty());
    p.write(
        "src/runtime.d.ts",
        &runtime.replace("div: {}", "div: { required: string }"),
    );
    let errors = p.step("runtime prop becomes required");
    assert!(errors.iter().any(|e| e.contains("required")), "{errors:#?}");
    p.write("src/main.tsx", "export const x = <div required=\"x\" />;\n");
    assert!(p.step("edited JSX file keeps runtime").is_empty());
    p.write("src/main.tsx", "export const x = <div />;\n");
    assert!(!p.step("edited JSX file misses required prop").is_empty());
    p.write("src/runtime.d.ts", runtime);
    assert!(p.step("runtime removes required prop").is_empty());
}

#[test]
fn package_declarations_see_project_globals() {
    let Some(mut p) = Project::new(
        "pkg-globals",
        &[
            (
                "tsconfig.json",
                r#"{"compilerOptions":{"strict":true,"noEmit":true,"skipLibCheck":true},"include":["src"]}"#,
            ),
            ("src/globals.ts", "type CustomGlobal = string;\n"),
            (
                "src/main.ts",
                "import { value } from \"pkg\";\nexport const n: number = value;\n",
            ),
            (
                "node_modules/pkg/package.json",
                r#"{"name":"pkg","types":"index.d.ts"}"#,
            ),
            (
                "node_modules/pkg/index.d.ts",
                "export declare const value: CustomGlobal;\n",
            ),
        ],
    ) else {
        eprintln!("lib files missing: skipped");
        return;
    };
    let cold = p.step("cold");
    assert!(cold.iter().any(|l| l.contains("TS2322")), "{cold:?}");
    p.write(
        "src/main.ts",
        "import { value } from \"pkg\";\nexport const n: number = value; // edited\n",
    );
    p.step("main.ts edited");
    p.write("src/globals.ts", "type CustomGlobal = number;\n");
    let fixed = p.step("the global changed");
    assert!(!fixed.iter().any(|l| l.contains("TS2322")), "{fixed:?}");
}

#[test]
fn relinked_files_keep_the_re_exports_between_them() {
    let Some(mut p) = Project::new(
        "relink-reexports",
        &[
            (
                "tsconfig.json",
                r#"{"compilerOptions":{"strict":true,"noEmit":true,"skipLibCheck":true},"include":["src"]}"#,
            ),
            ("src/d.d.ts", "export interface X { a: number }\n"),
            ("src/c.d.ts", "export { X } from \"./d\";\n"),
            ("src/b.ts", "export { X } from \"./c\";\n"),
            (
                "src/aug.ts",
                "declare module \"./b\" { interface X { extra: number } }\nexport {};\n",
            ),
            (
                "src/main.ts",
                "import type { X } from \"./b\";\nexport const x: X = { a: 1, extra: 2 };\n",
            ),
        ],
    ) else {
        eprintln!("lib files missing: skipped");
        return;
    };
    p.step("cold");
    p.write("src/d.d.ts", "export interface X { a: number }\n\n");
    p.step("whitespace edit to d.d.ts");
}

#[test]
fn path_reference_diagnostics_survive_history() {
    check_grammar_steps(
        "path-reference-errors",
        &[
            (
                "/// <reference path=\"missing.ts\" />\nexport {};\n",
                &[6053],
            ),
            ("/// <reference path=\"a.ts\" />\nexport {};\n", &[1006]),
            ("/// <reference path=\"file.txt\" />\nexport {};\n", &[6054]),
            ("/// <reference path=\"missing\" />\nexport {};\n", &[6231]),
            ("/// <reference path=\"file.js\" />\nexport {};\n", &[6504]),
            ("export {};\n", &[]),
        ],
    );
}

#[test]
fn path_reference_targets_follow_edits() {
    let dir = std::env::temp_dir().join(format!("ts-r-path-targets-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let cache = ts_r_cache::Cache::open_in(&dir, persist::NAMESPACE, persist::FORMAT_VERSION)
        .expect("cache");
    // A minimal lib with the core global types (without them every check reports TS2318,
    // and the lazy path defers to a full load).
    std::fs::create_dir_all(dir.join("libs")).expect("create dir");
    std::fs::write(
        dir.join("libs/lib.es5.d.ts"),
        "interface Array<T> {}\ninterface Boolean {}\ninterface CallableFunction {}\n\
         interface Function {}\ninterface IArguments {}\ninterface NewableFunction {}\n\
         interface Number {}\ninterface Object {}\ninterface RegExp {}\ninterface String {}\n",
    )
    .expect("write lib");
    let libs = LibStore::new(dir.join("libs"));
    let mut history = History::default();
    let run = CheckRun { threads: 1 };
    for (step, target) in [false, true, false].into_iter().enumerate() {
        let mut fs = MemoryFs::new();
        fs.set_cwd("/p");
        fs.add_file(
            "/p/tsconfig.json",
            r#"{"compilerOptions":{"lib":["es5"],"types":[],"noEmit":true},"files":["a.ts","b.ts"]}"#,
        );
        fs.add_file(
            "/p/a.ts",
            "/// <reference path=\"target.ts\" />\nexport {};\n",
        );
        fs.add_file("/p/b.ts", "export const b = 1;\n");
        if target {
            fs.add_file("/p/target.ts", "export {};\n");
        }
        let cfg = load_tsconfig(&fs, "/p/tsconfig.json");
        let program = load_program(&fs, &cfg.file_names, &cfg.options, Some(&libs), 1);
        let expected = check_program(&program, run).diagnostics;
        assert_eq!(
            expected.iter().filter(|d| d.code == 6053).count(),
            usize::from(!target)
        );
        for _ in 0..2 {
            let (actual, _) = history.check(&program, run, Checker::new);
            assert_eq!(actual, expected);
            let saved = persist::check(&fs, Some(&cache), &cfg, Some(&libs), 1);
            assert_eq!(saved.diagnostics, expected);
            if step > 0 {
                assert!(!saved.phases.iter().any(|(name, _)| *name == "fallback"));
            }
        }
    }
    let mut fs = MemoryFs::new();
    fs.set_cwd("/p");
    fs.add_file(
        "/p/tsconfig.json",
        r#"{"compilerOptions":{"lib":["es5"],"types":[],"noEmit":true},"files":["a.ts"]}"#,
    );
    fs.add_file(
        "/p/a.ts",
        "/// <reference path=\"missing.ts\" />\nconst x = ;\n",
    );
    let cfg = load_tsconfig(&fs, "/p/tsconfig.json");
    let program = load_program(&fs, &cfg.file_names, &cfg.options, Some(&libs), 1);
    let expected = check_program(&program, run).diagnostics;
    // A blocking syntax error drops the missing target (tsc 7.0.2: only TS1109).
    assert_eq!(expected.iter().map(|d| d.code).collect::<Vec<_>>(), [1109]);
    let (actual, stats) = history.check(&program, run, Checker::new);
    assert!(stats.blocked);
    assert_eq!(actual, expected);
    assert_eq!(
        persist::check(&fs, Some(&cache), &cfg, Some(&libs), 1).diagnostics,
        expected
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The parser's checker grammar errors (TS1254 in a declaration file here) do not stop the
/// check as syntax errors do, and replay from the persisted history with the file's check
/// diagnostics.
#[test]
fn grammar_errors_do_not_block() {
    let Some(mut p) = Project::new(
        "grammar-errors",
        &[
            (
                "tsconfig.json",
                r#"{"compilerOptions":{"strict":true,"noEmit":true},"include":["src"]}"#,
            ),
            ("src/g.d.ts", "export declare const x: number;\n"),
            ("src/main.ts", "export const n: number = \"a\";\n"),
        ],
    ) else {
        eprintln!("lib files missing: skipped");
        return;
    };
    p.step("cold");
    p.write("src/g.d.ts", "export declare const x = Math.max(1, 2);\n");
    let both = |d: &[String]| {
        d.iter().any(|l| l.contains("TS1254")) && d.iter().any(|l| l.contains("TS2322"))
    };
    let d = p.step("grammar error in g.d.ts");
    assert!(both(&d), "{d:#?}");
    p.write("src/main.ts", "export const n: number = \"b\";\n");
    let d = p.step("edit to main.ts");
    assert!(both(&d), "{d:#?}");
    p.write("src/main.ts", "export const n: number = \"b\";\nconst c;\n");
    let d = p.step("grammar error in main.ts");
    assert!(d.iter().any(|l| l.contains("TS1155")), "{d:#?}");
    p.write("src/g.d.ts", "export declare const x: number;\n");
    let d = p.step("fixed g.d.ts");
    assert!(!d.iter().any(|l| l.contains("TS1254")), "{d:#?}");
}

#[test]
fn grammar_errors_share_comment_directives() {
    check_grammar_steps(
        "grammar-directives",
        &[
            ("const x: number;\n", &[1155]),
            ("// @ts-ignore\nconst x: number;\n", &[]),
            ("// @ts-expect-error\nconst x: number;\n", &[]),
            ("// @ts-expect-error\nconst x: number = 1;\n", &[2578]),
        ],
    );
}

#[test]
fn grammar_errors_keep_positional_deduplication() {
    check_grammar_steps(
        "grammar-deduplication",
        &[
            (
                "export class C { constructor(public ...a: any[], b: any) {} }\n",
                &[1014],
            ),
            (
                "export class C { constructor(public ...a: any[], b: any) {} }\n\n",
                &[1014],
            ),
        ],
    );
}

fn check_grammar_steps(name: &str, steps: &[(&str, &[u32])]) {
    let dir = std::env::temp_dir().join(format!("ts-r-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let cache = ts_r_cache::Cache::open_in(&dir, persist::NAMESPACE, persist::FORMAT_VERSION)
        .expect("cache");
    // A minimal lib with the core global types (without them every check reports TS2318,
    // and the lazy path defers to a full load).
    std::fs::create_dir_all(dir.join("libs")).expect("create dir");
    std::fs::write(
        dir.join("libs/lib.es5.d.ts"),
        "interface Array<T> {}\ninterface Boolean {}\ninterface CallableFunction {}\n\
         interface Function {}\ninterface IArguments {}\ninterface NewableFunction {}\n\
         interface Number {}\ninterface Object {}\ninterface RegExp {}\ninterface String {}\n",
    )
    .expect("write lib");
    let libs = LibStore::new(dir.join("libs"));
    let mut fs = MemoryFs::new();
    fs.set_cwd("/p");
    fs.add_file(
        "/p/tsconfig.json",
        r#"{"compilerOptions":{"strict":true,"lib":["es5"],"types":[],"noEmit":true},"files":["a.ts"]}"#,
    );
    let mut history = History::default();
    let run = CheckRun { threads: 1 };
    for (step, &(src, codes)) in steps.iter().enumerate() {
        fs.add_file("/p/a.ts", src);
        let cfg = load_tsconfig(&fs, "/p/tsconfig.json");
        let program = load_program(&fs, &cfg.file_names, &cfg.options, Some(&libs), 1);
        let expected = check_program(&program, run).diagnostics;
        assert_eq!(
            expected.iter().map(|d| d.code).collect::<Vec<_>>(),
            codes,
            "{src}"
        );
        for repeat in 0..2 {
            let (actual, _) = history.check(&program, run, Checker::new);
            assert_eq!(actual, expected, "{src}");
            let persisted = persist::check(&fs, Some(&cache), &cfg, Some(&libs), 1);
            assert_eq!(persisted.diagnostics, expected, "{src}");
            if step > 0 && repeat == 0 {
                assert_eq!(persisted.mode, persist::Mode::Incremental);
                assert!(!persisted.phases.iter().any(|(name, _)| *name == "fallback"));
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// `noLib`: the missing core global types (TS2318) come through history as through a plain
/// check, cold and warm.
#[test]
fn no_lib_reports_missing_global_types() {
    let dir = std::env::temp_dir().join(format!("ts-r-no-lib-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let cache = ts_r_cache::Cache::open_in(&dir, persist::NAMESPACE, persist::FORMAT_VERSION)
        .expect("cache");
    let libs = LibStore::new(dir.join("libs"));
    let mut fs = MemoryFs::new();
    fs.set_cwd("/p");
    fs.add_file(
        "/p/tsconfig.json",
        r#"{"compilerOptions":{"noLib":true,"types":[],"noEmit":true},"files":["a.ts"]}"#,
    );
    fs.add_file("/p/a.ts", "export const a = 1;\n");
    let cfg = load_tsconfig(&fs, "/p/tsconfig.json");
    let program = load_program(&fs, &cfg.file_names, &cfg.options, None, 1);
    let expected = check_program(&program, CheckRun { threads: 1 }).diagnostics;
    assert_eq!(expected.iter().filter(|d| d.code == 2318).count(), 10);
    for _ in 0..2 {
        let persisted = persist::check(&fs, Some(&cache), &cfg, Some(&libs), 1);
        assert_eq!(persisted.diagnostics, expected);
    }
    let _ = std::fs::remove_dir_all(&dir);
}
