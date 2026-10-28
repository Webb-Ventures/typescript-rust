//! Lint in the daemon (L2): ts-r-lint diagnostics and code actions for open documents.
//!
//! * A [`LintWorker`] thread owns the lint engine: config lookup (the same
//!   [`ConfigLookup`] / [`load_configs`] as `ts-r-lint`, so ESLint flat configs are evaluated
//!   by Node once and then come from the `lint-config` cache), resolved per-file configs and
//!   rule instances. The daemon loop sends it [`Job`]s (a path and the text a client has) and
//!   gets [`Done`] events back, so linting never blocks type requests.
//! * Only files governed by an ESLint, oxlint or Biome config are linted. Without a config
//!   the editor shows nothing (as the ESLint extension does), unlike `ts-r-lint`'s built-in
//!   default for the CLI.
//! * Queued jobs for the same file collapse to the newest text when they are queued, so a
//!   busy worker holds one waiting text per file, not one per edit.
//! * A file that does not parse keeps its previous lint results (the type backend reports
//!   the syntax error), so diagnostics do not flicker while typing.
//! * Code actions ([`code_actions`]) are computed in the daemon from the last results: the
//!   rule's fix, its suggestions, "disable for this line", "disable for the entire file",
//!   "fix all problems of this rule" and `source.fixAll.ts-r` (one non-overlapping pass of
//!   every fix, like one `--fix` pass).
//! * Type-aware rules: with the ts-r type backend, each project thread runs the type-aware
//!   half of a file's rules ([`TypedConfigs`]) on the checker worker that just checked the
//!   file and publishes them with its semantic diagnostics (`tsr/project/lint.rs`); the lint
//!   worker runs the rest without types. Without it they are listed as not run.

use crate::text::{Pos, TextDoc, to_lsp_range};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};
use ts_r_ast::{NodeId, NodeKind, SourceKind};
use ts_r_lint::config::ConfigFile;
use ts_r_lint::config::biome::BIOME_CONFIG_NAMES;
use ts_r_lint::config::eslint_eval::ESLINT_CONFIG_NAMES;
use ts_r_lint::config::oxlint::OXLINT_CONFIG_NAMES;
use ts_r_lint::engine::FileConfig;
use ts_r_lint::project::{
    ConfigLookup, ConfigRef, LINTABLE_EXTENSIONS, LintOptions, WORKER_STACK, load_configs,
};
use ts_r_lint::types::FileKey;
use ts_r_lint::{Diagnostic, Edit, NoTypeInfo, Registry, Severity, lint_source};
use ts_r_parser::{ParseOptions, TsInJs};

/// `source` of every lint diagnostic.
pub const SOURCE: &str = "ts-r-lint";
/// Code action kind of "fix everything" (`editor.codeActionsOnSave: {"source.fixAll": ...}`).
pub const FIX_ALL_KIND: &str = "source.fixAll.ts-r";

/// Whether a document key is a file ts-r-lint lints (absolute path, known extension).
pub fn is_lintable(key: &str) -> bool {
    let p = Path::new(key);
    // Keys are `/`-separated absolute paths (`/w/a.ts`, `c:/w/a.ts`) on every OS, so this is
    // a string test: `Path::is_absolute` says no to `/w/a.ts` on Windows (no drive).
    let b = key.as_bytes();
    let absolute = key.starts_with('/')
        || (b.len() >= 3
            && b[0].is_ascii_alphabetic()
            && b[1] == b':'
            && matches!(b[2], b'/' | b'\\'))
        || p.is_absolute();
    absolute
        && p.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| LINTABLE_EXTENSIONS.contains(&e))
}

/// Whether a path is a lint config file (a change re-reads configs and re-lints every open
/// file).
pub fn is_config_file(key: &str) -> bool {
    let name = Path::new(key)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    ESLINT_CONFIG_NAMES.contains(&name)
        || OXLINT_CONFIG_NAMES.contains(&name)
        || BIOME_CONFIG_NAMES.contains(&name)
}

/// One lint request.
#[derive(Debug)]
pub struct Job {
    /// Document key (absolute path).
    pub key: String,
    /// The text to lint (a client's current text).
    pub text: String,
    /// The client whose text it is: a newer job of the same client replaces this one, another
    /// client's (different unsaved text of the same file) does not.
    pub client: u64,
}

/// What waits for the worker.
#[derive(Debug, Default)]
struct Queue {
    /// At most one job per file and client: a new one replaces the waiting one.
    jobs: Vec<Job>,
    /// Forget loaded configs before the next jobs.
    reload: bool,
    /// The [`LintWorker`] was dropped: the thread exits.
    closed: bool,
}

type Shared = Arc<(Mutex<Queue>, Condvar)>;

/// Results of linting one text.
#[derive(Debug)]
pub struct Linted {
    /// The text that was linted.
    pub text: TextDoc,
    /// ts-r-lint's diagnostics (byte spans into `text` without its BOM).
    pub diags: Vec<Diagnostic>,
    /// The same diagnostics as LSP diagnostics.
    pub lsp: Vec<Value>,
    /// 3 when `text` starts with a BOM (ts-r-lint lints the text without it), else 0.
    pub bom: u32,
    /// The grammar the text was linted with (whether it can contain JSX).
    pub kind: SourceKind,
    /// Whether that grammar took TypeScript syntax in a JavaScript file.
    pub ts_in_js: TsInJs,
}

