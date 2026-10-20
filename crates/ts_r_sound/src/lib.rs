//! Sound mode (design record `docs/design/0007-sound-mode.md`): in files that opt in, a
//! static type is a guarantee about the runtime value unless a written escape hatch says
//! otherwise. This crate runs after the regular check: it decides each file's level
//! (configuration and pragmas), re-checks sound and strict files with the forced flags, runs
//! the 14 rules over them through the checker's query API, applies precedence and
//! `@ts-r-unsafe` hatches, and returns `TSR` diagnostics plus the data of the coverage report.
//!
//! ```ignore
//! let base = check_program(&program, CheckRun { threads: 4 });
//! let sound = check_sound(&SoundRun { program: &program, config: &cfg,
//!     base: &base.diagnostics, threads: 4, case_sensitive: true });
//! ```

pub mod codes;
pub mod config;
mod ctx;
mod forced;
pub mod pragma;
pub mod report;
mod rules;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use rustc_hash::FxHashSet;
use ts_r_ast::Node;
use ts_r_checker::Layer;
use ts_r_checker::checker::{Checker, Ck};
use ts_r_checker::program::ProgramFiles;
use ts_r_diagnostics::{Diagnostic, FileId};

pub use config::{Level, PackageMode, SoundConfig};

use crate::ctx::{Ctx, Info};
use crate::pragma::Raw;

/// Stack size of sound-pass threads (the checker recurses deeply, like its own driver).
const STACK: usize = 256 << 20;

/// Precedence when several rules report the same node (record: "One diagnostic per node").
const PRECEDENCE: [u8; 14] = [14, 1, 11, 12, 2, 3, 7, 8, 13, 10, 6, 5, 9, 4];

/// Inputs of a sound pass.
#[derive(Debug)]
pub struct SoundRun<'a> {
    pub program: &'a ProgramFiles,
    pub config: &'a SoundConfig,
    /// Diagnostics of the regular check (used to tell forced-flag errors apart).
    pub base: &'a [Diagnostic],
    /// Worker threads (clamped to 1..=4).
    pub threads: usize,
    pub case_sensitive: bool,
}

/// One file's level, for the report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileLevel {
    pub path: String,
    pub level: Level,
    pub lines: u32,
}

/// One `@ts-r-unsafe` directive, for the report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HatchEntry {
    pub path: String,
    /// One-based line of the directive.
    pub line: u32,
    /// `S-02` style ids; empty for "every rule".
    pub rules: Vec<String>,
    pub reason: String,
    pub used: bool,
}

/// The result of a sound pass.
#[derive(Debug, Default)]
pub struct SoundResult {
    /// Sound diagnostics (`TSR` codes, plus forced-flag errors kept with their TS code under
    /// `forced-flag-codes = "TS"`), sorted by file path and position.
    pub diagnostics: Vec<Diagnostic>,
    /// Every project file (not lib, not `node_modules`) with its level.
    pub files: Vec<FileLevel>,
    pub hatches: Vec<HatchEntry>,
    /// Unchecked boundaries: `loose` modules, project `.d.ts` files.
    pub boundaries: Vec<String>,
    /// Rules switched off by `[sound.rules]` (the guarantee is partial).
    pub rules_off: Vec<u8>,
}

impl SoundResult {
    /// Number of sound diagnostics per rule (index 0 general, 1..=14).
    pub fn per_rule(&self) -> [usize; 15] {
        let mut out = [0usize; 15];
        for d in &self.diagnostics {
            if let Some(n) = codes::sound_number(d.code) {
                out[codes::rule_of(n).min(14) as usize] += 1;
            }
        }
        out
    }
}

fn is_external_path(p: &str) -> bool {
    p.contains("/node_modules/")
}

