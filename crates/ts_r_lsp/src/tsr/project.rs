//! One project (a `tsconfig.json`, or the inferred project for open files no config
//! includes) on its own thread.
//!
//! The thread owns the project's [`ProgramFiles`] and a text overlay of open documents. It
//! runs in *sessions*: a session links a [`Checker`] over the current program and answers
//! requests and checks files with it, until an edit makes the program stale; then the
//! program is updated (only the edited files are re-parsed and re-bound; a change to a
//! file's imports reloads the program) and a new session starts. A `Checker` borrows the
//! program, so this loop is what lets one live checker serve many requests without unsafe
//! code or leaked programs.
//!
//! Diagnostics (design record 0005, editor variant):
//!
//! * After an edit, the syntax diagnostics of each edited open file are published at once,
//!   together with its previous semantic diagnostics moved through the edit (those an edit
//!   touched are dropped), so squiggles do not flicker.
//! * Then the edited files are re-checked and published, then the open files that use
//!   something whose export shape changed (`ts_r_history::shape`: per-file exports, globals,
//!   ambient modules and augmentations from `ts_r_decls`, chased through their dependencies)
//!   — nothing more when the edited file's API hash did not change. Other open files keep
//!   their diagnostics.
//! * Work is done one file at a time; between files the inbox is polled, so a newer edit
//!   cancels the rest of the pass (it is redone on the new program) and requests are
//!   answered first.
//!
//! Levels (design record 0005, `ts_r_history`): before each session the program is ordered
//! lib, dependencies, stable project files, hot files (the recently edited ones and what
//! needs them), and the session's checker is built on the shared lower levels. Lib and
//! dependency levels are shared by every project of the daemon with the same files and
//! options; the stable level survives keystrokes in the same files, so an edit re-checks
//! only the hot files' own work.

mod lint;
mod release;
mod sound;

use super::convert::{ByteEdit, DiagKind, LspLines, diagnostic_to_lsp, remap_span};
use super::features::{self, KeptCk, ensure_ck};
use super::{OverlayFs, Shared};
use crate::jsonrpc::{self, codes};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};
use ts_r_checker::program::bind_options_for;
use ts_r_checker::{Checker, ProgramFiles, SourceFile, load_program};
use ts_r_config::CompilerOptions;
use ts_r_diagnostics::{Diagnostic, FileId};
use ts_r_history::shape::ProgramShapes;
use ts_r_resolver::{Resolver, ResolverOptions};

// Grammar errors the parser reports that tsc reports from the checker (see the driver).
use ts_r_checker::driver::CHECKER_GRAMMAR_CODES;

/// Messages to a project thread.
#[derive(Debug)]
pub enum PMsg {
    /// A document's text changed: `Some(text)` is the open text; `None` means "read it from
    /// disk" (closed, or changed on disk). `edits` are byte edits of the previous text.
    Doc {
        path: String,
        text: Option<Arc<str>>,
        edits: Vec<ByteEdit>,
    },
    /// This project now owns (publishes diagnostics for) the open document, or no longer.
    Own {
        path: String,
        uri: String,
        own: bool,
    },
    /// An LSP request about a document this project owns. With `reply`, the result goes
    /// there instead of to the client (requests several projects answer together:
    /// `workspace/willRenameFiles`; errors become `null`).
    Request {
        id: Value,
        method: String,
        params: Value,
        path: String,
        reply: Option<Sender<Value>>,
    },
    /// New options / root files (config edited): reload the program.
    Config {
        options: Box<CompilerOptions>,
        roots: Vec<String>,
    },
    /// The parity check failed (`true`): type diagnostics come from the user's TypeScript,
    /// stop publishing ours. `false`: ours again.
    Fallback(bool),
    /// Exit the thread.
    Stop,
}

/// What a session ended with.
enum Outcome {
    Rebuild,
    Stop,
}

/// Diagnostics last published for an owned document.
#[derive(Debug, Default)]
struct Published {
    syntax: Vec<Diagnostic>,
    semantic: Vec<Diagnostic>,
    /// Type-aware lint diagnostics (`project/lint.rs`), kept with the semantic ones.
    lint: Vec<ts_r_lint::Diagnostic>,
    /// The semantic diagnostics are from the current text.
    fresh: bool,
}