impl Linted {
    /// No results for `text` (a file that is not linted).
    pub fn empty(text: TextDoc) -> Linted {
        Linted {
            text,
            diags: Vec::new(),
            lsp: Vec::new(),
            bom: 0,
            kind: SourceKind::Ts,
            ts_in_js: TsInJs::Reject,
        }
    }
}

/// What the worker found for one job.
#[derive(Debug)]
pub enum Outcome {
    /// Linted.
    Linted(Linted),
    /// The text does not parse: keep the previous results.
    ParseError(TextDoc),
    /// Not linted (no config, ignored by the config, config failed to load): no
    /// diagnostics.
    NotLinted(TextDoc, String),
}

impl Outcome {
    /// The text this outcome is about.
    pub fn text(&self) -> &TextDoc {
        match self {
            Outcome::Linted(l) => &l.text,
            Outcome::ParseError(t) | Outcome::NotLinted(t, _) => t,
        }
    }
}

/// A config the worker loaded for the first time, for the once-per-client note.
#[derive(Clone, Debug)]
pub struct ConfigNote {
    /// Config path relative to the workspace root.
    pub name: String,
    /// Configured rules ts-r-lint does not run: `(rule, reason)`.
    pub unsupported: Vec<(String, &'static str)>,
    /// Notes from loading (function patterns, processors, invalid rule options).
    pub notes: Vec<String>,
    /// Why the config could not be loaded.
    pub error: Option<String>,
}

/// A finished job.
#[derive(Debug)]
pub struct Done {
    /// Document key.
    pub key: String,
    /// Result.
    pub outcome: Outcome,
    /// The governing config (relative name), if any.
    pub config: Option<String>,
    /// Set the first time a config is loaded.
    pub new_config: Option<ConfigNote>,
    /// Time spent in the worker (config resolution + lint + conversion).
    pub elapsed: Duration,
    /// A file a loaded config imports changed, so the config was loaded again: other open
    /// files' results are stale.
    pub config_changed: bool,
}

/// The lint thread.
#[derive(Debug)]
pub struct LintWorker {
    queue: Shared,
}

impl LintWorker {
    /// Starts the worker for workspace `root`; `done` is called on the worker thread for
    /// every finished job (it posts an event to the daemon loop).
    pub fn spawn(
        root: PathBuf,
        done: impl Fn(Done) + Send + 'static,
    ) -> std::io::Result<LintWorker> {
        let queue = Shared::default();
        let q = queue.clone();
        thread::Builder::new()
            .name("lint".into())
            .stack_size(WORKER_STACK)
            .spawn(move || worker_loop(root, q, done))?;
        Ok(LintWorker { queue })
    }

    fn with_queue(&self, f: impl FnOnce(&mut Queue)) {
        let (m, cv) = &*self.queue;
        if let Ok(mut q) = m.lock() {
            f(&mut q);
            cv.notify_one();
        }
    }

    /// Queues a job, replacing a waiting job for the same file and client.
    pub fn submit(&self, job: Job) {
        self.with_queue(|q| {
            q.jobs
                .retain(|o| o.key != job.key || o.client != job.client);
            q.jobs.push(job);
        });
    }

    /// Forgets loaded configs (a config file changed); later jobs load them again.
    pub fn reload_configs(&self) {
        self.with_queue(|q| q.reload = true);
    }
}

impl Drop for LintWorker {
    fn drop(&mut self) {
        self.with_queue(|q| q.closed = true);
    }
}

fn worker_loop(root: PathBuf, queue: Shared, done: impl Fn(Done)) {
    let mut engine = Engine::new(root);
    let (m, cv) = &*queue;
    loop {
        // Take everything queued.
        let (reload, jobs) = {
            let Ok(mut q) = m.lock() else { return };
            while !q.closed && !q.reload && q.jobs.is_empty() {
                q = match cv.wait(q) {
                    Ok(q) => q,
                    Err(_) => return,
                };
            }
            if q.closed {
                return;
            }
            (std::mem::take(&mut q.reload), std::mem::take(&mut q.jobs))
        };
        if reload {
            engine.reload();
        }
        for j in jobs {
            let t = Instant::now();
            let (outcome, config, new_config) = engine.lint(&j.key, j.text);
            done(Done {
                key: j.key,
                outcome,
                config,
                new_config,
                elapsed: t.elapsed(),
                config_changed: std::mem::take(&mut engine.deps_changed),
            });
        }
    }
}

struct Engine {
    root: PathBuf,
    opts: LintOptions,
    lookup: ConfigLookup,
    configs: BTreeMap<ConfigRef, Result<Arc<ConfigFile>, String>>,
    /// The files each loaded config imports, with their modification stamp at load time.
    /// Saving one is not a config-file change the daemon sees, so they are checked before
    /// a loaded config is used again.
    deps: BTreeMap<ConfigRef, Vec<(PathBuf, Option<Stamp>)>>,
    /// Set when a config was loaded again because an import changed.
    deps_changed: bool,
}

type Stamp = (SystemTime, u64);

fn stamp(path: &Path) -> Option<Stamp> {
    let m = std::fs::metadata(path).ok()?;
    Some((m.modified().ok()?, m.len()))
}

impl Engine {
    fn new(root: PathBuf) -> Engine {
        Engine {
            opts: LintOptions::new(root.clone()),
            root,
            lookup: ConfigLookup::new(),
            configs: BTreeMap::new(),
            deps: BTreeMap::new(),
            deps_changed: false,
        }
    }

