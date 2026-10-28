//! History-based re-checking of whole programs (design record 0005, "Invalidation"): the
//! state a check leaves behind, and the next check that re-checks only what an edit can
//! have changed.
//!
//! [`History::check`] keeps, per non-lib file, its text hash, a hash of how its imports
//! resolved, its [`FileShape`](crate::shape::FileShape), the deep hashes of what it used and
//! its check diagnostics. The next check re-checks a file when it is new, its text or
//! resolutions changed, a deep hash of something it uses changed, or one of its diagnostics
//! points into a file whose text changed (positions). Every other file's diagnostics are
//! reused. Link diagnostics (merges, duplicate declarations) come from a fresh link of the
//! whole program. The output equals `check_program` (the differential test in
//! `tests/differential.rs` holds it to that).

use rustc_hash::{FxHashMap, FxHashSet};
use ts_r_checker::driver::CHECKER_GRAMMAR_CODES;
use ts_r_checker::driver::LinkDiagnosticFilter;
use ts_r_checker::{CheckRun, Checker, ProgramFiles, SourceFile, check_files, should_check};
use ts_r_diagnostics::{Diagnostic, FileId};

use crate::shape::{FileShape, Hash, ProgramShapes, shape_of, shapes_of};

/// A diagnostic with file ids replaced by paths (`None`: no file).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathDiag {
    pub diag: Diagnostic,
    pub file: Option<String>,
    /// Per related info.
    pub related: Vec<Option<String>>,
}

impl PathDiag {
    pub fn from_diag(program: &ProgramFiles, d: &Diagnostic) -> PathDiag {
        let path = |f: FileId| program.files.get(f.0 as usize).map(|f| f.path.clone());
        PathDiag {
            file: path(d.file),
            related: d.related.iter().map(|r| path(r.file)).collect(),
            diag: d.clone(),
        }
    }

    /// The diagnostic with file ids of `program` (`None` when a file is gone).
    pub fn to_diag(&self, index: &FxHashMap<&str, u32>) -> Option<Diagnostic> {
        let id = |p: &Option<String>| match p {
            None => Some(FileId(u32::MAX)),
            Some(p) => index.get(p.as_str()).map(|&i| FileId(i)),
        };
        let mut d = self.diag.clone();
        d.file = id(&self.file)?;
        for (r, p) in d.related.iter_mut().zip(&self.related) {
            r.file = id(p)?;
        }
        Some(d)
    }

    /// Paths this diagnostic points into.
    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.file
            .iter()
            .chain(self.related.iter().flatten())
            .map(String::as_str)
    }
}

/// What the last check knew about one non-lib file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileState {
    pub text: Hash,
    /// How the file's module requests resolved and its other program edges.
    pub resolved: Hash,
    pub shape: FileShape,
    /// Deep hashes of `shape.uses`, in order, when the file was last checked.
    pub used: Vec<Hash>,
    /// Check diagnostics (`None`: not checked, for example a declaration file under
    /// `skipLibCheck`).
    pub diags: Option<Vec<PathDiag>>,
    /// The file's syntax diagnostics (the persisted history replays them without parsing).
    pub syntax: Vec<PathDiag>,
    /// Path-reference diagnostics, refreshed when the file's resolutions change.
    pub references: Vec<PathDiag>,
}

/// Counts from one [`History::check`].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CheckStats {
    /// Non-lib files whose text or resolutions changed (or that are new).
    pub changed: usize,
    /// Files re-checked.
    pub checked: usize,
    /// Files whose diagnostics were reused.
    pub reused: usize,
    /// Wall time (ms) of: hashing and shapes, deep hashes and planning, link and check.
    pub ms: [f64; 3],
    /// Blocking syntax errors: nothing was checked and the history was left as it was.
    pub blocked: bool,
}

/// The history of one program (one tsconfig) between checks.
#[derive(Clone, Debug, Default)]
pub struct History {
    /// Hash of the compiler options and lib files (a change starts over).
    pub key: Hash,
    pub files: FxHashMap<String, FileState>,
    /// Link and missing-global diagnostics of the last check.
    pub link: Option<Vec<PathDiag>>,
    /// How deep hashes write paths (saved history uses relative paths); as they are when
    /// `None`.
    pub path_key: Option<crate::shape::PathKey>,
}

/// blake3 of a file's text.
pub fn text_hash(f: &SourceFile) -> Hash {
    *blake3::hash(f.text.as_bytes()).as_bytes()
}