/// Every ambient module declared without a body (`declare module "x";`) in the program.
fn shorthand_modules(program: &ProgramFiles) -> FxHashSet<String> {
    let mut out = FxHashSet::default();
    for f in &program.files {
        if f.is_lib {
            continue;
        }
        let ast = &f.ast;
        let Node::Program { stmts } = *ast.node(ast.root) else {
            continue;
        };
        for &s in ast.list(stmts) {
            if let Node::Module {
                name, body: None, ..
            } = *ast.node(s)
                && let Node::StringLit { value } = *ast.node(name)
            {
                out.insert(ast.atom(value).to_owned());
            }
        }
    }
    out
}

/// Runs sound mode over `run.program`.
pub fn check_sound(run: &SoundRun<'_>) -> SoundResult {
    check_sound_inner(run, None, None)
}

/// Prepares a forced program before its checker is built (the editor orders it by level and
/// returns the shared level to layer the checker on). It must keep the file order: file ids
/// are shared with the regular check.
pub type PrepareForced<'a> = &'a dyn Fn(&mut ProgramFiles) -> Option<Arc<Layer>>;

/// [`check_sound`] for some files only (indices into `run.program.files`), as the editor
/// needs it for open files: other files get their level but no pass. `prepare` is called on
/// each forced program (see [`PrepareForced`]); without it, checkers are built from scratch.
pub fn check_sound_files(
    run: &SoundRun<'_>,
    files: &[usize],
    prepare: Option<PrepareForced<'_>>,
) -> SoundResult {
    check_sound_inner(run, Some(files), prepare)
}

/// The level of one file of `program` under `config`: its pragma, else the configuration.
/// `.d.ts`, JavaScript and `node_modules` files are always off.
pub fn file_level(
    program: &ProgramFiles,
    file: usize,
    config: &SoundConfig,
    case_sensitive: bool,
) -> Level {
    let Some(f) = program.files.get(file) else {
        return Level::Off;
    };
    if f.is_lib || is_external_path(&f.path) || f.is_external || f.is_declaration() || f.is_js() {
        return Level::Off;
    }
    let (pragma, _) = pragma::read_pragma(&f.ast, &f.text);
    pragma.unwrap_or_else(|| config.configured_level(&f.path, case_sensitive))
}

/// The pragma problems of one file (TSR0004 unknown level, TSR0005 late pragma), as
/// [`check_sound_files`] reports them, for a file that gets no sound pass (`off`).
pub fn pragma_diagnostics(program: &ProgramFiles, file: usize) -> Vec<Diagnostic> {
    let Some(f) = program.files.get(file) else {
        return Vec::new();
    };
    if f.is_lib || is_external_path(&f.path) || f.is_external || f.is_declaration() || f.is_js() {
        return Vec::new();
    }
    let (_, raw) = pragma::read_pragma(&f.ast, &f.text);
    raw.into_iter()
        .map(|r| codes::diagnostic(r.msg, FileId(file as u32), r.span, &[]).with_text(r.text))
        .collect()
}