    fn reload(&mut self) {
        self.lookup = ConfigLookup::new();
        self.configs.clear();
        self.deps.clear();
    }

    fn name_of(&self, r: &ConfigRef) -> Option<String> {
        match r {
            ConfigRef::Eslint(p) | ConfigRef::Oxlint(p) | ConfigRef::Biome(p) => Some(
                p.strip_prefix(&self.root)
                    .unwrap_or(p)
                    .to_string_lossy()
                    .into_owned(),
            ),
            ConfigRef::Default => None,
        }
    }

    /// The config for `path`: `(config name, note if newly loaded, resolved file config or why
    /// the file is not linted)`.
    fn config_for(
        &mut self,
        path: &Path,
    ) -> (
        Option<String>,
        Option<ConfigNote>,
        Result<Arc<FileConfig>, String>,
    ) {
        let r = self
            .lookup
            .governing(path.parent().unwrap_or(Path::new("/")));
        let Some(name) = self.name_of(&r) else {
            return (
                None,
                None,
                Err("no ESLint, oxlint or Biome config governs this file".into()),
            );
        };
        let mut note = None;
        if self
            .deps
            .get(&r)
            .is_some_and(|d| d.iter().any(|(p, s)| stamp(p) != *s))
        {
            self.configs.remove(&r);
            self.deps_changed = true;
        }
        if !self.configs.contains_key(&r) {
            let loaded = load_configs(std::slice::from_ref(&r), &self.opts, None)
                .and_then(|mut m| m.remove(&r).ok_or_else(|| "config not loaded".to_string()))
                .map(Arc::new);
            let mut n = ConfigNote {
                name: name.clone(),
                unsupported: Vec::new(),
                notes: Vec::new(),
                error: None,
            };
            match &loaded {
                Ok(cfg) => {
                    n.unsupported = cfg
                        .unsupported(false)
                        .into_iter()
                        .map(|(rule, why)| (rule, why.describe()))
                        .collect();
                    n.notes = cfg.notes();
                }
                Err(e) => n.error = Some(e.clone()),
            }
            note = Some(n);
            let deps = match &loaded {
                Ok(cfg) => cfg.deps.iter().map(|p| (p.clone(), stamp(p))).collect(),
                Err(_) => Vec::new(),
            };
            self.deps.insert(r.clone(), deps);
            self.configs.insert(r.clone(), loaded);
        }
        let fc = match &self.configs[&r] {
            Err(e) => Err(format!("{name} could not be loaded: {e}")),
            Ok(cfg) => match cfg.relative(path) {
                None => Err(format!("outside the directory of {name}")),
                Some(rel) => cfg
                    .file_config(&rel, &self.opts.resolve)
                    .map_err(|why| format!("not linted by {name} ({why:?})")),
            },
        };
        if let Some(n) = &mut note
            && let Ok(mut extra) = self.opts.resolve.notes.lock()
        {
            n.notes.append(&mut extra);
        }
        (Some(name), note, fc)
    }

    fn lint(&mut self, key: &str, text: String) -> (Outcome, Option<String>, Option<ConfigNote>) {
        let (name, note, fc) = self.config_for(Path::new(key));
        let doc = TextDoc::new(text);
        let fc = match fc {
            Ok(fc) => fc,
            Err(why) => return (Outcome::NotLinted(doc, why), name, note),
        };
        let full = doc.text();
        let (src, bom) = match full.strip_prefix('\u{feff}') {
            Some(s) => (s, 3),
            None => (full, 0),
        };
        let r = lint_source(key, src, &fc, &NoTypeInfo, FileKey(0));
        if r.diagnostics.iter().any(|d| d.fatal) {
            return (Outcome::ParseError(doc), name, note);
        }
        let lsp = lsp_diagnostics(&r.diagnostics, &doc, bom);
        let linted = Linted {
            diags: r.diagnostics,
            lsp,
            bom,
            kind: SourceKind::from_path(key),
            ts_in_js: if fc.env.typescript {
                TsInJs::Allow
            } else {
                TsInJs::Reject
            },
            text: doc,
        };
        (Outcome::Linted(linted), name, note)
    }
}

/// Lint configs for a type backend's project thread: the type-aware half of each file's
/// rules (the lint worker runs the other half). Loaded configs and per-file results are
/// cached until [`TypedConfigs::reload`].
#[derive(Default)]
pub struct TypedConfigs {
    engine: Option<Engine>,
    files: BTreeMap<String, Option<Arc<FileConfig>>>,
}

impl std::fmt::Debug for TypedConfigs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TypedConfigs")
            .field("files", &self.files.len())
            .finish_non_exhaustive()
    }
}