/// How `f`'s module requests resolved in `program`, and its program edges.
pub fn resolved_hash(program: &ProgramFiles, f: &SourceFile) -> Hash {
    let specs: Vec<_> = crate::module_requests_with_modes(f, &program.options).collect();
    let requests = specs.iter().map(|(spec, _, mode)| {
        let r = crate::request_resolution(program, f, spec, *mode).map(|r| {
            r.as_ref()
                .map(|r| (r.resolved.as_str(), r.included, r.external, r.package))
        });
        (spec.as_ref(), r)
    });
    let deps = program
        .deps
        .get(&f.path)
        .into_iter()
        .flatten()
        .map(String::as_str);
    resolved_hash_of(f.is_external, requests, deps)
}

/// [`resolved_hash`] from its parts: per module request its specifier and resolution
/// (`None`: not looked up; `Some(None)`: not found; else path, included, external, package),
/// then the file's program edges.
pub fn resolved_hash_of<'s>(
    external: bool,
    requests: impl Iterator<Item = (&'s str, Option<Option<(&'s str, bool, bool, bool)>>)>,
    deps: impl Iterator<Item = &'s str>,
) -> Hash {
    let mut h = blake3::Hasher::new();
    h.update(&[external as u8]);
    for (spec, r) in requests {
        put(&mut h, spec);
        match r {
            Some(Some((resolved, included, external, package))) => {
                h.update(&[1, included as u8, external as u8, package as u8]);
                put(&mut h, resolved);
            }
            Some(None) => {
                h.update(&[0]);
            }
            None => {
                h.update(&[2]);
            }
        }
    }
    h.update(b"deps");
    for d in deps {
        put(&mut h, d);
    }
    *h.finalize().as_bytes()
}

fn put(h: &mut blake3::Hasher, s: &str) {
    h.update(&(s.len() as u64).to_le_bytes());
    h.update(s.as_bytes());
}

/// Hash of what makes every file's history invalid when it changes: the compiler options
/// and the lib files.
pub fn program_key(program: &ProgramFiles) -> Hash {
    program_key_of(
        &program.options,
        program.files.iter().filter(|f| f.is_lib).map(|f| &**f),
    )
}

/// [`program_key`] from the options and the lib files (in program order).
pub fn program_key_of<'f>(
    options: &ts_r_config::CompilerOptions,
    libs: impl Iterator<Item = &'f SourceFile>,
) -> Hash {
    let hashes: Vec<(&str, Hash)> = libs
        .map(|f| (f.path.rsplit('/').next().unwrap_or(&f.path), text_hash(f)))
        .collect();
    program_key_from(options, hashes.iter().map(|(n, h)| (*n, h)))
}

/// [`program_key_of`] from the lib files' names and text hashes.
pub fn program_key_from<'a>(
    options: &ts_r_config::CompilerOptions,
    libs: impl Iterator<Item = (&'a str, &'a Hash)>,
) -> Hash {
    let mut h = blake3::Hasher::new();
    h.update(b"ts-r/history/1");
    put(&mut h, &format!("{options:?}"));
    for (name, hash) in libs {
        // The lib file's name: the history survives moving the lib directory.
        put(&mut h, name);
        h.update(hash);
    }
    *h.finalize().as_bytes()
}

/// Syntax errors that stop checking (as `check_program` reports them), if any.
///
/// The parser's checker grammar errors (`SourceFile::grammar`) are not in the list: they
/// never block, and they travel with the file's check diagnostics.
pub fn blocking_syntax(program: &ProgramFiles) -> (Vec<Diagnostic>, bool) {
    let mut syntax: Vec<Diagnostic> = Vec::new();
    for (i, f) in program.files.iter().enumerate() {
        if f.is_lib {
            continue;
        }
        syntax.extend(syntax_only(f).map(|d| d.with_file(FileId(i as u32))));
    }
    let blocking = syntax
        .iter()
        .any(|d| d.is_error() && !CHECKER_GRAMMAR_CODES.contains(&d.code));
    (syntax, blocking)
}

/// A file's syntax diagnostics as tsc reports them: without its checker grammar errors, with
/// the `lenient` ones (octal escapes in strings), as the checker driver has them
/// ([`ts_r_parser::tsc_syntax_errors`]), and with TS1084 for a malformed `/// <reference />`
/// (a parse error in tsc: it blocks checking like the others).
pub fn syntax_only(f: &SourceFile) -> impl Iterator<Item = Diagnostic> {
    ts_r_parser::tsc_syntax_errors(&f.syntax, &f.grammar, &f.lenient)
        .into_iter()
        .chain(ts_r_checker::invalid_reference_diagnostics(f))
}