/// A project thread's state (persists across sessions).
#[derive(Debug)]
pub struct Project {
    /// The coordinator's key (config path, or the inferred project's).
    key: String,
    pub name: String,
    shared: Arc<Shared>,
    options: CompilerOptions,
    roots: Vec<String>,
    /// Open document texts (all open documents of the workspace, not only this project's).
    overlay: HashMap<String, Arc<str>>,
    /// Open documents this project publishes diagnostics for: path → uri.
    owned: BTreeMap<String, String>,
    program: Arc<ProgramFiles>,
    /// Path → index in `program.files`.
    index: HashMap<String, usize>,
    /// Path → importers (paths), from the program's resolutions.
    importers: HashMap<String, Vec<String>>,
    /// API hash per file path, computed on demand.
    api: HashMap<String, [u8; 32]>,
    /// Export shapes of the program's files (built at the first API change, then kept
    /// current).
    shapes: Option<ProgramShapes>,
    /// Paths changed since the program was last updated, with their edits.
    dirty: BTreeMap<String, Vec<ByteEdit>>,
    /// Reload the whole program on the next update.
    reload: bool,
    /// Owned files to (re)check, in order.
    todo: VecDeque<String>,
    published: HashMap<String, Published>,
    /// Messages taken from the channel but not handled yet (in order).
    backlog: VecDeque<PMsg>,
    fallback: bool,
    /// Recently edited files (most recent first): the hot level.
    hot: Vec<String>,
    /// The shared levels the current session builds on (kept between sessions so the next
    /// one finds them in the registry).
    base: Option<Arc<ts_r_checker::Layer>>,
    /// The program's files or the hot set changed since the last [`Project::arrange`].
    relayout: bool,
    /// Counters for `ts-r/status`.
    pub stats: Stats,
    /// Sound mode (`project/sound.rs`).
    sound: sound::SoundState,
    /// Releasing closed files' results (`project/release.rs`).
    release: release::ReleaseState,
    /// Type-aware lint rules (`project/lint.rs`).
    lint: lint::LintState,
}

/// Per-project counters.
#[derive(Debug, Default, Clone)]
pub struct Stats {
    pub loads: u32,
    pub updates: u32,
    pub checks: u64,
    pub cancelled_passes: u32,
    /// Checker panics caught (each drops the session's workers).
    pub panics: u32,
    pub last_load_ms: f64,
    /// Shared levels built / found in the registry.
    pub levels_built: u32,
    pub levels_reused: u32,
}

impl Project {
    /// A project; the program is loaded on the thread.
    pub fn new(
        key: String,
        name: String,
        shared: Arc<Shared>,
        options: CompilerOptions,
        roots: Vec<String>,
    ) -> Project {
        Project {
            key,
            name,
            shared,
            program: Arc::new(empty_program(&options)),
            options,
            roots,
            overlay: HashMap::new(),
            owned: BTreeMap::new(),
            index: HashMap::new(),
            importers: HashMap::new(),
            api: HashMap::new(),
            shapes: None,
            dirty: BTreeMap::new(),
            reload: true,
            todo: VecDeque::new(),
            published: HashMap::new(),
            backlog: VecDeque::new(),
            fallback: false,
            hot: Vec::new(),
            base: None,
            relayout: true,
            stats: Stats::default(),
            sound: sound::SoundState::default(),
            release: release::ReleaseState::default(),
            lint: lint::LintState::default(),
        }
    }