impl TypedConfigs {
    /// Forgets loaded configs (a lint config changed).
    pub fn reload(&mut self) {
        self.engine = None;
        self.files.clear();
    }

    /// Whether [`TypedConfigs::config_for`] has answered for `path` (no config to load).
    pub fn is_known(&self, path: &str) -> bool {
        self.files.contains_key(path)
    }

    /// What [`TypedConfigs::config_for`] last answered for `path` (`None` if not asked yet).
    pub fn cached(&self, path: &str) -> Option<Arc<FileConfig>> {
        self.files.get(path).cloned().flatten()
    }

    /// The type-aware rules for `path` under workspace `root`, or `None` when no config
    /// governs the file or it enables none.
    pub fn config_for(&mut self, root: &Path, path: &str) -> Option<Arc<FileConfig>> {
        if !is_lintable(path) {
            return None;
        }
        if let Some(c) = self.files.get(path) {
            return c.clone();
        }
        let engine = self.engine.get_or_insert_with(|| {
            let mut e = Engine::new(root.to_path_buf());
            e.opts.resolve.types_available = true;
            e
        });
        let (_, _, fc) = engine.config_for(Path::new(path));
        let typed = fc
            .ok()
            .filter(|fc| fc.rules.has_typed())
            .map(|fc| Arc::new(fc.typed_only()));
        self.files.insert(path.to_string(), typed.clone());
        typed
    }
}

/// Drops the "needs type information" entries from a config note: the ts-r type backend
/// runs those rules.
pub fn typed_rules_run(n: &mut ConfigNote) {
    let why = ts_r_lint::config::mapping::UnsupportedReason::TypeCheckOff.describe();
    n.unsupported.retain(|(_, w)| *w != why);
}

/// Byte offset (in the linted text without BOM) -> LSP position.
fn pos(doc: &TextDoc, bom: u32, offset: u32) -> Pos {
    doc.position_at((offset + bom) as usize)
}

/// LSP ranges of byte spans (in the linted text without BOM), converted in one pass: a
/// long line with many problems is not rescanned from its start for each of them.
fn lsp_ranges(doc: &TextDoc, bom: u32, spans: impl Iterator<Item = (u32, u32)>) -> Vec<Value> {
    let offsets: Vec<usize> = spans
        .flat_map(|(start, end)| [start, end])
        .map(|o| (o + bom) as usize)
        .collect();
    doc.positions_at(&offsets)
        .chunks(2)
        .map(|p| to_lsp_range(p[0], p[1]))
        .collect()
}

/// ts-r-lint's diagnostics as LSP diagnostics.
pub fn lsp_diagnostics(diags: &[Diagnostic], doc: &TextDoc, bom: u32) -> Vec<Value> {
    let ranges = lsp_ranges(doc, bom, diags.iter().map(|d| (d.span.start, d.span.end)));
    diags
        .iter()
        .zip(ranges)
        .map(|(d, r)| lsp_diagnostic_at(d, r))
        .collect()
}

/// One ts-r-lint diagnostic as an LSP diagnostic.
pub fn lsp_diagnostic(d: &Diagnostic, doc: &TextDoc, bom: u32) -> Value {
    lsp_diagnostic_at(
        d,
        to_lsp_range(pos(doc, bom, d.span.start), pos(doc, bom, d.span.end)),
    )
}

/// One ts-r-lint diagnostic as an LSP diagnostic with its range already converted.
pub fn lsp_diagnostic_at(d: &Diagnostic, range: Value) -> Value {
    let mut v = json!({
        "range": range,
        "severity": if d.severity == Severity::Error { 1 } else { 2 },
        "source": SOURCE,
        "message": d.message,
    });
    if let Some(rule) = d.rule {
        v["code"] = json!(rule);
        if let Some(def) = Registry::global().find(rule) {
            v["codeDescription"] = json!({"href": def.meta.docs_url()});
        }
    }
    v
}

fn text_edit(doc: &TextDoc, bom: u32, e: &Edit) -> Value {
    json!({
        "range": to_lsp_range(pos(doc, bom, e.span.start), pos(doc, bom, e.span.end)),
        "newText": e.text,
    })
}

fn text_edits(doc: &TextDoc, bom: u32, edits: &[Edit]) -> Vec<Value> {
    let ranges = lsp_ranges(doc, bom, edits.iter().map(|e| (e.span.start, e.span.end)));
    edits
        .iter()
        .zip(ranges)
        .map(|(e, r)| json!({"range": r, "newText": e.text}))
        .collect()
}

fn ranges_touch(a: &Value, start: Pos, end: Pos) -> bool {
    let (Some(s), Some(e)) = (Pos::from_lsp(&a["start"]), Pos::from_lsp(&a["end"])) else {
        return false;
    };
    let key = |p: Pos| (p.line, p.character);
    key(s) <= key(end) && key(start) <= key(e)
}