/// Global diagnostics of a program (unresolved `types` entries, ambiguous project roots).
pub fn global_diagnostics(program: &ProgramFiles) -> Vec<Diagnostic> {
    let mut out: Vec<Diagnostic> = program
        .missing_types
        .iter()
        .map(|name| {
            let mut d = Diagnostic::new(
                ts_r_checker::diag::msg(
                    ts_r_checker::diag::dg::CANNOT_FIND_TYPE_DEFINITION_FILE_FOR_0_2688,
                ),
                ts_r_ast::Span::new(0, 0),
                &[name],
            );
            d.file = FileId(u32::MAX);
            d
        })
        .collect();
    out.extend(ts_r_checker::driver::ambiguous_root_diagnostics(program));
    out
}

impl History {
    /// Checks `program` re-using what the last check found; equals
    /// `check_program(program, run).diagnostics`. `make` builds the checker (flat or layered).
    pub fn check<'a>(
        &mut self,
        program: &'a ProgramFiles,
        run: CheckRun,
        make: impl FnOnce(&'a ProgramFiles) -> Checker<'a>,
    ) -> (Vec<Diagnostic>, CheckStats) {
        let mut stats = CheckStats::default();
        let mut t = std::time::Instant::now();
        let mut lap = |i: usize, stats: &mut CheckStats| {
            stats.ms[i] = t.elapsed().as_secs_f64() * 1000.0;
            t = std::time::Instant::now();
        };
        let (syntax, blocking) = blocking_syntax(program);
        if blocking {
            // Nothing is checked, and only the syntax errors are reported (not the reference
            // diagnostics), as `check_program`; the history stays as it was (it describes
            // texts by hash).
            stats.blocked = true;
            return (ts_r_checker::sort_and_dedup(program, syntax), stats);
        }
        let paths = program.reference_paths();
        let missing_types = program.missing_type_references_by_file();
        let references: Vec<Vec<Diagnostic>> = program
            .files
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let missing = missing_types
                    .get(f.path.as_str())
                    .map_or(&[][..], Vec::as_slice);
                ts_r_checker::reference_diagnostics(f, &program.options, &paths)
                    .into_iter()
                    .chain(ts_r_checker::type_reference_diagnostics(f, missing))
                    .map(|d| d.with_file(FileId(i as u32)))
                    .collect()
            })
            .collect();
        let mut syntax = syntax;
        syntax.extend(references.iter().flatten().cloned());
        let key = program_key(program);
        if key != self.key {
            self.files.clear();
            self.link = None;
            self.key = key;
        }
        // Which files changed.
        let mut changed: FxHashSet<&str> = FxHashSet::default();
        let mut fresh: Vec<&SourceFile> = Vec::new();
        let mut hashes: FxHashMap<&str, (Hash, Hash)> = FxHashMap::default();
        let present: FxHashSet<&str> = program.files.iter().map(|f| f.path.as_str()).collect();
        for f in program.files.iter().filter(|f| !f.is_lib) {
            let t = text_hash(f);
            let r = resolved_hash(program, f);
            hashes.insert(f.path.as_str(), (t, r));
            match self.files.get(&f.path) {
                Some(s) if s.text == t && s.resolved == r => {}
                // Same text, different resolutions: same positions.
                Some(s) if s.text == t => fresh.push(f),
                _ => {
                    changed.insert(f.path.as_str());
                    fresh.push(f);
                }
            }
        }
        let removed: Vec<String> = self
            .files
            .keys()
            .filter(|p| !present.contains(p.as_str()))
            .cloned()
            .collect();
        stats.changed = fresh.len() + removed.len();
        // Shapes: reused for unchanged files.
        let new_shapes: FxHashMap<String, FileShape> = shapes_of(program, &fresh, run.threads)
            .into_iter()
            .collect();
        let mut shapes = ProgramShapes {
            shapes: FxHashMap::default(),
            lib_globals: crate::shape::lib_global_names(program),
            order: program
                .files
                .iter()
                .filter(|f| !f.is_lib)
                .map(|f| f.path.clone())
                .collect(),
            key: self.path_key.clone(),
        };
        for f in program.files.iter().filter(|f| !f.is_lib) {
            let s = match new_shapes.get(&f.path) {
                Some(s) => s.clone(),
                None => match self.files.get_mut(&f.path) {
                    Some(st) => std::mem::take(&mut st.shape),
                    None => shape_of(program, f),
                },
            };
            shapes.shapes.insert(f.path.clone(), s);
        }
        lap(0, &mut stats);
        let mut deep = shapes.deep();
        // Targets.
        let mut targets: Vec<usize> = Vec::new();
        let mut used_now: FxHashMap<usize, Vec<Hash>> = FxHashMap::default();
        for (i, f) in program.files.iter().enumerate() {
            if f.is_lib {
                continue;
            }
            let check = should_check(program, i);
            if !check {
                continue;
            }
            let shape = &shapes.shapes[&f.path];
            let used: Vec<Hash> = shape.uses.iter().map(|u| deep.of(u)).collect();
            let reuse = check
                && !new_shapes.contains_key(&f.path)
                && match self.files.get(&f.path) {
                    Some(st) => {
                        st.used == used
                            && st.diags.as_ref().is_some_and(|ds| {
                                ds.iter().all(|d| {
                                    d.paths().all(|p| {
                                        present.contains(p)
                                            && !changed.contains(p)
                                            && !removed.iter().any(|r| r == p)
                                    })
                                })
                            })
                    }
                    None => false,
                };
            if check && !reuse {
                targets.push(i);
            }
            used_now.insert(i, used);
        }
        drop(deep);
        lap(1, &mut stats);
        stats.checked = targets.len();
        // Check (a link is needed for link diagnostics whenever anything changed).
        let mut out = syntax;
        out.extend(global_diagnostics(program));
        let mut results: FxHashMap<usize, Vec<PathDiag>> = FxHashMap::default();
        let link_changed = !fresh.is_empty() || !removed.is_empty() || self.link.is_none();
        if link_changed || !targets.is_empty() {
            let checker = make(program);
            let globals = ts_r_checker::driver::missing_global_type_diagnostics(&checker);
            out.extend(globals.iter().cloned());
            // As `check_program`: a checked file's, unless `@ts-ignore` or
            // `@ts-expect-error` governs its line.
            let mut filter = LinkDiagnosticFilter::new(program);
            let mut reported = |d: &&Diagnostic| filter.reported(d);
            out.extend(
                checker
                    .link
                    .diagnostics
                    .iter()
                    .filter(&mut reported)
                    .cloned(),
            );
            for (&i, ds) in targets.iter().zip(check_files(&checker, &targets, run)) {
                results.insert(
                    i,
                    ds.iter().map(|d| PathDiag::from_diag(program, d)).collect(),
                );
            }
            self.link = Some(
                checker
                    .link
                    .diagnostics
                    .iter()
                    .filter(&mut reported)
                    .chain(globals.iter())
                    .map(|d| PathDiag::from_diag(program, d))
                    .collect(),
            );
        } else if let Some(link) = &self.link {
            let index = path_index(program);
            out.extend(link.iter().filter_map(|d| d.to_diag(&index)));
        }
        lap(2, &mut stats);
        // New states.
        let mut next: FxHashMap<String, FileState> = FxHashMap::default();
        for (i, f) in program.files.iter().enumerate() {
            if f.is_lib {
                continue;
            }
            let (text, resolved) = hashes[f.path.as_str()];
            let diags = match results.remove(&i) {
                Some(ds) => Some(ds),
                None if should_check(program, i) => {
                    stats.reused += 1;
                    self.files.get_mut(&f.path).and_then(|s| s.diags.take())
                }
                None => None,
            };
            next.insert(
                f.path.clone(),
                FileState {
                    text,
                    resolved,
                    shape: shapes.shapes.remove(&f.path).unwrap_or_default(),
                    used: used_now.remove(&i).unwrap_or_default(),
                    diags,
                    syntax: syntax_only(f)
                        .map(|d| PathDiag::from_diag(program, &d.with_file(FileId(i as u32))))
                        .collect(),
                    references: references[i]
                        .iter()
                        .map(|d| PathDiag::from_diag(program, d))
                        .collect(),
                },
            );
        }
        self.files = next;
        let index = path_index(program);
        for st in self.files.values() {
            for d in st.diags.iter().flatten() {
                if let Some(d) = d.to_diag(&index) {
                    out.push(d);
                }
            }
        }
        (ts_r_checker::sort_and_dedup(program, out), stats)
    }
}

/// Path → file id.
pub fn path_index(program: &ProgramFiles) -> FxHashMap<&str, u32> {
    program
        .files
        .iter()
        .enumerate()
        .map(|(i, f)| (f.path.as_str(), i as u32))
        .collect()
}