fn check_sound_inner(
    run: &SoundRun<'_>,
    only: Option<&[usize]>,
    prepare: Option<PrepareForced<'_>>,
) -> SoundResult {
    let program = run.program;
    let n = program.files.len();
    let mut levels = vec![Level::Off; n];
    let mut pragma_diags: Vec<Vec<Raw>> = (0..n).map(|_| Vec::new()).collect();
    let mut result = SoundResult {
        rules_off: run.config.rules_off.clone(),
        ..SoundResult::default()
    };
    for (i, f) in program.files.iter().enumerate() {
        if f.is_lib || is_external_path(&f.path) || f.is_external {
            continue;
        }
        if f.is_declaration() {
            result
                .boundaries
                .push(format!("declaration file {}", f.path));
        }
        if f.is_declaration() || f.is_js() {
            result.files.push(FileLevel {
                path: f.path.clone(),
                level: Level::Off,
                lines: f.lines().line_count() as u32,
            });
            continue;
        }
        let (pragma, diags) = pragma::read_pragma(&f.ast, &f.text);
        pragma_diags[i] = diags;
        levels[i] =
            pragma.unwrap_or_else(|| run.config.configured_level(&f.path, run.case_sensitive));
        result.files.push(FileLevel {
            path: f.path.clone(),
            level: levels[i],
            lines: f.lines().line_count() as u32,
        });
    }
    let mut loose_modules = shorthand_modules(program);
    for (name, mode) in &run.config.packages {
        if *mode == PackageMode::Loose {
            loose_modules.insert(name.clone());
        } else {
            loose_modules.remove(name);
        }
    }
    let mut loose_sorted: Vec<&String> = loose_modules.iter().collect();
    loose_sorted.sort();
    for m in loose_sorted {
        result.boundaries.push(format!("loose module \"{m}\""));
    }
    let info = Info {
        program,
        config: run.config,
        levels,
        loose_modules,
    };
    let wanted = |i: usize| only.is_none_or(|o| o.contains(&i));
    let targets: Vec<usize> = (0..n)
        .filter(|&i| info.levels[i] >= Level::Strict && wanted(i))
        .collect();
    let mut out: Vec<Diagnostic> = Vec::new();
    for (i, ds) in pragma_diags.into_iter().enumerate() {
        if !wanted(i) {
            continue;
        }
        for r in ds {
            out.push(codes::diagnostic(r.msg, FileId(i as u32), r.span, &[]).with_text(r.text));
        }
    }
    if !targets.is_empty() {
        let mut forced = forced::forced_programs(program);
        let mut layers: [Option<Arc<Layer>>; 2] = [None, None];
        if let Some(prepare) = prepare {
            layers[0] = prepare(&mut forced.all);
            if let Some(p) = forced.strict_only.as_mut() {
                layers[1] = prepare(p);
            }
        }
        let checker = build(&forced.all, &layers[0]);
        let strict_checker = forced.strict_only.as_ref().map(|p| build(p, &layers[1]));
        let threads = run.threads.clamp(1, 4).min(targets.len());
        let next = AtomicUsize::new(0);
        let results: Vec<Mutex<FileOut>> = targets.iter().map(|_| Mutex::default()).collect();
        let work = || {
            let mut ck: Option<Ck<'_, '_>> = None;
            let mut sck: Option<Ck<'_, '_>> = None;
            loop {
                let k = next.fetch_add(1, Ordering::Relaxed);
                let Some(&fi) = targets.get(k) else { break };
                let file = FileId(fi as u32);
                let ck = ck.get_or_insert_with(|| Ck::new(&checker, file));
                let strict_diags = strict_checker.as_ref().map(|c| {
                    sck.get_or_insert_with(|| Ck::new(c, file))
                        .check_source_file(file)
                });
                let fo = check_file(ck, &info, &forced, file, run.base, strict_diags.as_deref());
                *results[k].lock().unwrap_or_else(|e| e.into_inner()) = fo;
            }
        };
        std::thread::scope(|s| {
            let mut handles = Vec::new();
            for _ in 0..threads {
                let work = &work;
                match std::thread::Builder::new()
                    .stack_size(STACK)
                    .spawn_scoped(s, work)
                {
                    Ok(h) => handles.push(h),
                    Err(_) => work(),
                }
            }
            for h in handles {
                let _ = h.join();
            }
        });
        for (k, r) in results.into_iter().enumerate() {
            let fo = r.into_inner().unwrap_or_else(|e| e.into_inner());
            let path = &program.files[targets[k]].path;
            out.extend(fo.diagnostics);
            for h in fo.hatches {
                result.hatches.push(HatchEntry {
                    path: path.clone(),
                    ..h
                });
            }
        }
    }
    let path_of = |d: &Diagnostic| -> &str {
        program
            .files
            .get(d.file.0 as usize)
            .map_or("", |f| f.path.as_str())
    };
    out.sort_by(|a, b| {
        path_of(a)
            .cmp(path_of(b))
            .then(a.span.start.cmp(&b.span.start))
            .then(a.span.len().cmp(&b.span.len()))
            .then(a.code.cmp(&b.code))
            .then(a.message.cmp(&b.message))
    });
    out.dedup_by(|a, b| a.file == b.file && a.span == b.span && a.code == b.code);
    result.diagnostics = out;
    result
}