/// Non-overlapping merged edits of `fixes` (ESLint's one-pass rule: a fix that starts at or
/// before the end of the previous one waits for the next pass).
fn one_pass(src: &str, fixes: &[&ts_r_lint::Fix]) -> Vec<Edit> {
    let mut merged: Vec<Edit> = fixes
        .iter()
        .filter(|f| !f.edits.is_empty())
        .map(|f| f.merged(src))
        .filter(|e| e.span.start <= e.span.end && (e.span.end as usize) <= src.len())
        .collect();
    merged.sort_by_key(|e| (e.span.start, e.span.end));
    let mut out: Vec<Edit> = Vec::new();
    for e in merged {
        if out.last().is_some_and(|l| l.span.end >= e.span.start) {
            continue;
        }
        out.push(e);
    }
    out
}

fn workspace_edit(uri: &str, edits: Vec<Value>) -> Value {
    let mut changes = serde_json::Map::new();
    changes.insert(uri.to_string(), Value::Array(edits));
    json!({ "changes": changes })
}

/// Content of line `line` (no line break) and the byte offset where it starts.
fn line_text(doc: &TextDoc, line: u32) -> (&str, usize) {
    let text = doc.text();
    let start = doc.offset_at(Pos::new(line, 0));
    let rest = &text[start..];
    let end = rest.find('\n').unwrap_or(rest.len());
    (rest[..end].trim_end_matches('\r'), start)
}

fn newline(doc: &TextDoc) -> &'static str {
    if doc.text().contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    }
}

/// Byte spans of the JSX text in `src` (which a disable comment must not be inserted
/// into), if it has any.
fn jsx_text_spans(src: &str, kind: SourceKind, ts_in_js: TsInJs) -> Vec<(u32, u32)> {
    // Every JSX element or fragment ends with `</` or `/>`.
    if !kind.is_jsx() || !(src.contains("</") || src.contains("/>")) {
        return Vec::new();
    }
    let ast = ts_r_parser::parse_with(src, kind, ParseOptions { ts_in_js }).ast;
    (1..ast.nodes.len() as u32)
        .filter_map(NodeId::from_raw)
        .filter(|&n| ast.kind(n) == NodeKind::JsxText)
        .map(|n| (ast.span(n).start, ast.span(n).end))
        .collect()
}

/// Inserts `// eslint-disable-next-line rule` above `line`, or adds the rule to such a
/// comment that is already there. Where the line starts in JSX children (the line break
/// before it is JSX text), a `//` comment would be rendered text: the comment is
/// `{/* eslint-disable-next-line rule */}` there.
fn disable_line_edit(doc: &TextDoc, line: u32, rule: &str, jsx_text: &[(u32, u32)]) -> Value {
    let (cur, start) = line_text(doc, line);
    let at = start as u32;
    let jsx = jsx_text.iter().any(|&(a, b)| a < at && at <= b);
    let (open, close) = if jsx {
        ("{/* eslint-disable-next-line ", " */}")
    } else {
        ("// eslint-disable-next-line ", "")
    };
    if line > 0 {
        let (prev, _) = line_text(doc, line - 1);
        let t = prev.trim();
        if let Some(rules) = t
            .strip_prefix(open)
            .and_then(|r| r.strip_suffix(close.trim_start()))
            && !rules.contains("--")
            && !rules.trim().is_empty()
        {
            // At the end of the rule list: the line end, or before ` */}`.
            let tail = if jsx {
                close.len() as u32 - u32::from(!rules.ends_with(' '))
            } else {
                0
            };
            let end = prev.trim_end().encode_utf16().count() as u32 - tail;
            let p = Pos::new(line - 1, end).to_lsp();
            return json!({"range": {"start": p, "end": p}, "newText": format!(", {rule}")});
        }
    }
    let indent: String = cur.chars().take_while(|c| c.is_whitespace()).collect();
    let p = Pos::new(line, 0).to_lsp();
    json!({
        "range": {"start": p, "end": p},
        "newText": format!("{indent}{open}{rule}{close}{}", newline(doc)),
    })
}

/// Inserts `/* eslint-disable rule */` at the top (after a shebang), or adds the rule to
/// such a comment already on that line.
fn disable_file_edit(doc: &TextDoc, rule: &str) -> Value {
    let first = if doc.text().trim_start_matches('\u{feff}').starts_with("#!") {
        1
    } else {
        0
    };
    let (t, _) = line_text(doc, first);
    if let Some(inner) = t
        .trim()
        .strip_prefix("/* eslint-disable ")
        .and_then(|r| r.strip_suffix("*/"))
        && !inner.contains("--")
        && !inner.trim().is_empty()
    {
        let line_end = t.trim_end().encode_utf16().count() as u32;
        // Insert before the closing `*/` (and the space before it).
        let close = line_end - 2 - u32::from(inner.ends_with(' '));
        let p = Pos::new(first, close).to_lsp();
        return json!({"range": {"start": p, "end": p}, "newText": format!(", {rule}")});
    }
    let mut p = Pos::new(first, 0);
    if first == 0 && doc.text().starts_with('\u{feff}') {
        p.character = 1;
    }
    let p = p.to_lsp();
    json!({
        "range": {"start": p, "end": p},
        "newText": format!("/* eslint-disable {rule} */{}", newline(doc)),
    })
}