    /// Orders the program by level and finds or builds its shared lower levels (see the
    /// module docs). File ids change with the order: the index and the published
    /// diagnostics are remapped.
    fn arrange(&mut self) {
        if !std::mem::take(&mut self.relayout) {
            return;
        }
        let hot: Vec<usize> = self
            .hot
            .iter()
            .filter_map(|p| self.index.get(p).copied())
            .collect();
        let Some(program) = Arc::get_mut(&mut self.program) else {
            self.base = None;
            return;
        };
        let t = Instant::now();
        let arranged = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            ts_r_history::arrange(program, &hot)
        })) {
            Ok(a) => a,
            Err(e) => {
                // A checker bug while building a level: check the program as one level. The
                // files may have been reordered already, so re-index them and re-check
                // every open file.
                self.stats.panics += 1;
                crate::daemon::log(&format!(
                    "ts-r {}: checker panic while arranging levels: {}",
                    self.name,
                    panic_text(&*e)
                ));
                self.base = None;
                self.index = self
                    .program
                    .files
                    .iter()
                    .enumerate()
                    .map(|(i, f)| (f.path.clone(), i))
                    .collect();
                self.todo = self.owned.keys().cloned().collect();
                return;
            }
        };
        if !arranged.same_order() {
            let perm = &arranged.perm;
            let map = |f: FileId| match perm.get(f.0 as usize) {
                Some(&n) => FileId(n),
                None => f,
            };
            for pb in self.published.values_mut() {
                for d in pb.semantic.iter_mut().chain(pb.syntax.iter_mut()) {
                    d.file = map(d.file);
                    for r in d.related.iter_mut() {
                        r.file = map(r.file);
                    }
                }
            }
            self.index = self
                .program
                .files
                .iter()
                .enumerate()
                .map(|(i, f)| (f.path.clone(), i))
                .collect();
        }
        self.stats.levels_built += arranged.built;
        self.stats.levels_reused += arranged.reused;
        self.base = arranged.parent;
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        if ms > 20.0 {
            crate::daemon::log(&format!(
                "ts-r {}: arranged levels {:?} in {ms:.1} ms (built {}, reused {})",
                self.name, arranged.ends, arranged.built, arranged.reused
            ));
        }
    }

    /// Records edited files as hot (the three most recent distinct ones).
    fn note_hot<'s>(&mut self, paths: impl IntoIterator<Item = &'s String>) {
        for p in paths {
            if self.hot.first() != Some(p) {
                self.hot.retain(|h| h != p);
                self.hot.insert(0, p.clone());
                self.relayout = true;
            }
        }
        self.hot.truncate(3);
    }

    /// The thread body.
    pub fn run(mut self, rx: Receiver<PMsg>) {
        loop {
            if !self.release(&rx) {
                return;
            }
            self.update_program();
            self.arrange();
            let program = self.program.clone();
            let outcome = match self.build_checker(&program) {
                Ok(checker) => {
                    let mut cks: Vec<KeptCk<'_, '_>> = Vec::new();
                    self.session(&program, &checker, &mut cks, &rx)
                }
                Err(msg) => self.without_checker(&msg, &rx),
            };
            drop(program);
            if matches!(outcome, Outcome::Stop) {
                return;
            }
        }
    }

    /// The session's checker: layered on the shared levels, else one level. A checker panic
    /// while linking (a bug) falls back to one level; if that panics too, the error text.
    fn build_checker<'p>(&mut self, program: &'p ProgramFiles) -> Result<Checker<'p>, String> {
        let base = self.base.clone();
        if let Some(base) = base {
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                Checker::layered(program, Some(&base)).ok()
            })) {
                Ok(Some(c)) => return Ok(c),
                Ok(None) => {}
                Err(e) => {
                    self.stats.panics += 1;
                    crate::daemon::log(&format!(
                        "ts-r {}: checker panic while linking levels: {}",
                        self.name,
                        panic_text(&*e)
                    ));
                }
            }
        }
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| Checker::new(program))).map_err(
            |e| {
                self.stats.panics += 1;
                let msg = panic_text(&*e);
                crate::daemon::log(&format!(
                    "ts-r {}: checker panic while linking the program: {msg}",
                    self.name
                ));
                msg
            },
        )
    }

    /// No checker could be built (a checker bug): every open file shows one internal-error
    /// diagnostic, requests fail, and the next change of a document or of the configuration
    /// tries again.
    fn without_checker(&mut self, msg: &str, rx: &Receiver<PMsg>) -> Outcome {
        let error = |uri: &str| {
            jsonrpc::notification(
                "textDocument/publishDiagnostics",
                json!({"uri": uri, "diagnostics": [{
                    "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}},
                    "severity": 1,
                    "source": "ts-r",
                    "message": format!("ts-r internal error: the type checker failed on this project ({msg}). Diagnostics resume after the next change."),
                }]}),
            )
        };
        for uri in self.owned.values() {
            self.shared.send(error(uri));
        }
        loop {
            match self.next(rx, None) {
                None | Some(PMsg::Stop) => return Outcome::Stop,
                Some(PMsg::Doc { path, text, edits }) => {
                    if self.on_doc(path, text, edits) {
                        self.absorb_edits(rx);
                        return Outcome::Rebuild;
                    }
                }
                Some(PMsg::Own { path, uri, own }) => {
                    if self.on_own(path, uri.clone(), own) {
                        return Outcome::Rebuild;
                    }
                    if own {
                        self.shared.send(error(&uri));
                    } else {
                        self.shared.send(jsonrpc::notification(
                            "textDocument/publishDiagnostics",
                            json!({"uri": uri, "diagnostics": []}),
                        ));
                    }
                }
                Some(PMsg::Config { options, roots }) => {
                    self.options = *options;
                    self.roots = roots;
                    self.reload = true;
                    self.sound.invalidate();
                    self.lint_config_changed();
                    return Outcome::Rebuild;
                }
                Some(PMsg::Fallback(on)) => self.fallback = on,
                Some(PMsg::Request { id, reply, .. }) => match reply {
                    Some(reply) => {
                        let _ = reply.send(Value::Null);
                    }
                    None => self.shared.send(jsonrpc::error_response(
                        id,
                        codes::INTERNAL_ERROR,
                        "the ts-r checker failed on this project",
                    )),
                },
            }
        }
    }

    // ------------------------------------------------------------------------------------
    // Messages
    // ------------------------------------------------------------------------------------

    /// Next message: the backlog first, then the channel. Blocks for at most `wait`.
    fn next(&mut self, rx: &Receiver<PMsg>, wait: Option<Duration>) -> Option<PMsg> {
        if let Some(m) = self.backlog.pop_front() {
            return Some(m);
        }
        match wait {
            None => rx.recv().ok().or(Some(PMsg::Stop)),
            Some(d) if d.is_zero() => rx.try_recv().ok(),
            Some(d) => match rx.recv_timeout(d) {
                Ok(m) => Some(m),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => Some(PMsg::Stop),
            },
        }
    }

    /// Records a document change; returns whether the program must be updated.
    fn on_doc(&mut self, path: String, text: Option<Arc<str>>, edits: Vec<ByteEdit>) -> bool {
        match text {
            Some(t) => {
                if self.overlay.get(&path).is_some_and(|o| **o == *t) {
                    return false;
                }
                self.overlay.insert(path.clone(), t);
            }
            None => {
                self.overlay.remove(&path);
                if crate::lint::is_config_file(&path) {
                    self.lint_config_changed();
                }
            }
        }
        let relevant = self.index.contains_key(&path)
            || self.owned.contains_key(&path)
            || self.roots.contains(&path);
        if !relevant {
            return false;
        }
        self.dirty.entry(path).or_default().extend(edits);
        true
    }

    /// Whether an open file it owns may be a root of its program (not a `node_modules`
    /// file of a configured project).
    fn may_root(&self, path: &str) -> bool {
        self.key == super::INFERRED || !path.contains("/node_modules/")
    }

    /// An open package file it owns that its program does not read: the inferred
    /// project's (tsc gives a file to a configured project only when its program has it).
    fn foreign(&self, path: &str) -> bool {
        !self.may_root(path) && self.owned.contains_key(path) && !self.index.contains_key(path)
    }

    fn on_own(&mut self, path: String, uri: String, own: bool) -> bool {
        if own {
            let fresh = !self.owned.contains_key(&path);
            self.owned.insert(path.clone(), uri);
            if !self.index.contains_key(&path) {
                // Not part of the program (yet): an inferred project's new root, or a file
                // the config does not list. Reload with it. A package file a config owns is
                // no root: it is in the program only when imported.
                if !self.roots.contains(&path) && self.may_root(&path) {
                    self.roots.push(path.clone());
                }
                self.dirty.entry(path).or_default();
                self.reload = true;
                return true;
            }
            if fresh {
                self.todo.retain(|p| p != &path);
                self.todo.push_front(path.clone());
                self.publish_syntax(&path, &[]);
            }
            false
        } else {
            self.owned.remove(&path);
            self.published.remove(&path);
            self.todo.retain(|p| p != &path);
            self.release.note_close();
            false
        }
    }

    // ------------------------------------------------------------------------------------
    // Program updates
    // ------------------------------------------------------------------------------------

    fn update_program(&mut self) {
        if self.reload {
            self.load_all();
            return;
        }
        if self.dirty.is_empty() {
            return;
        }
        let t = Instant::now();
        let dirty = std::mem::take(&mut self.dirty);
        let mut edited: Vec<String> = Vec::new();
        let mut api_changed: Vec<String> = Vec::new();
        let mut replaced: Vec<(usize, SourceFile)> = Vec::new();
        if Arc::get_mut(&mut self.program).is_none() {
            // A session still holds the program (cannot happen): reload instead.
            self.reload = true;
        }
        let fs = OverlayFs::new(&self.overlay);
        let resolver = Resolver::new(&fs, ResolverOptions::from_compiler_options(&self.options));
        for path in dirty.keys() {
            if self.reload {
                break;
            }
            let Some(&i) = self.index.get(path) else {
                if self.owned.contains_key(path) || self.roots.contains(path) {
                    self.reload = true;
                }
                continue;
            };
            let old = self.program.files[i].clone();
            let Some(text) = ts_r_config::FileSystem::read_file(&fs, path) else {
                // Deleted on disk.
                self.reload = true;
                continue;
            };
            if text == old.text {
                continue;
            }
            edited.push(path.clone());
            let opts = bind_options_for(path, &self.options, Some(&resolver));
            let mut f = SourceFile::new(path.clone(), text, false, &opts);
            f.is_external = old.is_external;
            if requests(&f) != requests(&old) {
                self.reload = true;
                continue;
            }
            let old_api = match self.api.get(path) {
                Some(h) => *h,
                None => ts_r_decls::index(&old.ast, &old.bound).api_hash,
            };
            let new_api = ts_r_decls::index(&f.ast, &f.bound).api_hash;
            if old_api != new_api {
                api_changed.push(path.clone());
            }
            self.api.insert(path.clone(), new_api);
            replaced.push((i, f));
        }
        self.note_hot(edited.iter());
        if self.reload {
            // Imports changed (or a file appeared/disappeared): reload, re-check every
            // owned file whose text changed and their dependents.
            for (p, e) in dirty {
                self.dirty.insert(p, e);
            }
            self.load_all();
            return;
        }
        // The shapes before the edit are needed to see what changed.
        if !api_changed.is_empty() && self.shapes.is_none() {
            self.shapes = Some(ProgramShapes::build(&self.program, 2));
        }
        if !replaced.is_empty()
            && let Some(p) = Arc::get_mut(&mut self.program)
        {
            for (i, f) in replaced {
                p.files[i] = Arc::new(f);
            }
            self.relayout = true;
        }
        let owned: Vec<String> = self.owned.keys().cloned().collect();
        let dependents = match self.shapes.as_mut() {
            Some(s) if !edited.is_empty() => s.update(&self.program, &edited, &owned),
            _ => Vec::new(),
        };
        self.stats.updates += 1;
        let changed_owned: Vec<String> = dirty
            .keys()
            .filter(|p| self.owned.contains_key(*p))
            .cloned()
            .collect();
        for p in &changed_owned {
            self.publish_syntax(p, &dirty[p]);
        }
        self.schedule(&changed_owned, &dependents);
        crate::daemon::log(&format!(
            "ts-r {}: updated {} file(s) in {:.1} ms (API changed: {}, dependents: {})",
            self.name,
            dirty.len(),
            t.elapsed().as_secs_f64() * 1000.0,
            api_changed.len(),
            dependents.len()
        ));
    }

    /// Owned files that import any of `from` (transitively).
    fn importer_closure(&self, from: &[String]) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut stack: Vec<String> = from.to_vec();
        let mut seen: HashSet<String> = stack.iter().cloned().collect();
        while let Some(p) = stack.pop() {
            for imp in self.importers.get(&p).into_iter().flatten() {
                if seen.insert(imp.clone()) {
                    stack.push(imp.clone());
                    if self.owned.contains_key(imp) {
                        out.push(imp.clone());
                    }
                }
            }
        }
        out
    }

    /// Orders the check queue: changed owned files first, then `dependents` (owned files
    /// whose diagnostics the change can affect).
    fn schedule(&mut self, changed_owned: &[String], dependents: &[String]) {
        let mut first: Vec<String> = changed_owned.to_vec();
        let mut deps: BTreeSet<String> = dependents.iter().cloned().collect();
        for p in changed_owned {
            deps.remove(p);
            if let Some(pb) = self.published.get_mut(p) {
                pb.fresh = false;
            }
        }
        for d in &deps {
            if let Some(pb) = self.published.get_mut(d) {
                pb.fresh = false;
            }
        }
        // Keep earlier unfinished work, after the new.
        let rest: Vec<String> = self
            .todo
            .drain(..)
            .filter(|p| !first.contains(p) && !deps.contains(p))
            .collect();
        first.extend(deps);
        first.extend(rest);
        self.todo = first.into();
    }

    fn load_all(&mut self) {
        let t = Instant::now();
        let fs = OverlayFs::new(&self.overlay);
        let mut roots = self.roots.clone();
        for p in self.owned.keys() {
            if !roots.contains(p) && self.may_root(p) {
                roots.push(p.clone());
            }
        }
        self.program = Arc::new(load_program(
            &fs,
            &roots,
            &self.options,
            Some(&self.shared.libs),
            2,
        ));
        self.reload = false;
        self.relayout = true;
        self.index = self
            .program
            .files
            .iter()
            .enumerate()
            .map(|(i, f)| (f.path.clone(), i))
            .collect();
        // The edges from before the reload still matter for this round: an importer of a
        // deleted (or no longer resolved) file has no edge to it in the new program.
        let old_importers = std::mem::take(&mut self.importers);
        let mut pairs: Vec<(&String, &String)> = self
            .program
            .resolutions
            .iter()
            .filter_map(|((from, _), r)| {
                r.as_ref()
                    .filter(|r| r.included)
                    .map(|r| (&r.resolved, from))
            })
            .collect();
        pairs.sort();
        pairs.dedup();
        for (to, from) in pairs {
            self.importers
                .entry(to.clone())
                .or_default()
                .push(from.clone());
        }
        // `/// <reference path>` dependents are not in `resolutions`; a change there is
        // caught by global scripts re-checking everything.
        self.api.clear();
        // What the coordinator watches on disk for this project.
        let inputs: Vec<String> = self
            .program
            .files
            .iter()
            .filter(|f| !f.is_lib && !super::watch::ignored(&f.path))
            .map(|f| f.path.clone())
            .collect();
        self.shared.report_inputs(&self.key, inputs);
        for p in self.owned.keys().filter(|p| self.foreign(p)) {
            self.shared.disown(p, None);
        }
        self.stats.loads += 1;
        self.stats.last_load_ms = t.elapsed().as_secs_f64() * 1000.0;
        let dirty = std::mem::take(&mut self.dirty);
        let owned: Vec<String> = self.owned.keys().cloned().collect();
        let first_load = self.stats.loads == 1;
        if !first_load {
            // Edited files (opening a document is not an edit).
            let edited: Vec<String> = dirty
                .iter()
                .filter(|(_, e)| !e.is_empty())
                .map(|(p, _)| p.clone())
                .collect();
            self.note_hot(edited.iter());
        }
        let changed: Vec<String> = if first_load {
            owned.clone()
        } else {
            owned
                .iter()
                .filter(|p| dirty.contains_key(*p))
                .cloned()
                .collect()
        };
        for p in &changed {
            self.publish_syntax(p, dirty.get(p).map(Vec::as_slice).unwrap_or(&[]));
        }
        // Imports changed: dependents may see different exports. With shapes from before
        // the reload, only files whose used shapes changed; otherwise every importer, through
        // old or new edges.
        let dependents = match self.shapes.take() {
            Some(old) if !first_load => {
                let new = ProgramShapes::build(&self.program, 2);
                let d = old.affected_by(&new, &owned);
                self.shapes = Some(new);
                d
            }
            _ => {
                let from: Vec<String> = dirty.keys().cloned().collect();
                let mut both = self.importers.clone();
                for (to, from) in old_importers {
                    let e = both.entry(to).or_default();
                    e.extend(from);
                    e.sort();
                    e.dedup();
                }
                let new_importers = std::mem::replace(&mut self.importers, both);
                let d = self.importer_closure(&from);
                self.importers = new_importers;
                d
            }
        };
        self.schedule(&changed, &dependents);
        if first_load {
            self.todo = owned.into();
        }
        crate::daemon::log(&format!(
            "ts-r {}: loaded {} files in {:.1} ms",
            self.name,
            self.program.files.len(),
            self.stats.last_load_ms
        ));
    }

    // ------------------------------------------------------------------------------------
    // Sessions
    // ------------------------------------------------------------------------------------

    fn session<'c, 'p>(
        &mut self,
        program: &'p ProgramFiles,
        checker: &'c Checker<'p>,
        cks: &mut Vec<KeptCk<'c, 'p>>,
        rx: &Receiver<PMsg>,
    ) -> Outcome {
        loop {
            // Requests and edits first; background checking only when the inbox is empty.
            let busy = !self.todo.is_empty() && !self.fallback;
            let wait = if busy {
                Some(Duration::ZERO)
            } else {
                self.release.wait()
            };
            match self.next(rx, wait) {
                Some(PMsg::Stop) => return Outcome::Stop,
                Some(PMsg::Doc { path, text, edits }) => {
                    if self.on_doc(path, text, edits) {
                        // Take every queued edit before rebuilding (coalesce keystrokes).
                        let before = self.todo.len();
                        self.absorb_edits(rx);
                        if before > 0 {
                            self.stats.cancelled_passes += 1;
                        }
                        return Outcome::Rebuild;
                    }
                }
                Some(PMsg::Own { path, uri, own }) => {
                    if self.on_own(path, uri.clone(), own) {
                        return Outcome::Rebuild;
                    }
                    if !own {
                        self.shared.send(jsonrpc::notification(
                            "textDocument/publishDiagnostics",
                            json!({"uri": uri, "diagnostics": []}),
                        ));
                    }
                }
                Some(PMsg::Config { options, roots }) => {
                    self.options = *options;
                    self.roots = roots;
                    self.reload = true;
                    self.sound.invalidate();
                    self.lint_config_changed();
                    return Outcome::Rebuild;
                }
                Some(PMsg::Fallback(on)) => {
                    self.fallback = on;
                    if !on {
                        let owned: Vec<String> = self.owned.keys().cloned().collect();
                        self.todo = owned.into();
                    }
                }
                Some(PMsg::Request {
                    id,
                    method,
                    params,
                    path,
                    reply,
                }) => {
                    if let Some(reply) = reply {
                        let _ = reply.send(self.answer_part(program, checker, &method, &params));
                        continue;
                    }
                    if self.foreign(&path) {
                        let request =
                            json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
                        self.shared.disown(&path, Some(request));
                        continue;
                    }
                    self.answer(program, checker, cks, id, &method, &params, &path);
                }
                None => {
                    // Inbox empty: one step of background checking.
                    if let Some(path) = self.todo.pop_front() {
                        self.check_and_publish(program, checker, cks, &path);
                    } else if self.release.due() {
                        return Outcome::Rebuild;
                    }
                }
            }
        }
    }

    /// Moves queued document changes into the overlay (keeping other messages in order).
    fn absorb_edits(&mut self, rx: &Receiver<PMsg>) {
        while let Ok(m) = rx.try_recv() {
            match m {
                PMsg::Doc { path, text, edits } => {
                    self.on_doc(path, text, edits);
                }
                other => {
                    self.backlog.push_back(other);
                    // Later edits must not overtake this message.
                    break;
                }
            }
        }
    }

    fn file_id(&self, program: &ProgramFiles, path: &str) -> Option<FileId> {
        let i = *self.index.get(path)?;
        (i < program.files.len() && program.files[i].path == path).then_some(FileId(i as u32))
    }

    /// Runs `f` with the session's workers, turning a checker panic into `None` (the workers
    /// are dropped: their caches may be half-written). The checker must never panic on user
    /// input; this keeps the project thread alive when it does.
    fn guarded<'c, 'p, R>(
        &mut self,
        cks: &mut Vec<KeptCk<'c, 'p>>,
        path: &str,
        f: impl FnOnce(&mut Vec<KeptCk<'c, 'p>>) -> R,
    ) -> Option<R> {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(cks))) {
            Ok(r) => Some(r),
            Err(e) => {
                cks.clear();
                self.stats.panics += 1;
                let msg = panic_text(&*e);
                crate::daemon::log(&format!(
                    "ts-r {}: checker panic on {path}: {msg}",
                    self.name
                ));
                None
            }
        }
    }

    /// Semantic diagnostics of `file` (the checker's, plus link diagnostics), sorted.
    fn semantic(
        program: &ProgramFiles,
        checker: &Checker<'_>,
        file: FileId,
        from_ck: &[Diagnostic],
    ) -> Vec<Diagnostic> {
        let f = &program.files[file.0 as usize];
        if f.is_lib
            || (f.is_declaration() && program.options.skip_lib_check() == Some(true))
            || (f.is_js() && program.options.check_js() != Some(true))
        {
            return Vec::new();
        }
        let mut out = from_ck.to_vec();
        out.extend(
            checker
                .link
                .diagnostics
                .iter()
                .filter(|d| d.file == file)
                .cloned(),
        );
        sort_diags(&mut out);
        out
    }

    fn check_and_publish<'c, 'p>(
        &mut self,
        program: &'p ProgramFiles,
        checker: &'c Checker<'p>,
        cks: &mut Vec<KeptCk<'c, 'p>>,
        path: &str,
    ) {
        if !self.owned.contains_key(path) || self.fallback {
            return;
        }
        let Some(file) = self.file_id(program, path) else {
            return;
        };
        let t = Instant::now();
        let Some(k) = self.guarded(cks, path, |cks| ensure_ck(checker, cks, file)) else {
            return;
        };
        self.stats.checks += 1;
        let mut semantic = Self::semantic(program, checker, file, &cks[k].diags);
        let sound = self.sound_check(program, file, &semantic);
        semantic.extend(sound);
        // Lint configs load once (an ESLint config through Node): publish the type
        // diagnostics first when that is still to come, with the previous lint results.
        let lint_ready = self.lint_config_ready(path);
        let lint = if lint_ready {
            Some(self.typed_lint(program, cks, k, file, path))
        } else {
            None
        };
        let f = &program.files[file.0 as usize];
        let mut syntax = f.syntax.clone();
        sort_diags(&mut syntax);
        let pb = self.published.entry(path.to_string()).or_default();
        pb.syntax = syntax;
        pb.semantic = semantic;
        if let Some(lint) = lint {
            pb.lint = lint;
        }
        pb.fresh = true;
        self.publish(path);
        if !lint_ready {
            let lint = self.typed_lint(program, cks, k, file, path);
            if let Some(pb) = self.published.get_mut(path)
                && pb.lint != lint
            {
                pb.lint = lint;
                self.publish(path);
            }
        }
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        if ms > 50.0 {
            crate::daemon::log(&format!("ts-r {}: checked {path} in {ms:.1} ms", self.name));
        }
    }

    /// Publishes the syntax diagnostics of an edited owned file right after parsing, with
    /// its previous semantic diagnostics moved through the edits.
    fn publish_syntax(&mut self, path: &str, edits: &[ByteEdit]) {
        if self.fallback {
            return;
        }
        let Some(&i) = self.index.get(path) else {
            return;
        };
        let Some(f) = self.program.files.get(i).cloned() else {
            return;
        };
        let mut syntax = f.syntax.clone();
        sort_diags(&mut syntax);
        let pb = self.published.entry(path.to_string()).or_default();
        let had = !pb.syntax.is_empty() || !pb.semantic.is_empty() || !pb.lint.is_empty();
        pb.syntax = syntax;
        let len = f.text.len() as u32;
        pb.semantic
            .retain_mut(|d| match remap_span(d.span.start, d.span.end, edits) {
                Some((s, e)) if e <= len => {
                    d.span = ts_r_span::Span::new(s, e);
                    true
                }
                _ => false,
            });
        pb.lint
            .retain_mut(|d| match remap_span(d.span.start, d.span.end, edits) {
                Some((s, e)) if e <= len => {
                    d.span = ts_r_span::Span::new(s, e);
                    true
                }
                _ => false,
            });
        pb.fresh = false;
        if had || !pb.syntax.is_empty() {
            self.publish(path);
        }
    }

    fn publish(&self, path: &str) {
        let program = &self.program;
        let (Some(uri), Some(pb), Some(&i)) = (
            self.owned.get(path),
            self.published.get(path),
            self.index.get(path),
        ) else {
            return;
        };
        let Some(f) = program.files.get(i) else {
            return;
        };
        let lines = LspLines::new(&f.text);
        let related = |r: &ts_r_diagnostics::RelatedInfo| -> Option<Value> {
            let rf = program.files.get(r.file.0 as usize)?;
            let rl = if rf.path == f.path {
                lines.clone()
            } else {
                LspLines::new(&rf.text)
            };
            Some(json!({
                "location": {"uri": crate::uri::path_to_uri(&rf.path), "range": rl.range(&rf.text, r.span.start, r.span.end)},
                "message": r.message,
            }))
        };
        let mut diags: Vec<Value> = pb
            .syntax
            .iter()
            .map(|d| diagnostic_to_lsp(d, &f.text, &lines, DiagKind::Syntax, &related))
            .collect();
        diags.extend(
            pb.semantic
                .iter()
                .map(|d| diagnostic_to_lsp(d, &f.text, &lines, DiagKind::Semantic, &related)),
        );
        self.sound_decorate(&mut diags);
        diags.extend(Self::lint_lsp(&pb.lint, &f.text, &lines));
        self.shared.send(jsonrpc::notification(
            "textDocument/publishDiagnostics",
            json!({"uri": uri, "diagnostics": diags}),
        ));
    }

    // ------------------------------------------------------------------------------------
    // Requests
    // ------------------------------------------------------------------------------------

    /// This project's part of a request several projects answer (the coordinator merges
    /// the parts): `workspace/willRenameFiles`.
    fn answer_part(
        &self,
        program: &ProgramFiles,
        checker: &Checker<'_>,
        method: &str,
        params: &Value,
    ) -> Value {
        if method != "workspace/willRenameFiles" {
            return Value::Null;
        }
        let renames = super::rename_pairs(params);
        let fs = OverlayFs::new(&self.overlay);
        features::actions::rename::will_rename_files(program, checker, &fs, &renames)
    }

    #[allow(clippy::too_many_arguments)]
    fn answer<'c, 'p>(
        &mut self,
        program: &'p ProgramFiles,
        checker: &'c Checker<'p>,
        cks: &mut Vec<KeptCk<'c, 'p>>,
        id: Value,
        method: &str,
        params: &Value,
        path: &str,
    ) {
        if self.shared.take_cancelled(&id) {
            self.shared.send(jsonrpc::error_response(
                id,
                codes::REQUEST_CANCELLED,
                "cancelled",
            ));
            return;
        }
        if method == "workspace/willRenameFiles" {
            let edit = self.answer_part(program, checker, method, params);
            self.shared.send(jsonrpc::response(id, edit));
            return;
        }
        let Some(file) = self.file_id(program, path) else {
            self.shared.send(jsonrpc::response(id, Value::Null));
            return;
        };
        if method == "ts-r/fileDiagnostics" {
            // Pull diagnostics (the tsserver bridge): syntax and semantic separately.
            let Some(k) = self.guarded(cks, path, |cks| ensure_ck(checker, cks, file)) else {
                self.shared.send(jsonrpc::error_response(
                    id,
                    codes::INTERNAL_ERROR,
                    "the ts-r checker failed on this file",
                ));
                return;
            };
            let semantic = Self::semantic(program, checker, file, &cks[k].diags);
            let f = &program.files[file.0 as usize];
            let lines = LspLines::new(&f.text);
            let no_rel = |_: &ts_r_diagnostics::RelatedInfo| None;
            let mut syntax = f.syntax.clone();
            sort_diags(&mut syntax);
            let s: Vec<Value> = syntax
                .iter()
                .map(|d| diagnostic_to_lsp(d, &f.text, &lines, DiagKind::Syntax, &no_rel))
                .collect();
            let m: Vec<Value> = if self.fallback {
                Vec::new()
            } else {
                semantic
                    .iter()
                    .map(|d| diagnostic_to_lsp(d, &f.text, &lines, DiagKind::Semantic, &no_rel))
                    .collect()
            };
            self.shared.send(jsonrpc::response(
                id,
                json!({"syntax": s, "semantic": m, "fallback": self.fallback}),
            ));
            return;
        }
        let sound = self.sound_request(program, file, method, params);
        if method != "textDocument/codeAction"
            && let Some(v) = sound
        {
            self.shared.send(jsonrpc::response(id, v));
            return;
        }
        let t = Instant::now();
        let cx = features::Ctx {
            program,
            checker,
            file,
        };
        let mut result = self
            .guarded(cks, path, |cks| features::handle(&cx, cks, method, params))
            .unwrap_or_else(|| {
                Err((
                    codes::INTERNAL_ERROR,
                    format!("the ts-r checker failed answering {method}"),
                ))
            });
        if method == "textDocument/codeAction" {
            // Our fixes and refactors (none if the checker failed), then sound mode's quick
            // fixes, then the type-aware lint rules'.
            let mut all = match result {
                Ok(Value::Array(a)) => a,
                _ => Vec::new(),
            };
            if let Some(Value::Array(s)) = sound {
                all.extend(s);
            }
            all.extend(self.lint_code_actions(program, file, path, params));
            result = Ok(Value::Array(all));
        }
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        if ms > 100.0 {
            crate::daemon::log(&format!("ts-r {}: {method} took {ms:.1} ms", self.name));
        }
        match result {
            Ok(v) => self.shared.send(jsonrpc::response(id, v)),
            Err((code, msg)) => self.shared.send(jsonrpc::error_response(id, code, msg)),
        }
    }
}

