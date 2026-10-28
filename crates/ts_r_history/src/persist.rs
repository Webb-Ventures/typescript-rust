//! The persisted history of `ts-r-check` (design record 0005, "Storage"): one manifest per
//! project in the ts-r cache (namespace [`NAMESPACE`], [`FORMAT_VERSION`]).
//!
//! The manifest holds, with paths relative to the config directory (lib files as
//! `lib:<name>`): the lib files' hashes and the requested ones that were missing, the root
//! files, how `types` entries resolved, per program file its text hash, binding options,
//! module requests with their resolutions and reference targets, the run's output, and the
//! [`History`] states (shapes, used deep hashes, diagnostics). [`check`] first verifies the
//! manifest without parsing anything (`crate::lazy`): same libs (missing ones still
//! missing), roots and `types`, every file's text (skipped when its stat fingerprint, kept
//! in a sidecar entry, still matches), binding options and resolutions. When all match it
//! returns the stored output (a warm run with no change). After an edit it parses only the
//! edited files, plans the re-check from the stored shapes, and parses just what checking
//! the affected files needs. Anything it cannot handle (no manifest, other libs, blocking
//! syntax errors) loads the whole program and checks it through a [`History`].

use ts_r_binder::RequestKind;
use ts_r_cache::{Cache, KeyBuilder};
use ts_r_checker::program::{
    Resolution, bind_options_for, expand_wildcard_types, reference_target,
};
use ts_r_checker::{CheckRun, Checker, LibStore, ProgramFiles, load_program};
use ts_r_config::{CompilerOptions, FileSystem, ParsedConfig, path as cpath};
use ts_r_diagnostics::Diagnostic;
use ts_r_resolver::{Extension, ResolutionMode, Resolver, ResolverOptions};

use crate::codec::{Reader, Writer};
use crate::incremental::{CheckStats, FileState, History, PathDiag, text_hash};
use crate::shape::{Hash, PathKey};

/// Cache namespace of persisted histories.
pub const NAMESPACE: &str = "check";
/// Bump when the manifest layout or anything hashed into it changes.
pub const FORMAT_VERSION: u32 = 13;
const MAGIC: &[u8; 4] = b"TSRH";

/// Where a module request led (paths relative to the config directory).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SavedRes {
    pub path: String,
    pub included: bool,
    pub external: bool,
    /// The file is inside a package with a package id ([`Resolution::package`]).
    pub package: bool,
    pub ext: Extension,
}

/// A module request: specifier, request kind, separate mode, resolution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SavedRequest {
    pub spec: String,
    pub kind: u8,
    pub mode: Option<ResolutionMode>,
    pub res: Option<SavedRes>,
}

/// A `/// <reference path|types>`: kind, value, target.
pub(crate) type SavedReference = (String, String, Option<String>);

/// One program file as the last run saw it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SavedFile {
    pub path: String,
    pub text: Hash,
    pub bind: Hash,
    pub external: bool,
    /// The text mentions `@ts-r` (a sound-mode pragma; see [`has_pragma`]).
    pub pragma: bool,
    /// Per module request, in order, with mode-specific resolutions.
    pub requests: Vec<SavedRequest>,
    pub references: Vec<SavedReference>,
}

#[derive(Debug, Default)]
pub(crate) struct Manifest {
    pub program_key: Hash,
    /// The states describe the saved files (false after a run stopped by syntax errors,
    /// which saves the new files but keeps the old states).
    pub consistent: bool,
    pub libs: Vec<(String, Hash)>,
    /// Requested lib files that were not found (sorted), and whether there was a lib store.
    pub missing_libs: Vec<String>,
    pub lib_store: bool,
    pub roots: Vec<String>,
    pub types: Vec<(String, Option<String>)>,
    /// Non-lib files in program order.
    pub files: Vec<SavedFile>,
    pub output: Vec<PathDiag>,
    /// Sorted by path.
    pub states: Vec<(String, FileState)>,
    pub link: Option<Vec<PathDiag>>,
}