/// A checker over `p`, on the shared level `l` when there is one.
fn build<'p>(p: &'p ProgramFiles, l: &Option<Arc<Layer>>) -> Checker<'p> {
    match l {
        Some(l) => Checker::layered(p, Some(l)).unwrap_or_else(|_| Checker::new(p)),
        None => Checker::new(p),
    }
}

#[derive(Debug, Default)]
struct FileOut {
    diagnostics: Vec<Diagnostic>,
    hatches: Vec<HatchEntry>,
}

trait WithText {
    fn with_text(self, text: String) -> Self;
}

impl WithText for Diagnostic {
    fn with_text(mut self, text: String) -> Self {
        self.message = text;
        self
    }
}

/// Checks one sound or strict file.
fn check_file(
    ck: &mut Ck<'_, '_>,
    info: &Info<'_>,
    forced: &forced::Forced,
    file: FileId,
    base: &[Diagnostic],
    strict_diags: Option<&[Diagnostic]>,
) -> FileOut {
    let forced_diags = ck.check_source_file(file);
    let level = info.level(file);
    let base_here: Vec<&Diagnostic> = base.iter().filter(|d| d.file == file).collect();
    let mut out = FileOut::default();
    let mut raws: Vec<Raw> = Vec::new();
    if level.has_rule(4) && info.config.rule_on(4) {
        let (r, kept) = forced::recode(
            &base_here,
            strict_diags,
            &forced_diags,
            &forced.newly,
            info.config.policy.forced_flags_keep_tsc_codes,
        );
        raws.extend(r);
        out.diagnostics.extend(kept);
    }
    {
        let mut cx = Ctx::new(ck, info, file);
        rules::run_all(&mut cx);
        raws.append(&mut cx.out);
    }
    ck.diags.clear();
    let raws = apply_precedence(raws);
    let sf = ck.c.file(file);
    let (mut hatches, hatch_diags) = pragma::read_hatches(&sf.ast, &sf.text, sf.lines());
    let mut kept: Vec<Raw> = Vec::new();
    for r in raws {
        let line = sf.lines().line_of(r.span.start);
        let rule = r.msg.rule();
        let mut suppressed = false;
        for h in hatches.iter_mut() {
            if h.valid && line == h.line + 1 && h.covers(rule) {
                h.used = true;
                suppressed = true;
            }
        }
        if !suppressed {
            kept.push(r);
        }
    }
    kept.extend(hatch_diags);
    for h in &hatches {
        if h.valid && !h.used {
            kept.push(Raw::new(codes::UNUSED_UNSAFE, h.span, &[]));
        }
    }
    for r in kept {
        out.diagnostics
            .push(codes::diagnostic(r.msg, file, r.span, &[]).with_text(r.text));
    }
    out.hatches = hatches
        .into_iter()
        .filter(|h| h.valid)
        .map(|h| HatchEntry {
            path: String::new(),
            line: h.line + 1,
            rules: h
                .rules
                .unwrap_or_default()
                .into_iter()
                .map(|r| format!("S-{r:02}"))
                .collect(),
            reason: h.reason,
            used: h.used,
        })
        .collect();
    out
}

/// Keeps one diagnostic per node (same span): the first rule in [`PRECEDENCE`].
fn apply_precedence(mut raws: Vec<Raw>) -> Vec<Raw> {
    let rank = |r: &Raw| {
        let rule = r.msg.rule();
        PRECEDENCE
            .iter()
            .position(|&x| x == rule)
            .unwrap_or(PRECEDENCE.len())
    };
    raws.sort_by(|a, b| {
        (a.span.start, a.span.end, rank(a), a.msg.n).cmp(&(
            b.span.start,
            b.span.end,
            rank(b),
            b.msg.n,
        ))
    });
    let mut out: Vec<Raw> = Vec::with_capacity(raws.len());
    for r in raws {
        if let Some(last) = out.last()
            && last.span == r.span
            && last.msg.rule() != 0
            && r.msg.rule() != 0
        {
            continue;
        }
        out.push(r);
    }
    out
}