fn wants(only: &[String], kind: &str) -> bool {
    only.is_empty()
        || only
            .iter()
            .any(|o| kind == o || kind.starts_with(&format!("{o}.")))
}

/// Code actions for `range` of a document whose current text is `l.text`. `only` is the
/// request's `context.only` (empty = everything).
pub fn code_actions(l: &Linted, uri: &str, start: Pos, end: Pos, only: &[String]) -> Vec<Value> {
    let doc = &l.text;
    let src = doc.text().get(l.bom as usize..).unwrap_or("");
    let mut out = Vec::new();
    if wants(only, "quickfix") {
        // JSX text spans shifted by the BOM (offsets into `doc`), parsed only if needed.
        let mut jsx_text: Option<Vec<(u32, u32)>> = None;
        let mut disabled_line: Vec<(u32, &str)> = Vec::new();
        let mut disabled_file: Vec<&str> = Vec::new();
        let mut fix_all_rules: Vec<&str> = Vec::new();
        for (d, lsp) in l.diags.iter().zip(&l.lsp) {
            if !ranges_touch(&lsp["range"], start, end) {
                continue;
            }
            let label = d.rule.unwrap_or("ts-r-lint");
            if let Some(fix) = &d.fix {
                let e = fix.merged(src);
                out.push(json!({
                    "title": format!("Fix this {label} problem"),
                    "kind": "quickfix",
                    "diagnostics": [lsp],
                    "isPreferred": true,
                    "edit": workspace_edit(uri, vec![text_edit(doc, l.bom, &e)]),
                }));
            }
            for s in &d.suggestions {
                let e = s.fix.merged(src);
                out.push(json!({
                    "title": s.desc,
                    "kind": "quickfix",
                    "diagnostics": [lsp],
                    "edit": workspace_edit(uri, vec![text_edit(doc, l.bom, &e)]),
                }));
            }
            let Some(rule) = d.rule else { continue };
            if d.fix.is_some() && !fix_all_rules.contains(&rule) {
                let fixes: Vec<&ts_r_lint::Fix> = l
                    .diags
                    .iter()
                    .filter(|o| o.rule == Some(rule))
                    .filter_map(|o| o.fix.as_ref())
                    .collect();
                if fixes.len() > 1 {
                    fix_all_rules.push(rule);
                    let edits = text_edits(doc, l.bom, &one_pass(src, &fixes));
                    out.push(json!({
                        "title": format!("Fix all {rule} problems"),
                        "kind": "quickfix",
                        "diagnostics": [lsp],
                        "edit": workspace_edit(uri, edits),
                    }));
                }
            }
            let line = Pos::from_lsp(&lsp["range"]["start"]).map_or(0, |p| p.line);
            if !disabled_line.contains(&(line, rule)) {
                disabled_line.push((line, rule));
                let jsx = jsx_text.get_or_insert_with(|| {
                    jsx_text_spans(src, l.kind, l.ts_in_js)
                        .into_iter()
                        .map(|(a, b)| (a + l.bom, b + l.bom))
                        .collect()
                });
                out.push(json!({
                    "title": format!("Disable {rule} for this line"),
                    "kind": "quickfix",
                    "diagnostics": [lsp],
                    "edit": workspace_edit(uri, vec![disable_line_edit(doc, line, rule, jsx)]),
                }));
            }
            if !disabled_file.contains(&rule) {
                disabled_file.push(rule);
                out.push(json!({
                    "title": format!("Disable {rule} for the entire file"),
                    "kind": "quickfix",
                    "diagnostics": [lsp],
                    "edit": workspace_edit(uri, vec![disable_file_edit(doc, rule)]),
                }));
            }
        }
    }
    if !only.is_empty() && wants(only, FIX_ALL_KIND) {
        let fixes: Vec<&ts_r_lint::Fix> = l.diags.iter().filter_map(|d| d.fix.as_ref()).collect();
        let edits = text_edits(doc, l.bom, &one_pass(src, &fixes));
        if !edits.is_empty() {
            out.push(json!({
                "title": "Fix all auto-fixable ts-r-lint problems",
                "kind": FIX_ALL_KIND,
                "edit": workspace_edit(uri, edits),
            }));
        }
    }
    out
}

/// Capabilities the daemon adds for lint.
pub fn code_action_provider() -> Value {
    json!({ "codeActionKinds": ["quickfix", FIX_ALL_KIND] })
}

/// `initialize` result for a lint-only client.
pub fn lint_only_capabilities() -> Value {
    json!({
        "positionEncoding": "utf-16",
        "textDocumentSync": { "openClose": true, "change": 2, "save": { "includeText": false } },
        "codeActionProvider": code_action_provider(),
    })
}