/// What [`check`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// No manifest (or an unusable one): everything checked.
    #[default]
    Cold,
    /// Nothing changed: the stored output, nothing parsed or checked.
    Unchanged,
    /// Something changed: the program was loaded and affected files re-checked.
    Incremental,
}

/// The result of [`check`].
#[derive(Debug, Default)]
pub struct Outcome {
    /// Per file id: `(path, text)` for non-lib files, `None` for lib files. Runs that skip
    /// parsing fill the text only for files a diagnostic points into (empty otherwise).
    pub sources: Vec<Option<(String, String)>>,
    /// Sorted diagnostics with file ids into `sources`.
    pub diagnostics: Vec<Diagnostic>,
    pub mode: Mode,
    pub stats: CheckStats,
    /// Wall time per phase (ms), for `TS_R_CHECK_STATS`.
    pub phases: Vec<(&'static str, f64)>,
    /// Non-lib files in program order with their `node_modules` flag (tests compare it with
    /// `load_program`).
    pub files: Vec<(String, bool)>,
    /// Files parsed by this run (lib files included).
    pub parsed: usize,
    /// Some non-lib file mentions `@ts-r` (known without reading unchanged files).
    pub pragma: bool,
    /// The whole program, when this run loaded it (no history, or history that fell back to
    /// a full load): `diagnostics` and `sources` use its file ids, so a caller that needs the
    /// program too (`ts-r-check`'s TS6059) takes it instead of loading and checking again.
    /// `None` when the history answered from its saved state.
    pub program: Option<ProgramFiles>,
}

/// Whether `text` mentions `@ts-r` (sound-mode pragmas; `ts-r-check` then needs the program).
pub fn has_pragma(text: &str) -> bool {
    text.contains("@ts-r")
}

pub(crate) fn kind_code(k: RequestKind) -> u8 {
    match k {
        RequestKind::Import => 0,
        RequestKind::ExportFrom => 1,
        RequestKind::ImportEquals => 2,
        RequestKind::DynamicImport => 3,
        RequestKind::ImportType => 4,
        RequestKind::Augmentation => 5,
    }
}

pub(crate) fn kind_of(v: u8) -> Option<RequestKind> {
    Some(match v {
        0 => RequestKind::Import,
        1 => RequestKind::ExportFrom,
        2 => RequestKind::ImportEquals,
        3 => RequestKind::DynamicImport,
        4 => RequestKind::ImportType,
        5 => RequestKind::Augmentation,
        _ => return None,
    })
}

const EXTENSIONS: [Extension; 12] = [
    Extension::Ts,
    Extension::Tsx,
    Extension::Dts,
    Extension::Mts,
    Extension::Cts,
    Extension::Dmts,
    Extension::Dcts,
    Extension::Js,
    Extension::Jsx,
    Extension::Mjs,
    Extension::Cjs,
    Extension::Json,
];

fn ext_code(e: Extension) -> u8 {
    EXTENSIONS.iter().position(|x| *x == e).unwrap_or(0) as u8
}

/// Paths relative to the config directory, lib files as `lib:<name>`.
pub(crate) struct Paths<'a> {
    pub base: &'a str,
    pub lib_dir: Option<String>,
}

impl Paths<'_> {
    pub fn key(&self) -> PathKey {
        PathKey {
            base: self.base.to_owned(),
            lib_dir: self.lib_dir.clone(),
        }
    }

    pub fn rel(&self, p: &str) -> String {
        self.key().rel(p)
    }

    pub fn abs(&self, r: &str) -> String {
        if let Some(spec) = r.strip_prefix('?') {
            return spec.to_owned();
        }
        if let Some(name) = r.strip_prefix("lib:") {
            return match &self.lib_dir {
                Some(d) => format!("{d}/{name}"),
                None => r.to_owned(),
            };
        }
        cpath::normalize(&cpath::combine(self.base, r))
    }

    pub fn diag(&self, d: &PathDiag, f: &dyn Fn(&str) -> String) -> PathDiag {
        PathDiag {
            diag: d.diag.clone(),
            file: d.file.as_deref().map(f),
            related: d.related.iter().map(|p| p.as_deref().map(f)).collect(),
        }
    }

    fn state(&self, s: &FileState, f: &dyn Fn(&str) -> String) -> FileState {
        FileState {
            text: s.text,
            resolved: s.resolved,
            shape: s.shape.map_paths(f),
            used: s.used.clone(),
            diags: s
                .diags
                .as_ref()
                .map(|ds| ds.iter().map(|d| self.diag(d, f)).collect()),
            syntax: s.syntax.iter().map(|d| self.diag(d, f)).collect(),
            references: s.references.iter().map(|d| self.diag(d, f)).collect(),
        }
    }

    /// A saved resolution of `r`.
    pub fn res(&self, r: &Resolution) -> SavedRes {
        SavedRes {
            path: self.rel(&r.resolved),
            included: r.included,
            external: r.external,
            package: r.package,
            ext: r.extension,
        }
    }
}