fn empty_program(options: &CompilerOptions) -> ProgramFiles {
    ProgramFiles {
        files: Vec::new(),
        resolutions: Default::default(),
        mode_resolutions: Default::default(),
        options: options.clone(),
        roots: Vec::new(),
        missing_libs: Vec::new(),
        missing_types: Vec::new(),
        missing_type_references: Vec::new(),
        ambiguous_roots: Vec::new(),
        deps: Default::default(),
    }
}

/// `(specifier, kind)` of every module request and triple-slash reference: when these
/// change, resolution must run again.
fn requests(f: &SourceFile) -> Vec<(String, u8)> {
    let mut v: Vec<(String, u8)> = f
        .bound
        .module_requests
        .iter()
        .map(|r| (f.ast.atom(r.specifier).to_string(), r.kind as u8))
        .collect();
    v.extend(f.references.iter().map(|(k, s)| (format!("{k}:{s}"), 255)));
    v
}

/// Sorts diagnostics by position, code and message, dropping exact duplicates.
pub fn sort_diags(d: &mut Vec<Diagnostic>) {
    d.sort_by(|a, b| {
        a.span
            .start
            .cmp(&b.span.start)
            .then(a.span.end.cmp(&b.span.end))
            .then(a.code.cmp(&b.code))
            .then(a.message.cmp(&b.message))
    });
    d.dedup_by(|a, b| a.span == b.span && a.code == b.code && a.message == b.message);
}

/// Whether a syntax diagnostic is a real parse error (not a grammar check tsc runs in the
/// checker).
pub fn is_parse_error(d: &Diagnostic) -> bool {
    d.is_error() && !CHECKER_GRAMMAR_CODES.contains(&d.code)
}

/// The text of a caught panic.
fn panic_text(e: &(dyn std::any::Any + Send)) -> String {
    e.downcast_ref::<String>()
        .cloned()
        .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default()
}