/// Text for the once-per-client note about rules a config enables that are not run.
pub fn unsupported_message(n: &ConfigNote) -> Option<(String, String)> {
    if let Some(e) = &n.error {
        let first = e.lines().next().unwrap_or("");
        return Some((
            format!("ts-r-lint could not load {}: {first}", n.name),
            format!("ts-r-lint could not load {}:\n{e}", n.name),
        ));
    }
    if n.unsupported.is_empty() {
        return None;
    }
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for (_, why) in &n.unsupported {
        *counts.entry(why).or_default() += 1;
    }
    let summary: Vec<String> = counts.iter().map(|(w, c)| format!("{c} {w}")).collect();
    let short = format!(
        "ts-r-lint: {} enables {} rule(s) that are not run in the editor ({}). Keep them in CI with ESLint.",
        n.name,
        n.unsupported.len(),
        summary.join("; ")
    );
    let mut long = format!("ts-r-lint: rules in {} that are not run:\n", n.name);
    for (rule, why) in &n.unsupported {
        long.push_str(&format!("  {rule} ({why})\n"));
    }
    for note in &n.notes {
        long.push_str(&format!("  note: {note}\n"));
    }
    Some((short, long))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ts_r_lint::{Fix, Suggestion};
    use ts_r_span::Span;

    fn diag(rule: &'static str, span: (u32, u32), fix: Option<Fix>) -> Diagnostic {
        Diagnostic {
            rule: Some(rule),
            severity: Severity::Warn,
            span: Span::new(span.0, span.1),
            message: "m".into(),
            message_id: None,
            fix,
            suggestions: Vec::new(),
            fatal: false,
            loc: None,
        }
    }

    fn linted(text: &str, diags: Vec<Diagnostic>) -> Linted {
        let doc = TextDoc::new(text);
        let bom = if text.starts_with('\u{feff}') { 3 } else { 0 };
        let lsp = lsp_diagnostics(&diags, &doc, bom);
        Linted {
            text: doc,
            diags,
            lsp,
            bom,
            kind: SourceKind::Tsx,
            ts_in_js: TsInJs::Reject,
        }
    }

    fn edit_of(a: &Value) -> &Value {
        &a["edit"]["changes"]["file:///a.ts"][0]
    }

    #[test]
    fn a_busy_worker_keeps_one_waiting_text_per_file() {
        let (started, wait_started) = std::sync::mpsc::channel();
        let (release, wait_release) = std::sync::mpsc::channel::<()>();
        let w = LintWorker::spawn(PathBuf::from("/nonexistent-tsr-lint-root"), move |d| {
            let _ = started.send(d.key);
            let _ = wait_release.recv();
        })
        .unwrap();
        let job = |key: &str, text: &str| Job {
            key: key.into(),
            text: text.into(),
            client: 1,
        };
        w.submit(job("/w/a.ts", "0"));
        assert_eq!(wait_started.recv().unwrap(), "/w/a.ts");
        // The worker is busy: edits replace the waiting text instead of piling up.
        for i in 1..100 {
            w.submit(job("/w/a.ts", &i.to_string()));
            w.submit(job("/w/b.ts", &i.to_string()));
        }
        // Another client's text of the same file waits beside it.
        w.submit(Job {
            client: 2,
            ..job("/w/a.ts", "other")
        });
        {
            let q = w.queue.0.lock().unwrap();
            let waiting: Vec<(&str, &str)> = q
                .jobs
                .iter()
                .map(|j| (j.key.as_str(), j.text.as_str()))
                .collect();
            assert_eq!(
                waiting,
                [("/w/a.ts", "99"), ("/w/b.ts", "99"), ("/w/a.ts", "other")]
            );
        }
        drop(release);
    }

    #[test]
    fn lintable_and_config_files() {
        assert!(is_lintable("/w/a.tsx"));
        assert!(!is_lintable("untitled:Untitled-1"));
        assert!(!is_lintable("/w/a.json"));
        assert!(is_lintable("c:/w/a.ts"));
        assert!(is_lintable(r"C:\w\a.ts"));
        assert!(!is_lintable("a.ts"));
        assert!(is_config_file("/w/eslint.config.mjs"));
        assert!(is_config_file("/w/.oxlintrc.json"));
        assert!(!is_config_file("/w/a.ts"));
    }

    #[test]
    fn diagnostics_use_utf16_columns_and_skip_the_bom() {
        let text = "\u{feff}let é = 1;\nvar x;\n";
        let l = linted(text, vec![diag("no-var", (12, 18), None)]);
        assert_eq!(
            l.lsp[0]["range"]["start"],
            json!({"line": 1, "character": 0})
        );
        let l = linted(
            "const s = '😀'; debugger;\n",
            vec![diag("no-debugger", (18, 27), None)],
        );
        assert_eq!(l.lsp[0]["range"]["start"]["character"], 16);
        assert_eq!(l.lsp[0]["code"], "no-debugger");
        assert_eq!(
            l.lsp[0]["codeDescription"]["href"],
            "https://eslint.org/docs/latest/rules/no-debugger"
        );
    }

    #[test]
    fn actions_fix_suggest_and_disable() {
        let text = "function f() {\n    debugger;\n}\n";
        let mut d = diag(
            "no-debugger",
            (19, 28),
            Some(Fix::delete(Span::new(19, 28))),
        );
        d.suggestions.push(Suggestion {
            message_id: None,
            desc: "Remove it".into(),
            fix: Fix::delete(Span::new(19, 28)),
        });
        let l = linted(text, vec![d]);
        let a = code_actions(&l, "file:///a.ts", Pos::new(1, 5), Pos::new(1, 5), &[]);
        let titles: Vec<&str> = a.iter().map(|x| x["title"].as_str().unwrap()).collect();
        assert_eq!(
            titles,
            [
                "Fix this no-debugger problem",
                "Remove it",
                "Disable no-debugger for this line",
                "Disable no-debugger for the entire file"
            ]
        );
        assert_eq!(edit_of(&a[0])["newText"], "");
        assert_eq!(
            edit_of(&a[2])["newText"],
            "    // eslint-disable-next-line no-debugger\n"
        );
        assert_eq!(
            edit_of(&a[3])["newText"],
            "/* eslint-disable no-debugger */\n"
        );
        // Outside the range: nothing.
        assert!(code_actions(&l, "file:///a.ts", Pos::new(0, 0), Pos::new(0, 3), &[]).is_empty());
        // Only fixAll requested: one pass of all fixes.
        let only = vec!["source.fixAll".to_string()];
        let a = code_actions(&l, "file:///a.ts", Pos::new(0, 0), Pos::new(0, 0), &only);
        assert_eq!(a.len(), 1);
        assert_eq!(a[0]["kind"], FIX_ALL_KIND);
    }

    #[test]
    fn disable_comments_extend_existing_ones() {
        let doc = TextDoc::new(
            "#!/usr/bin/env node\n/* eslint-disable a */\nx;\n  // eslint-disable-next-line a\n  y;\n",
        );
        let e = disable_file_edit(&doc, "b");
        assert_eq!(e["range"]["start"], json!({"line": 1, "character": 19}));
        assert_eq!(e["newText"], ", b");
        let e = disable_line_edit(&doc, 4, "b", &[]);
        assert_eq!(e["range"]["start"], json!({"line": 3, "character": 31}));
        let e = disable_line_edit(&doc, 2, "b", &[]);
        assert_eq!(e["newText"], "// eslint-disable-next-line b\n");
        let crlf = TextDoc::new("x;\r\n");
        assert_eq!(
            disable_file_edit(&crlf, "b")["newText"],
            "/* eslint-disable b */\r\n"
        );
    }

    #[test]
    fn disable_line_inside_jsx_children_uses_a_jsx_comment() {
        let text = "const a = (\n  <div>\n    {console.log('x')}\n    {/* eslint-disable-next-line b */}\n    <i>{f()}</i>\n  </div>\n);\n";
        let l = linted(text, vec![diag("no-console", (25, 41), None)]);
        let a = code_actions(&l, "file:///a.ts", Pos::new(2, 5), Pos::new(2, 5), &[]);
        let line = a
            .iter()
            .find(|x| x["title"] == "Disable no-console for this line")
            .unwrap();
        assert_eq!(
            edit_of(line)["newText"],
            "    {/* eslint-disable-next-line no-console */}\n"
        );
        // An existing JSX disable comment gets the rule added before its ` */}`.
        let jsx = jsx_text_spans(text, SourceKind::Tsx, TsInJs::Reject);
        let e = disable_line_edit(&l.text, 4, "c", &jsx);
        assert_eq!(e["range"]["start"], json!({"line": 3, "character": 34}));
        assert_eq!(e["newText"], ", c");
        // The line opening the element is not in JSX children: a `//` comment is right.
        let e = disable_line_edit(&l.text, 1, "c", &jsx);
        assert_eq!(e["newText"], "  // eslint-disable-next-line c\n");
        // Without JSX in the file, nothing is parsed.
        assert!(jsx_text_spans("a < b;\n", SourceKind::Tsx, TsInJs::Reject).is_empty());
    }

    #[test]
    fn fix_all_of_a_rule_skips_overlapping_fixes() {
        let text = "var a; var b; var c;\n";
        let ds = vec![
            diag("no-var", (0, 6), Some(Fix::replace(Span::new(0, 3), "let"))),
            diag(
                "no-var",
                (7, 13),
                Some(Fix::replace(Span::new(7, 10), "let")),
            ),
            diag("no-var", (8, 13), Some(Fix::replace(Span::new(8, 9), "x"))),
        ];
        let l = linted(text, ds);
        let a = code_actions(&l, "file:///a.ts", Pos::new(0, 0), Pos::new(0, 1), &[]);
        let all = a
            .iter()
            .find(|x| x["title"] == "Fix all no-var problems")
            .unwrap();
        assert_eq!(
            all["edit"]["changes"]["file:///a.ts"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }
}