pub(crate) fn lib_dir(libs: Option<&LibStore>) -> Option<String> {
    libs.map(|l| l.dir().to_string_lossy().replace('\\', "/"))
}

/// The cache key of a project's manifest: tool version and build, config path and options.
pub(crate) fn manifest_key(config_path: &str, options: &CompilerOptions) -> ts_r_cache::Key {
    KeyBuilder::new("ts-r/check-history")
        .str(env!("CARGO_PKG_VERSION"))
        .str(&exe_stamp())
        .u64(FORMAT_VERSION as u64)
        .str(config_path)
        .str(&format!("{options:?}"))
        .key()
}

/// Size and modification time of the running executable: a rebuilt checker (same version
/// number, different behaviour) must not reuse diagnostics of the old one. Part of the key
/// only, never of the stored data.
pub(crate) fn exe_stamp() -> String {
    std::env::current_exe()
        .and_then(std::fs::metadata)
        .map(|m| {
            let t = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            format!("{}:{t}", m.len())
        })
        .unwrap_or_default()
}

pub(crate) fn bind_hash<F: FileSystem>(
    path: &str,
    options: &CompilerOptions,
    resolver: &Resolver<&F>,
) -> Hash {
    let o = bind_options_for(path, options, Some(resolver));
    *blake3::hash(format!("{o:?}").as_bytes()).as_bytes()
}

/// The cache key of a manifest's stat sidecar.
pub(crate) fn sidecar_key(manifest: &ts_r_cache::Key) -> ts_r_cache::Key {
    KeyBuilder::new("ts-r/check-history-stat")
        .key_field(manifest)
        .key()
}

/// Checks the project of `config` like `check_program`, through its persisted history in
/// `cache` (`None`: no history, a plain check).
pub fn check<F: FileSystem>(
    fs: &F,
    cache: Option<&Cache>,
    config: &ParsedConfig,
    libs: Option<&LibStore>,
    threads: usize,
) -> Outcome {
    let roots = &config.file_names;
    let options = &config.options;
    let paths = Paths {
        base: &config.config_dir,
        lib_dir: lib_dir(libs),
    };
    let key = manifest_key(&config.config_path, options);
    let mut phases = Phases::default();
    let blob = cache.and_then(|c| c.get(&key));
    let manifest = blob.as_ref().and_then(|b| decode(b));
    let digest = blob.as_ref().map(|b| *blake3::hash(b).as_bytes());
    drop(blob);
    let sidecar = match (cache, &digest) {
        (Some(c), Some(d)) if manifest.is_some() => c
            .get(&sidecar_key(&key))
            .and_then(|b| crate::stat::Sidecar::decode(&b, d)),
        _ => None,
    };
    phases.lap("read");
    // Lazy reconstruction does not track the JavaScript depth of each import route, nor
    // where `libReplacement` finds a lib's `@typescript/lib-*` package (it checks only the
    // bundled libs): those take the full load.
    if let (Some(c), Some(m), Some(digest)) = (cache, &manifest, digest)
        && !(options.effective().allow_js() && options.effective().max_node_module_js_depth() > 0)
        && options.lib_replacement() != Some(true)
    {
        let run = crate::lazy::Run {
            fs,
            cache: c,
            key: &key,
            digest,
            paths: &paths,
            config,
            libs,
            threads,
        };
        if let Some(mut out) = run.check(m, sidecar.unwrap_or_default(), &mut phases) {
            out.phases = phases.done;
            return out;
        }
        phases.lap("fallback");
    }
    let program = load_program(fs, roots, options, libs, threads);
    phases.lap("load");
    // Text for every non-lib file, and for the lib files diagnostics point into (tsc prints
    // a lib file's diagnostics with its path).
    let sources = |program: &ProgramFiles, diags: &[ts_r_diagnostics::Diagnostic]| {
        let mut shown: Vec<bool> = program.files.iter().map(|f| !f.is_lib).collect();
        for d in diags {
            for f in std::iter::once(d.file).chain(d.related.iter().map(|r| r.file)) {
                if let Some(s) = shown.get_mut(f.0 as usize) {
                    *s = true;
                }
            }
        }
        program
            .files
            .iter()
            .zip(shown)
            .map(|(f, shown)| shown.then(|| (f.path.clone(), f.text.clone())))
            .collect()
    };
    let files = program
        .files
        .iter()
        .filter(|f| !f.is_lib)
        .map(|f| (f.path.clone(), f.is_external))
        .collect();
    let parsed = program.files.len();
    let pragma = program
        .files
        .iter()
        .any(|f| !f.is_lib && has_pragma(&f.text));
    let Some(cache) = cache else {
        // No history: a plain check.
        let result = ts_r_checker::check_program(&program, CheckRun { threads });
        phases.lap("check");
        return Outcome {
            sources: sources(&program, &result.diagnostics),
            diagnostics: result.diagnostics,
            mode: Mode::Cold,
            stats: CheckStats::default(),
            phases: phases.done,
            files,
            parsed,
            pragma,
            program: Some(program),
        };
    };
    let mut history = History {
        path_key: Some(paths.key()),
        ..History::default()
    };
    let mode = match manifest {
        Some(m) => {
            history.key = m.program_key;
            let abs = |p: &str| paths.abs(p);
            for (p, s) in &m.states {
                history.files.insert(paths.abs(p), paths.state(s, &abs));
            }
            history.link = m
                .link
                .as_ref()
                .map(|ds| ds.iter().map(|d| paths.diag(d, &abs)).collect());
            Mode::Incremental
        }
        None => Mode::Cold,
    };
    phases.lap("restore");
    let (diagnostics, stats) = history.check(&program, CheckRun { threads }, Checker::new);
    phases.lap("check");
    let mut m = manifest_of(fs, &program, &history, &diagnostics, &paths, roots, options);
    m.consistent = !stats.blocked;
    m.lib_store = libs.is_some();
    // No fingerprints: the next run hashes every file once and records them.
    let _ = cache.put(&key, &encode(&m));
    crate::libinfo::remember(cache, &program, &m.libs);
    phases.lap("save");
    Outcome {
        sources: sources(&program, &diagnostics),
        diagnostics,
        mode,
        stats,
        phases: phases.done,
        files,
        parsed,
        pragma,
        program: Some(program),
    }
}

/// Wall time per phase.
#[derive(Debug)]
pub(crate) struct Phases {
    t: std::time::Instant,
    pub done: Vec<(&'static str, f64)>,
}

impl Default for Phases {
    fn default() -> Phases {
        Phases {
            t: std::time::Instant::now(),
            done: Vec::new(),
        }
    }
}

impl Phases {
    pub fn lap(&mut self, name: &'static str) {
        self.done
            .push((name, self.t.elapsed().as_secs_f64() * 1000.0));
        self.t = std::time::Instant::now();
    }
}

/// How the module requests of `f` (a file of `program`) resolved, as saved.
pub(crate) fn saved_requests(
    program: &ProgramFiles,
    f: &ts_r_checker::SourceFile,
    paths: &Paths<'_>,
) -> Vec<SavedRequest> {
    crate::module_requests_with_modes(f, &program.options)
        .map(|(spec, kind, mode)| {
            let spec = spec.into_owned();
            let res = crate::request_resolution(program, f, &spec, mode)
                .cloned()
                .flatten()
                .map(|r| paths.res(&r));
            SavedRequest {
                spec,
                kind: kind_code(kind),
                mode,
                res,
            }
        })
        .collect()
}

fn manifest_of<F: FileSystem>(
    fs: &F,
    program: &ProgramFiles,
    history: &History,
    output: &[Diagnostic],
    paths: &Paths<'_>,
    roots: &[String],
    options: &CompilerOptions,
) -> Manifest {
    let resolver = Resolver::new(fs, ResolverOptions::from_compiler_options(options));
    let rel = |p: &str| paths.rel(p);
    // In program order (the fast path replays `load_program`'s lib walk against it).
    let libs: Vec<(String, Hash)> = program
        .files
        .iter()
        .filter(|f| f.is_lib)
        .map(|f| {
            let name = f.path.rsplit('/').next().unwrap_or(&f.path).to_owned();
            (name, text_hash(f))
        })
        .collect();
    let mut missing_libs = program.missing_libs.clone();
    missing_libs.sort();
    missing_libs.dedup();
    let files = program
        .files
        .iter()
        .filter(|f| !f.is_lib)
        .map(|f| SavedFile {
            path: rel(&f.path),
            text: text_hash(f),
            bind: bind_hash(&f.path, options, &resolver),
            external: f.is_external,
            pragma: has_pragma(&f.text),
            requests: saved_requests(program, f, paths),
            references: f
                .references
                .iter()
                .filter(|(k, _)| *k != "lib")
                .map(|(k, v)| {
                    let t = reference_target(fs, &resolver, &f.path, k, v).map(|t| rel(&t));
                    ((*k).to_owned(), v.clone(), t)
                })
                .collect(),
        })
        .collect();
    let types = expand_wildcard_types(fs, &resolver, options.effective().types())
        .into_iter()
        .map(|t| {
            let r = resolver
                .resolve_type_reference(&t, None, ResolutionMode::Import)
                .map(|r| rel(&r.resolved_file_name));
            (t, r)
        })
        .collect();
    let mut states: Vec<(String, FileState)> = history
        .files
        .iter()
        .map(|(p, s)| (rel(p), paths.state(s, &rel)))
        .collect();
    states.sort_by(|a, b| a.0.cmp(&b.0));
    Manifest {
        program_key: history.key,
        consistent: true,
        libs,
        missing_libs,
        lib_store: false,
        roots: roots.iter().map(|r| rel(r)).collect(),
        types,
        files,
        output: output
            .iter()
            .map(|d| paths.diag(&PathDiag::from_diag(program, d), &rel))
            .collect(),
        states,
        link: history
            .link
            .as_ref()
            .map(|ds| ds.iter().map(|d| paths.diag(d, &rel)).collect()),
    }
}

pub(crate) fn encode(m: &Manifest) -> Vec<u8> {
    let mut w = Writer::default();
    w.out.extend_from_slice(MAGIC);
    w.u32(FORMAT_VERSION);
    w.hash(&m.program_key);
    w.bool(m.consistent);
    w.count(m.libs.len());
    for (n, h) in &m.libs {
        w.str(n);
        w.hash(h);
    }
    w.count(m.missing_libs.len());
    for n in &m.missing_libs {
        w.str(n);
    }
    w.bool(m.lib_store);
    w.count(m.roots.len());
    for r in &m.roots {
        w.str(r);
    }
    w.count(m.types.len());
    for (t, r) in &m.types {
        w.str(t);
        w.opt_str(r.as_deref());
    }
    w.count(m.files.len());
    for f in &m.files {
        w.str(&f.path);
        w.hash(&f.text);
        w.hash(&f.bind);
        w.bool(f.external);
        w.bool(f.pragma);
        w.count(f.requests.len());
        for r in &f.requests {
            w.str(&r.spec);
            w.u8(r.kind);
            w.u8(match r.mode {
                None => 0,
                Some(ResolutionMode::Import) => 1,
                Some(ResolutionMode::Require) => 2,
            });
            match &r.res {
                Some(s) => {
                    w.u8(1);
                    w.str(&s.path);
                    w.bool(s.included);
                    w.bool(s.external);
                    w.bool(s.package);
                    w.u8(ext_code(s.ext));
                }
                None => w.u8(0),
            }
        }
        w.count(f.references.len());
        for (k, v, t) in &f.references {
            w.str(k);
            w.str(v);
            w.opt_str(t.as_deref());
        }
    }
    w.diags(&m.output);
    w.count(m.states.len());
    for (p, s) in &m.states {
        w.str(p);
        w.hash(&s.text);
        w.hash(&s.resolved);
        w.shape(&s.shape);
        w.hashes(&s.used);
        match &s.diags {
            Some(ds) => {
                w.u8(1);
                w.diags(ds);
            }
            None => w.u8(0),
        }
        w.diags(&s.syntax);
        w.diags(&s.references);
    }
    match &m.link {
        Some(ds) => {
            w.u8(1);
            w.diags(ds);
        }
        None => w.u8(0),
    }
    w.out
}

pub(crate) fn decode(data: &[u8]) -> Option<Manifest> {
    let mut r = Reader::new(data);
    let magic = [r.u8()?, r.u8()?, r.u8()?, r.u8()?];
    if &magic != MAGIC || r.u32()? != FORMAT_VERSION {
        return None;
    }
    let mut m = Manifest {
        program_key: r.hash()?,
        consistent: r.bool()?,
        ..Manifest::default()
    };
    for _ in 0..r.count()? {
        m.libs.push((r.str()?, r.hash()?));
    }
    for _ in 0..r.count()? {
        m.missing_libs.push(r.str()?);
    }
    m.lib_store = r.bool()?;
    for _ in 0..r.count()? {
        m.roots.push(r.str()?);
    }
    for _ in 0..r.count()? {
        m.types.push((r.str()?, r.opt_str()?));
    }
    for _ in 0..r.count()? {
        let path = r.str()?;
        let text = r.hash()?;
        let bind = r.hash()?;
        let external = r.bool()?;
        let pragma = r.bool()?;
        let mut requests = Vec::new();
        for _ in 0..r.count()? {
            let spec = r.str()?;
            let kind = r.u8()?;
            kind_of(kind)?;
            let mode = match r.u8()? {
                0 => None,
                1 => Some(ResolutionMode::Import),
                2 => Some(ResolutionMode::Require),
                _ => return None,
            };
            let res = match r.u8()? {
                0 => None,
                1 => Some(SavedRes {
                    path: r.str()?,
                    included: r.bool()?,
                    external: r.bool()?,
                    package: r.bool()?,
                    ext: *EXTENSIONS.get(r.u8()? as usize)?,
                }),
                _ => return None,
            };
            requests.push(SavedRequest {
                spec,
                kind,
                mode,
                res,
            });
        }
        let mut references = Vec::new();
        for _ in 0..r.count()? {
            references.push((r.str()?, r.str()?, r.opt_str()?));
        }
        m.files.push(SavedFile {
            path,
            text,
            bind,
            external,
            pragma,
            requests,
            references,
        });
    }
    m.output = r.diags()?;
    for _ in 0..r.count()? {
        let p = r.str()?;
        let text = r.hash()?;
        let resolved = r.hash()?;
        let shape = r.shape()?;
        let used = r.hashes()?;
        let diags = match r.u8()? {
            0 => None,
            1 => Some(r.diags()?),
            _ => return None,
        };
        let syntax = r.diags()?;
        let references = r.diags()?;
        m.states.push((
            p,
            FileState {
                text,
                resolved,
                shape,
                used,
                diags,
                syntax,
                references,
            },
        ));
    }
    m.link = match r.u8()? {
        0 => None,
        1 => Some(r.diags()?),
        _ => return None,
    };
    r.done().then_some(m)
}

/// Opens the history cache (`TS_R_CACHE_DIR` or the default root).
pub fn open_cache() -> Option<Cache> {
    Cache::open(NAMESPACE, FORMAT_VERSION).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shape::{FileShape, Provided, Provides, Ref};
    use ts_r_diagnostics::{Category, FileId, RelatedInfo};

    fn sample() -> Manifest {
        let diag = PathDiag {
            diag: Diagnostic {
                code: 2322,
                category: Category::Error,
                message: "Type 'string' is not assignable to type 'number'.".to_owned(),
                file: FileId(0),
                span: ts_r_span::Span { start: 3, end: 9 },
                related: vec![RelatedInfo {
                    code: 6500,
                    category: Category::Message,
                    message: "declared here".to_owned(),
                    file: FileId(0),
                    span: ts_r_span::Span { start: 0, end: 1 },
                }],
            },
            file: Some("src/a.ts".to_owned()),
            related: vec![Some("lib:lib.es5.d.ts".to_owned())],
        };
        let shape = FileShape {
            api: [7; 32],
            provides: vec![Provided {
                what: Provides::Export("a".to_owned()),
                hash: [1; 32],
                deps: vec![
                    Ref::Export("src/b.ts".to_owned(), "B".to_owned()),
                    Ref::Free("x".to_owned()),
                ],
            }],
            stars: vec!["src/c.ts".to_owned()],
            uses: vec![
                Ref::All("src/b.ts".to_owned()),
                Ref::LibGlobals,
                Ref::AllGlobals,
                Ref::Ambient("*.svg".to_owned()),
                Ref::Global("JSX".to_owned()),
            ],
            locals: [3; 32],
        };
        Manifest {
            program_key: [9; 32],
            consistent: true,
            libs: vec![("lib.es5.d.ts".to_owned(), [2; 32])],
            missing_libs: vec!["lib.dom.d.ts".to_owned()],
            lib_store: true,
            roots: vec!["src/a.ts".to_owned()],
            types: vec![("node".to_owned(), None)],
            files: vec![SavedFile {
                path: "src/a.ts".to_owned(),
                text: [3; 32],
                bind: [4; 32],
                external: false,
                pragma: true,
                requests: vec![
                    SavedRequest {
                        spec: "./b".to_owned(),
                        kind: 0,
                        mode: Some(ResolutionMode::Import),
                        res: Some(SavedRes {
                            path: "src/b.ts".to_owned(),
                            included: true,
                            external: false,
                            package: false,
                            ext: Extension::Tsx,
                        }),
                    },
                    SavedRequest {
                        spec: "x".to_owned(),
                        kind: 3,
                        mode: None,
                        res: None,
                    },
                ],
                references: vec![("path".to_owned(), "./g.d.ts".to_owned(), None)],
            }],
            output: vec![diag.clone()],
            states: vec![(
                "src/a.ts".to_owned(),
                FileState {
                    text: [3; 32],
                    resolved: [5; 32],
                    shape,
                    used: vec![[6; 32]],
                    diags: Some(vec![diag.clone()]),
                    syntax: vec![diag.clone()],
                    references: vec![diag.clone()],
                },
            )],
            link: Some(vec![diag]),
        }
    }

    #[test]
    fn round_trip() {
        let bytes = encode(&sample());
        let back = decode(&bytes).expect("decodes");
        assert_eq!(encode(&back), bytes);
        assert_eq!(back.states[0].1, sample().states[0].1);
        assert_eq!(back.files, sample().files);
    }

    #[test]
    fn damage_is_a_miss() {
        let bytes = encode(&sample());
        for n in 0..bytes.len() {
            assert!(decode(&bytes[..n]).is_none(), "prefix {n}");
        }
        let mut longer = bytes.clone();
        longer.push(0);
        assert!(decode(&longer).is_none());
        for i in 0..bytes.len() {
            let mut b = bytes.clone();
            b[i] ^= 0xff;
            let _ = decode(&b); // must not panic
        }
    }

    #[test]
    fn relative_paths() {
        let p = Paths {
            base: "/w/proj",
            lib_dir: Some("/opt/lib".to_owned()),
        };
        assert_eq!(p.rel("/w/proj/src/a.ts"), "src/a.ts");
        assert_eq!(
            p.rel("/w/node_modules/x/i.d.ts"),
            "../node_modules/x/i.d.ts"
        );
        assert_eq!(p.rel("/opt/lib/lib.es5.d.ts"), "lib:lib.es5.d.ts");
        for a in [
            "/w/proj/src/a.ts",
            "/w/node_modules/x/i.d.ts",
            "/opt/lib/lib.es5.d.ts",
        ] {
            assert_eq!(p.abs(&p.rel(a)), a);
        }
    }

    #[test]
    fn lib_paths_on_a_drive() {
        // Windows: the lib dir is a drive path, and its files are still libs.
        let p = Paths {
            base: "C:/w/proj",
            lib_dir: Some("C:/opt/lib".to_owned()),
        };
        assert_eq!(p.rel("C:/opt/lib/lib.es5.d.ts"), "lib:lib.es5.d.ts");
        assert_eq!(p.abs("lib:lib.es5.d.ts"), "C:/opt/lib/lib.es5.d.ts");
    }

    /// Another process saves a different manifest while this run scans files against the one
    /// it read: the fingerprints this run refreshes stay tied to the manifest it scanned.
    #[test]
    fn refreshed_fingerprints_belong_to_the_scanned_manifest() {
        use ts_r_config::{FileSystem, RealFs};

        struct Racing<'a> {
            inner: RealFs,
            cache: &'a Cache,
            key: ts_r_cache::Key,
        }
        impl FileSystem for Racing<'_> {
            fn read_file(&self, path: &str) -> Option<String> {
                if path.ends_with("/a.ts") {
                    let _ = self.cache.put(&self.key, b"a manifest saved meanwhile");
                }
                self.inner.read_file(path)
            }
            fn file_exists(&self, path: &str) -> bool {
                self.inner.file_exists(path)
            }
            fn directory_exists(&self, path: &str) -> bool {
                self.inner.directory_exists(path)
            }
            fn read_dir(&self, path: &str) -> Vec<ts_r_config::DirEntry> {
                self.inner.read_dir(path)
            }
            fn realpath(&self, path: &str) -> String {
                self.inner.realpath(path)
            }
            fn case_sensitive(&self) -> bool {
                self.inner.case_sensitive()
            }
            fn cwd(&self) -> String {
                self.inner.cwd()
            }
        }

        let Some(libs) = LibStore::locate() else {
            eprintln!("lib files missing: skipped");
            return;
        };
        crate::stat::set_racy_window(std::time::Duration::ZERO);
        let dir = std::env::temp_dir().join(format!("ts-r-history-race-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).expect("dir");
        std::fs::write(
            dir.join("tsconfig.json"),
            r#"{"compilerOptions":{"noEmit":true},"include":["src"]}"#,
        )
        .expect("write");
        let a = dir.join("src/a.ts");
        std::fs::write(&a, "export const a = 1;\n").expect("write");
        let cache = Cache::open_in(&dir.join("cache"), NAMESPACE, FORMAT_VERSION).expect("cache");
        let fs = RealFs::new();
        let config = dir
            .join("tsconfig.json")
            .to_string_lossy()
            .replace('\\', "/");
        let cfg = ts_r_config::load_tsconfig(&fs, &config);
        assert_eq!(
            check(&fs, Some(&cache), &cfg, Some(&libs), 1).mode,
            Mode::Cold
        );
        let key = manifest_key(&cfg.config_path, &cfg.options);
        let scanned = *blake3::hash(&cache.get(&key).expect("manifest")).as_bytes();
        // Same text, new fingerprint: the next run reads a.ts and refreshes the sidecar.
        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(&a, "export const a = 1;\n").expect("write");
        let racing = Racing {
            inner: RealFs::new(),
            cache: &cache,
            key,
        };
        let run = check(&racing, Some(&cache), &cfg, Some(&libs), 1);
        assert_eq!(run.mode, Mode::Unchanged, "{:?}", run.phases);
        let side = cache.get(&sidecar_key(&key)).expect("sidecar");
        assert!(crate::stat::Sidecar::decode(&side, &scanned).is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
