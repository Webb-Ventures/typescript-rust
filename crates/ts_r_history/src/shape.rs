//! Export shapes: what each file provides to others and what checking it reads from them
//! (design record 0005, "Invalidation", as implemented: see its implementation notes).
//!
//! A file's [`FileShape`] comes from its `ts_r_decls` index (exports, globals, ambient modules
//! and augmentations with their signature hashes and dependencies) and its binder output
//! (imports and free names), with module specifiers resolved to paths. [`DeepHashes`] then
//! hashes every [`Ref`] together with everything it (transitively) depends on: when a file
//! changes, another file must be re-checked only if the deep hash of something it uses
//! changed. Cycles (mutually dependent declarations) are hashed per strongly connected
//! component, so a change anywhere in a cycle changes the hash of every member.

use rustc_hash::{FxHashMap, FxHashSet};
use ts_r_binder::{AliasTarget, ImportName};
use ts_r_checker::{ProgramFiles, SourceFile};
use ts_r_decls::{Dep, Section};

pub type Hash = [u8; 32];

/// Something a file's checking can read from other files.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Ref {
    /// Export `name` (`default` for default exports) of the module file `path`.
    Export(String, String),
    /// Everything the module file `path` exports (namespace imports, `import()` types,
    /// `export =`).
    All(String),
    /// The declarations of a global name.
    Global(String),
    /// A free name the file references: its global declarations, or when there are none,
    /// every global name ("Cannot find name 'x'. Did you mean 'y'?").
    Free(String),
    /// A module specifier as an ambient module name (`declare module "x"`).
    Ambient(String),
    /// Global declarations of non-lib files that merge with lib globals (`declare global {
    /// interface Array<T> }`): every file uses lib types implicitly (`T[]`, `async`).
    LibGlobals,
    /// Every global declaration (`globalThis`, `window`).
    AllGlobals,
    /// Module augmentations of the module file `path` (in other files).
    Augs(String),
}

/// What a file provides.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Provides {
    Export(String),
    Global(String),
    Ambient(String),
    /// A module augmentation of the module file `path`.
    Augments(String),
}

/// One provided declaration: its own signature hash and what the signature depends on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Provided {
    pub what: Provides,
    pub hash: Hash,
    pub deps: Vec<Ref>,
}

/// What a file provides to and uses from other files. Paths are as in the program.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct FileShape {
    /// `ts_r_decls` API hash (for JSON modules the text hash).
    pub api: Hash,
    pub provides: Vec<Provided>,
    /// Targets of `export * from`.
    pub stars: Vec<String>,
    /// Everything checking the file reads from others (sorted, deduplicated).
    pub uses: Vec<Ref>,
    /// Hash of the module's top-level local names (zero for scripts and dependency files):
    /// an import of a name the module does not export reports TS2459/TS2460 when the
    /// module declares it locally, TS2305 when not.
    pub locals: Hash,
}

impl FileShape {
    /// The shape with every path mapped by `f` (to store relative paths and back).
    pub fn map_paths(&self, f: &dyn Fn(&str) -> String) -> FileShape {
        let r = |x: &Ref| match x {
            Ref::Export(p, n) => Ref::Export(f(p), n.clone()),
            Ref::All(p) => Ref::All(f(p)),
            Ref::Augs(p) => Ref::Augs(f(p)),
            other => other.clone(),
        };
        FileShape {
            api: self.api,
            provides: self
                .provides
                .iter()
                .map(|p| Provided {
                    what: match &p.what {
                        Provides::Augments(t) => Provides::Augments(f(t)),
                        other => other.clone(),
                    },
                    hash: p.hash,
                    deps: p.deps.iter().map(r).collect(),
                })
                .collect(),
            stars: self.stars.iter().map(|s| f(s)).collect(),
            uses: self.uses.iter().map(r).collect(),
            locals: self.locals,
        }
    }
}

/// Global names whose use reads every global (`globalThis.x`, `window.x`).
const ALL_GLOBALS_NAMES: [&str; 4] = ["globalThis", "window", "self", "global"];

/// Computes the shape of `f`, a file of `program`.
pub fn shape_of(program: &ProgramFiles, f: &SourceFile) -> FileShape {
    let resolve = |spec: &str| -> Option<String> {
        program
            .resolutions
            .get(&(f.path.clone(), spec.to_owned()))
            .and_then(|r| r.as_ref())
            .map(|r| r.resolved.clone())
    };
    if f.path.ends_with(".json") {
        return FileShape {
            api: *blake3::hash(f.text.as_bytes()).as_bytes(),
            ..FileShape::default()
        };
    }
    if f.is_external {
        return external_shape(program, f, &resolve);
    }
    let idx = ts_r_decls::index(&f.ast, &f.bound);
    let dep_ref = |d: &Dep| -> Ref {
        match d {
            Dep::Import { specifier, name } => match resolve(specifier) {
                Some(t) if name == "*" || name == "export=" => Ref::All(t),
                Some(t) => Ref::Export(t, name.clone()),
                None => Ref::Ambient(specifier.clone()),
            },
            Dep::Global(g) => Ref::Global(g.clone()),
            Dep::Sibling(n) => Ref::Export(f.path.clone(), n.clone()),
        }
    };
    let mut uses: Vec<Ref> = Vec::new();
    let mut provides: Vec<Provided> = Vec::new();
    for e in &idx.entries {
        let what = match e.section {
            Section::Export => Provides::Export(e.name.clone()),
            Section::Global | Section::GlobalAugmentation => Provides::Global(e.name.clone()),
            Section::AmbientModule => Provides::Ambient(e.name.clone()),
            // An augmentation of a module that does not resolve is an error, not an
            // ambient module (keyed by its specifier).
            Section::ModuleAugmentation => match resolve(&e.name) {
                Some(t) => Provides::Augments(t),
                None => Provides::Augments(e.name.clone()),
            },
        };
        let mut deps: Vec<Ref> = e.deps.iter().map(dep_ref).collect();
        if e.section == Section::ModuleAugmentation && is_bare(&e.name) {
            // The linker merges it into an ambient module of that name when there is one.
            deps.push(Ref::Ambient(e.name.clone()));
        }
        deps.sort();
        deps.dedup();
        uses.extend(deps.iter().cloned());
        provides.push(Provided {
            what,
            hash: e.hash,
            deps,
        });
    }
    if let Some(u) = &idx.umd_name {
        provides.push(Provided {
            what: Provides::Global(u.clone()),
            hash: idx.api_hash,
            deps: vec![Ref::All(f.path.clone())],
        });
    }
    provides.sort_by(|a, b| a.what.cmp(&b.what).then(a.hash.cmp(&b.hash)));
    let mut stars: Vec<String> = idx.export_stars.iter().filter_map(|s| resolve(s)).collect();
    stars.sort();
    stars.dedup();

    let mut shape = FileShape {
        api: idx.api_hash,
        provides,
        stars,
        uses,
        locals: module_locals_hash(f),
    };
    // Declaration dependencies identify imports by specifier only. When requests of the
    // same specifier resolve apart, conservatively include every alternate target.
    for (spec, _, mode) in crate::module_requests_with_modes(f, &program.options) {
        if mode.is_some()
            && let Some(Some(r)) = crate::request_resolution(program, f, &spec, mode)
        {
            let dep = Ref::All(r.resolved.clone());
            shape.uses.push(dep.clone());
            for p in &mut shape.provides {
                p.deps.push(dep.clone());
                p.deps.sort();
                p.deps.dedup();
            }
        }
    }
    add_uses(program, f, &resolve, &mut shape);
    shape
}

/// The shape of a dependency file (`node_modules`): what it provides at file granularity
/// (every entry hashed by the file's text and depending on everything the file imports).
/// Cheap (no signature hashing), and dependencies rarely change.
fn external_shape(
    program: &ProgramFiles,
    f: &SourceFile,
    resolve: &dyn Fn(&str) -> Option<String>,
) -> FileShape {
    let b = &f.bound;
    let text = *blake3::hash(f.text.as_bytes()).as_bytes();
    let own = vec![Ref::All(f.path.clone())];
    let mut deps: Vec<Ref> = program
        .deps
        .get(&f.path)
        .into_iter()
        .flatten()
        .map(|d| Ref::All(d.clone()))
        .collect();
    let mut provides: Vec<Provided> = Vec::new();
    for (spec, kind) in crate::module_requests(f, &program.options) {
        let target = resolve(&spec);
        if target.is_none() {
            deps.push(Ref::Ambient(spec.to_string()));
        }
        if kind == ts_r_binder::RequestKind::Augmentation {
            let mut deps = own.clone();
            if is_bare(&spec) {
                deps.push(Ref::Ambient(spec.to_string()));
            }
            provides.push(Provided {
                what: Provides::Augments(target.unwrap_or_else(|| spec.to_string())),
                hash: text,
                deps,
            });
        }
    }
    deps.sort();
    deps.dedup();
    provides.push(Provided {
        what: Provides::Export("*".to_owned()),
        hash: text,
        deps,
    });
    let globals = b
        .script_globals()
        .iter()
        .chain(b.table(b.global_augmentations));
    for &(n, _) in globals {
        provides.push(Provided {
            what: Provides::Global(b.name_text(&f.ast, n).to_owned()),
            hash: text,
            deps: own.clone(),
        });
    }
    for &(n, _) in b.table(b.ambient_modules) {
        provides.push(Provided {
            what: Provides::Ambient(b.name_text(&f.ast, n).to_owned()),
            hash: text,
            deps: own.clone(),
        });
    }
    if let Some((n, _)) = b.umd_name {
        provides.push(Provided {
            what: Provides::Global(b.name_text(&f.ast, n).to_owned()),
            hash: text,
            deps: own.clone(),
        });
    }
    provides.sort_by(|a, b| a.what.cmp(&b.what).then(a.hash.cmp(&b.hash)));
    provides.dedup();
    let mut shape = FileShape {
        api: text,
        provides,
        stars: Vec::new(),
        uses: Vec::new(),
        locals: [0; 32],
    };
    add_uses(program, f, resolve, &mut shape);
    // Its declarations can name globals a project file declares (`value: CustomGlobal`):
    // the file's entry depends on the global names it reads, so deep hashes and the lazy
    // check's reach follow them like they follow its imports.
    let free: Vec<Ref> = shape
        .uses
        .iter()
        .filter(|u| matches!(u, Ref::Free(_) | Ref::AllGlobals))
        .cloned()
        .collect();
    for p in &mut shape.provides {
        if matches!(&p.what, Provides::Export(n) if n == "*") {
            p.deps.extend(free.iter().cloned());
            p.deps.sort();
            p.deps.dedup();
        }
    }
    shape
}

/// A non-relative module specifier (one that can name an ambient module).
fn is_bare(spec: &str) -> bool {
    !spec.starts_with('.') && !spec.starts_with('/')
}

/// Hash of the sorted names in a module's top-level scope (zero for a script, whose
/// top-level names are globals and tracked as such).
fn module_locals_hash(f: &SourceFile) -> Hash {
    let b = &f.bound;
    let Some(top) = b.scopes.first().filter(|_| b.is_module) else {
        return [0; 32];
    };
    let mut names: Vec<&str> = b
        .table(top.locals)
        .iter()
        .map(|&(n, _)| b.name_text(&f.ast, n))
        .collect();
    names.sort_unstable();
    let mut h = blake3::Hasher::new();
    h.update(&(names.len() as u64).to_le_bytes());
    for n in names {
        str_into(&mut h, n);
    }
    *h.finalize().as_bytes()
}

/// Adds what checking `f` reads from other files to `shape.uses` (sorted, deduplicated).
fn add_uses(
    program: &ProgramFiles,
    f: &SourceFile,
    resolve: &dyn Fn(&str) -> Option<String>,
    shape: &mut FileShape,
) {
    let FileShape {
        provides,
        stars,
        uses,
        ..
    } = shape;
    // Imports.
    let b = &f.bound;
    let mut covered: FxHashSet<u32> = FxHashSet::default();
    for a in &b.aliases {
        let AliasTarget::Module { request, import } = a.target else {
            continue;
        };
        let Some(req) = b.module_requests.get(request as usize) else {
            continue;
        };
        covered.insert(request);
        let spec = f.ast.atom(req.specifier);
        if is_bare(spec) {
            uses.push(Ref::Ambient(spec.to_owned()));
        }
        uses.push(match resolve(spec) {
            Some(t) => match import {
                ImportName::Named(n) => Ref::Export(t, b.name_text(&f.ast, n).to_owned()),
                ImportName::Default => Ref::Export(t, "default".to_owned()),
                ImportName::Namespace | ImportName::ExportEquals => Ref::All(t),
            },
            None => Ref::Ambient(spec.to_owned()),
        });
    }
    let implicit_helpers =
        usize::from(ts_r_checker::emit_helpers::helpers_specifier(f, &program.options).is_some());
    for (i, (spec, _)) in crate::module_requests(f, &program.options).enumerate() {
        if is_bare(&spec) {
            uses.push(Ref::Ambient(spec.to_string()));
        }
        if i.checked_sub(implicit_helpers)
            .is_some_and(|request| covered.contains(&(request as u32)))
        {
            continue;
        }
        uses.push(match resolve(&spec) {
            Some(t) => Ref::All(t),
            None => Ref::Ambient(spec.to_string()),
        });
    }
    for s in stars.iter() {
        uses.push(Ref::All(s.clone()));
    }
    // Globals: free names, and the file's own global declarations (merges and duplicates).
    for r in &b.refs.free {
        let name = b.name_text(&f.ast, r.name);
        if ALL_GLOBALS_NAMES.contains(&name) {
            uses.push(Ref::AllGlobals);
        }
        uses.push(Ref::Free(name.to_owned()));
    }
    for p in provides.iter() {
        if let Provides::Global(g) = &p.what {
            uses.push(Ref::Global(g.clone()));
        }
        if let Provides::Ambient(a) = &p.what {
            uses.push(Ref::Ambient(a.clone()));
        }
        if let Provides::Augments(t) = &p.what {
            uses.push(Ref::All(t.clone()));
        }
    }
    // Its own module: augmentations of it in other files change its declarations.
    uses.push(Ref::Augs(f.path.clone()));
    // Program edges the binder output does not show (`/// <reference path|types>`); import
    // edges are covered above.
    let requested: FxHashSet<String> = crate::module_requests(f, &program.options)
        .filter_map(|(spec, _)| resolve(&spec))
        .collect();
    for d in program.deps.get(&f.path).into_iter().flatten() {
        if !requested.contains(d) {
            uses.push(Ref::All(d.clone()));
        }
    }
    // Implicit: lib types (`T[]`, promises), JSX element types.
    uses.push(Ref::LibGlobals);
    uses.push(Ref::Global("JSX".to_owned()));
    uses.push(Ref::Global("React".to_owned()));
    uses.sort();
    uses.dedup();
}

/// How paths are written in saved history: relative to the config directory, lib files as
/// `lib:<name>`, anything not absolute (an unresolved specifier) as `?<spec>`. Deep hashes
/// hash paths in this form, so hashes computed over absolute paths match hashes computed
/// over saved (relative) shapes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PathKey {
    pub base: String,
    pub lib_dir: Option<String>,
}

impl PathKey {
    pub fn rel(&self, p: &str) -> String {
        // The lib dir first: on Windows it is a drive path (`C:/...`), which the `/` test
        // below would write as `?C:/...`, so a lib file's stored diagnostics would no
        // longer name it as a lib.
        if let Some(d) = &self.lib_dir
            && let Some(rest) = p.strip_prefix(d.as_str())
            && let Some(name) = rest.strip_prefix('/')
        {
            return format!("lib:{name}");
        }
        if !p.starts_with('/') {
            return format!("?{p}");
        }
        let base = self.base.trim_end_matches('/');
        if let Some(rest) = p.strip_prefix(base)
            && let Some(rest) = rest.strip_prefix('/')
        {
            return rest.to_owned();
        }
        let a: Vec<&str> = p.split('/').filter(|s| !s.is_empty()).collect();
        let b: Vec<&str> = base.split('/').filter(|s| !s.is_empty()).collect();
        let common = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
        let mut out: Vec<&str> = vec![".."; b.len() - common];
        out.extend(&a[common..]);
        out.join("/")
    }
}

/// Shapes of every non-lib file of a program, kept across edits.
#[derive(Clone, Debug, Default)]
pub struct ProgramShapes {
    pub shapes: FxHashMap<String, FileShape>,
    pub lib_globals: FxHashSet<String>,
    /// Program order of the files (merged declarations see each other in this order).
    pub order: Vec<String>,
    /// Paths are hashed in this form (see [`PathKey`]); as they are when `None`.
    pub key: Option<PathKey>,
}

impl ProgramShapes {
    /// Shapes of every non-lib file of `program`, on up to `threads` threads.
    pub fn build(program: &ProgramFiles, threads: usize) -> ProgramShapes {
        let files: Vec<&SourceFile> = program
            .files
            .iter()
            .filter(|f| !f.is_lib)
            .map(|f| &**f)
            .collect();
        ProgramShapes {
            shapes: shapes_of(program, &files, threads).into_iter().collect(),
            lib_globals: lib_global_names(program),
            order: files.iter().map(|f| f.path.clone()).collect(),
            key: None,
        }
    }

    /// Deep hashes over these shapes.
    pub fn deep(&self) -> DeepHashes<'_> {
        let mut list: Vec<(&str, &FileShape)> = Vec::with_capacity(self.shapes.len());
        let mut listed: FxHashSet<&str> = FxHashSet::default();
        for p in &self.order {
            if let Some((p, s)) = self.shapes.get_key_value(p)
                && listed.insert(p.as_str())
            {
                list.push((p.as_str(), s));
            }
        }
        let mut rest: Vec<(&str, &FileShape)> = self
            .shapes
            .iter()
            .filter(|(p, _)| !listed.contains(p.as_str()))
            .map(|(p, s)| (p.as_str(), s))
            .collect();
        rest.sort_unstable_by(|a, b| a.0.cmp(b.0));
        list.extend(rest);
        let mut d = DeepHashes::new(&list, &self.lib_globals);
        d.key = self.key.as_ref();
        d
    }

    /// Replaces the shapes of `changed` (paths of `program`; missing ones are removed) and
    /// returns the `candidates` (other files) that must be re-checked: those with a use whose
    /// deep hash changed.
    pub fn update(
        &mut self,
        program: &ProgramFiles,
        changed: &[String],
        candidates: &[String],
    ) -> Vec<String> {
        let cands: Vec<&String> = candidates
            .iter()
            .filter(|c| !changed.contains(c) && self.shapes.contains_key(*c))
            .collect();
        let fresh: Vec<(String, Option<FileShape>)> = changed
            .iter()
            .map(|p| {
                let s = program
                    .file_index(p)
                    .map(|i| &program.files[i])
                    .filter(|f| !f.is_lib)
                    .map(|f| shape_of(program, f));
                (p.clone(), s)
            })
            .collect();
        self.order = program
            .files
            .iter()
            .filter(|f| !f.is_lib)
            .map(|f| f.path.clone())
            .collect();
        // Same provided surface: nothing another file uses changed.
        let same_surface = |a: &FileShape, b: &FileShape| {
            a.api == b.api && a.provides == b.provides && a.stars == b.stars && a.locals == b.locals
        };
        if fresh.iter().all(|(p, s)| match (self.shapes.get(p), s) {
            (Some(a), Some(b)) => same_surface(a, b),
            (None, None) => true,
            _ => false,
        }) {
            for (p, s) in fresh {
                if let Some(s) = s {
                    self.shapes.insert(p, s);
                }
            }
            return Vec::new();
        }
        let before: Vec<Vec<Hash>> = {
            let mut d = self.deep();
            cands
                .iter()
                .map(|c| self.shapes[*c].uses.iter().map(|u| d.of(u)).collect())
                .collect()
        };
        for (p, s) in fresh {
            match s {
                Some(s) => self.shapes.insert(p, s),
                None => self.shapes.remove(&p),
            };
        }
        let mut d = self.deep();
        cands
            .iter()
            .zip(before)
            .filter(|(c, b)| {
                let uses = &self.shapes[**c].uses;
                uses.iter().zip(b).any(|(u, h)| d.of(u) != *h)
            })
            .map(|(c, _)| (*c).clone())
            .collect()
    }

    /// The `candidates` that must be re-checked when the program's shapes go from `self` to
    /// `new` (a reload): new or gone files, changed uses, or a use whose deep hash changed.
    pub fn affected_by(&self, new: &ProgramShapes, candidates: &[String]) -> Vec<String> {
        let mut a = self.deep();
        let mut b = new.deep();
        candidates
            .iter()
            .filter(|c| match (self.shapes.get(*c), new.shapes.get(*c)) {
                (Some(o), Some(n)) => o.uses != n.uses || n.uses.iter().any(|u| a.of(u) != b.of(u)),
                _ => true,
            })
            .cloned()
            .collect()
    }
}

/// Shapes of `files` (of `program`) on up to `threads` threads, in order.
pub fn shapes_of(
    program: &ProgramFiles,
    files: &[&SourceFile],
    threads: usize,
) -> Vec<(String, FileShape)> {
    let threads = threads.clamp(1, 16);
    if threads == 1 || files.len() < 32 {
        return files
            .iter()
            .map(|f| (f.path.clone(), shape_of(program, f)))
            .collect();
    }
    let chunk = files.len().div_ceil(threads);
    std::thread::scope(|s| {
        let handles: Vec<_> = files
            .chunks(chunk)
            .map(|c| {
                s.spawn(move || {
                    c.iter()
                        .map(|f| (f.path.clone(), shape_of(program, f)))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap_or_default())
            .collect()
    })
}

/// Global names the lib files declare (script globals and `declare global`).
pub fn lib_global_names(program: &ProgramFiles) -> FxHashSet<String> {
    let mut out = FxHashSet::default();
    for f in program.files.iter().filter(|f| f.is_lib) {
        out.extend(lib_globals_of(f));
    }
    out
}

/// The global names lib file `f` declares.
pub fn lib_globals_of(f: &SourceFile) -> Vec<String> {
    let b = &f.bound;
    b.script_globals()
        .iter()
        .chain(b.table(b.global_augmentations))
        .map(|&(n, _)| b.name_text(&f.ast, n).to_owned())
        .collect()
}

/// What a set of uses reaches (see [`DeepHashes::reach`]).
#[derive(Debug, Default)]
pub struct Reach<'a> {
    /// Files whose declarations are reached.
    pub files: FxHashSet<&'a str>,
    /// Global names reached (declared or free).
    pub names: FxHashSet<String>,
    /// Every global is reached (`globalThis`, an undeclared name).
    pub all_globals: bool,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
enum Node {
    All(String),
    Aug(String),
    Global(String),
    Free(String),
    AmbDecl(String),
    AmbUse(String),
    Missing(String),
    /// One export of a module file (by name), resolved through its own entries, then its
    /// `export *` targets. `true`: the import itself (an unresolved name depends on every
    /// name, "Did you mean"); `false`: a lookup through `export *`.
    Export(String, String, bool),
    /// The set of names a module file exports (a missing import's "Did you mean").
    Names(String),
    LibGlobals,
    AllGlobals,
}

type Parts<'a> = Vec<(&'a str, &'a Hash, &'a [Ref])>;

/// Deep hashes of [`Ref`]s over a set of file shapes.
#[derive(Debug)]
pub struct DeepHashes<'a> {
    /// Export entries by module path.
    exports: FxHashMap<&'a str, Parts<'a>>,
    /// Export entries by module path and export name.
    named: FxHashMap<&'a str, FxHashMap<&'a str, Parts<'a>>>,
    files: FxHashMap<&'a str, &'a FileShape>,
    augs: FxHashMap<&'a str, Parts<'a>>,
    globals: FxHashMap<&'a str, Parts<'a>>,
    ambient: FxHashMap<&'a str, Parts<'a>>,
    ambient_names: Vec<&'a str>,
    /// Module augmentations that can merge into an ambient module: (specifier, key in
    /// `augs`).
    amb_augs: Vec<(&'a str, &'a str)>,
    global_names: Vec<&'a str>,
    lib_globals: Vec<&'a str>,
    /// Every global name the lib files declare.
    lib_all: &'a FxHashSet<String>,
    ids: FxHashMap<Node, u32>,
    nodes: Vec<Node>,
    deep: Vec<Option<Hash>>,
    /// Paths are hashed in this form; as they are when `None`.
    pub key: Option<&'a PathKey>,
}

impl<'a> DeepHashes<'a> {
    /// `shapes`: every non-lib file of the program with its shape, in program order;
    /// `lib_globals`: global names the lib files declare.
    pub fn new(shapes: &[(&'a str, &'a FileShape)], lib_globals: &'a FxHashSet<String>) -> Self {
        let mut d = DeepHashes {
            exports: FxHashMap::default(),
            named: FxHashMap::default(),
            files: FxHashMap::default(),
            augs: FxHashMap::default(),
            globals: FxHashMap::default(),
            ambient: FxHashMap::default(),
            ambient_names: Vec::new(),
            amb_augs: Vec::new(),
            global_names: Vec::new(),
            lib_globals: Vec::new(),
            lib_all: lib_globals,
            ids: FxHashMap::default(),
            nodes: Vec::new(),
            deep: Vec::new(),
            key: None,
        };
        for &(path, s) in shapes {
            d.files.insert(path, s);
            for p in &s.provides {
                let part = (path, &p.hash, p.deps.as_slice());
                match &p.what {
                    Provides::Export(n) => {
                        d.exports.entry(path).or_default().push(part);
                        d.named
                            .entry(path)
                            .or_default()
                            .entry(n.as_str())
                            .or_default()
                            .push(part);
                    }
                    Provides::Global(g) => d.globals.entry(g.as_str()).or_default().push(part),
                    Provides::Ambient(a) => d.ambient.entry(a.as_str()).or_default().push(part),
                    Provides::Augments(t) => {
                        d.augs.entry(t.as_str()).or_default().push(part);
                        for r in &p.deps {
                            if let Ref::Ambient(spec) = r {
                                d.amb_augs.push((spec.as_str(), t.as_str()));
                            }
                        }
                    }
                }
            }
        }
        d.ambient_names = d.ambient.keys().copied().collect();
        d.ambient_names.sort_unstable();
        d.amb_augs.sort_unstable();
        d.amb_augs.dedup();
        d.global_names = d.globals.keys().copied().collect();
        d.global_names.sort_unstable();
        d.lib_globals = d
            .global_names
            .iter()
            .copied()
            .filter(|g| lib_globals.contains(*g))
            .collect();
        d
    }

    /// The deep hash of `r`: changes when anything `r` stands for, or depends on, changes.
    pub fn of(&mut self, r: &Ref) -> Hash {
        let n = self.node_of_ref(r);
        let id = self.id(n);
        if let Some(h) = self.deep[id as usize] {
            return h;
        }
        self.tarjan(id);
        self.deep[id as usize].unwrap_or([0; 32])
    }

    /// Every file whose declarations `roots` (and what they depend on) stand for: the files
    /// a checker needs to see to check a file with these uses.
    pub fn files_reached<'r>(
        &self,
        roots: impl IntoIterator<Item = &'r Ref>,
    ) -> FxHashSet<&'a str> {
        self.reach(roots).files
    }

    /// The files (as [`DeepHashes::files_reached`]) and global names `roots` reach.
    pub fn reach<'r>(&self, roots: impl IntoIterator<Item = &'r Ref>) -> Reach<'a> {
        let mut seen: FxHashSet<Node> = FxHashSet::default();
        let mut stack: Vec<Node> = roots.into_iter().map(|r| self.node_of_ref(r)).collect();
        let mut out: FxHashSet<&'a str> = FxHashSet::default();
        let mut names: FxHashSet<String> = FxHashSet::default();
        let mut all_globals = false;
        let add = |out: &mut FxHashSet<&'a str>, ps: Option<&Parts<'a>>| {
            for (p, ..) in ps.into_iter().flatten() {
                out.insert(*p);
            }
        };
        while let Some(n) = stack.pop() {
            if seen.contains(&n) {
                continue;
            }
            match &n {
                Node::All(t) | Node::Export(t, ..) | Node::Names(t) => {
                    if let Some((p, _)) = self.files.get_key_value(t.as_str()) {
                        out.insert(*p);
                    }
                }
                Node::Aug(t) => add(&mut out, self.augs.get(t.as_str())),
                Node::Global(g) => {
                    names.insert(g.clone());
                    add(&mut out, self.globals.get(g.as_str()));
                }
                Node::Free(g) => {
                    names.insert(g.clone());
                }
                Node::AmbDecl(a) => add(&mut out, self.ambient.get(a.as_str())),
                Node::AllGlobals => all_globals = true,
                _ => {}
            }
            let (_, succ) = self.expand(&n);
            seen.insert(n);
            stack.extend(succ.into_iter().filter(|s| !seen.contains(s)));
        }
        Reach {
            files: out,
            names,
            all_globals,
        }
    }

    fn node_of_ref(&self, r: &Ref) -> Node {
        match r {
            Ref::Export(t, n) => self.export_node(t, n, true),
            Ref::All(t) => {
                if self.files.contains_key(t.as_str()) {
                    Node::All(t.clone())
                } else {
                    Node::Missing(t.clone())
                }
            }
            Ref::Global(g) => Node::Global(g.clone()),
            Ref::Free(g) => Node::Free(g.clone()),
            Ref::Ambient(s) => Node::AmbUse(s.clone()),
            Ref::LibGlobals => Node::LibGlobals,
            Ref::AllGlobals => Node::AllGlobals,
            Ref::Augs(t) => Node::Aug(t.clone()),
        }
    }

    /// Whether exports of `t` are tracked per name: a project module whose entries come from
    /// `ts_r_decls` (which records references between exports). Dependency files (one
    /// entry `*` for the file), JSON modules and `export =` modules count as a whole.
    fn per_export(&self, t: &str) -> bool {
        !t.ends_with(".json")
            && self.files.get(t).is_some_and(|s| {
                !s.provides
                    .iter()
                    .any(|p| matches!(&p.what, Provides::Export(n) if n == "*" || n == "export="))
            })
    }

    fn export_node(&self, t: &str, n: &str, root: bool) -> Node {
        if !self.files.contains_key(t) {
            Node::Missing(t.to_owned())
        } else if self.per_export(t) {
            Node::Export(t.to_owned(), n.to_owned(), root)
        } else {
            Node::All(t.to_owned())
        }
    }

    /// Whether `t` surely exports `n`: declared by `t` or a per-export module its `export *`
    /// targets reach.
    fn resolves(&self, t: &str, n: &str) -> bool {
        let mut seen: FxHashSet<&str> = FxHashSet::default();
        let mut stack: Vec<&str> = vec![t];
        while let Some(t) = stack.pop() {
            if !seen.insert(t) || !self.per_export(t) {
                continue;
            }
            if self.named.get(t).is_some_and(|m| m.contains_key(n)) {
                return true;
            }
            if let Some(s) = self.files.get(t) {
                stack.extend(s.stars.iter().map(String::as_str));
            }
        }
        false
    }

    fn names_node(&self, t: &str) -> Node {
        if !self.files.contains_key(t) {
            Node::Missing(t.to_owned())
        } else if self.per_export(t) {
            Node::Names(t.to_owned())
        } else {
            Node::All(t.to_owned())
        }
    }

    fn id(&mut self, n: Node) -> u32 {
        if let Some(&i) = self.ids.get(&n) {
            return i;
        }
        let i = self.nodes.len() as u32;
        self.nodes.push(n.clone());
        self.deep.push(None);
        self.ids.insert(n, i);
        i
    }

    fn path_into(&self, h: &mut blake3::Hasher, p: &str) {
        match self.key {
            Some(k) => str_into(h, &k.rel(p)),
            None => str_into(h, p),
        }
    }

    /// The node's own hash and its successors.
    fn expand(&self, n: &Node) -> (Hash, Vec<Node>) {
        let mut h = blake3::Hasher::new();
        let mut succ: Vec<Node> = Vec::new();
        let parts = |h: &mut blake3::Hasher, succ: &mut Vec<Node>, ps: Option<&Parts<'a>>| {
            // In program order: merged declarations depend on it (the first declaration of
            // a global variable fixes its type, interface members merge in order).
            let ps: Vec<&(&str, &Hash, &[Ref])> =
                ps.map(|v| v.iter().collect()).unwrap_or_default();
            h.update(&(ps.len() as u64).to_le_bytes());
            for (path, hash, deps) in ps {
                self.path_into(h, path);
                h.update(*hash);
                for d in deps.iter() {
                    succ.push(self.node_of_ref(d));
                }
            }
        };
        match n {
            Node::All(t) => {
                h.update(b"A");
                self.path_into(&mut h, t);
                if let Some(s) = self.files.get(t.as_str()) {
                    h.update(&s.api);
                    for st in &s.stars {
                        succ.push(Node::All(st.clone()));
                    }
                }
                parts(&mut h, &mut succ, self.exports.get(t.as_str()));
                if self.augs.contains_key(t.as_str()) {
                    succ.push(Node::Aug(t.clone()));
                }
            }
            Node::Aug(t) => {
                h.update(b"G");
                self.path_into(&mut h, t);
                parts(&mut h, &mut succ, self.augs.get(t.as_str()));
            }
            Node::Global(g) => {
                h.update(b"g");
                str_into(&mut h, g);
                parts(&mut h, &mut succ, self.globals.get(g.as_str()));
            }
            Node::Free(g) => {
                h.update(b"f");
                str_into(&mut h, g);
                succ.push(Node::Global(g.clone()));
                if !self.globals.contains_key(g.as_str()) && !self.lib_all.contains(g.as_str()) {
                    // Not declared anywhere: the error suggests similar names.
                    succ.push(Node::AllGlobals);
                }
            }
            Node::AmbDecl(a) => {
                h.update(b"d");
                str_into(&mut h, a);
                parts(&mut h, &mut succ, self.ambient.get(a.as_str()));
                for &(spec, key) in &self.amb_augs {
                    if crate::levels::ambient_matches(a, spec) {
                        succ.push(Node::Aug(key.to_owned()));
                    }
                }
            }
            Node::AmbUse(s) => {
                h.update(b"u");
                str_into(&mut h, s);
                for a in &self.ambient_names {
                    if crate::levels::ambient_matches(a, s) {
                        succ.push(Node::AmbDecl((*a).to_owned()));
                    }
                }
            }
            Node::Export(t, name, root) => {
                h.update(b"e");
                self.path_into(&mut h, t);
                str_into(&mut h, name);
                match self
                    .named
                    .get(t.as_str())
                    .and_then(|m| m.get(name.as_str()))
                {
                    Some(ps) => parts(&mut h, &mut succ, Some(ps)),
                    None => {
                        // Not declared here: through `export *` (every target's answer:
                        // two targets with the name conflict).
                        h.update(b"*");
                        if let Some(s) = self.files.get(t.as_str()) {
                            for st in &s.stars {
                                self.path_into(&mut h, st);
                                succ.push(self.export_node(st, name, false));
                            }
                        }
                        // Unresolved: the error suggests similar names.
                        if *root && !self.resolves(t, name) {
                            succ.push(Node::Names(t.clone()));
                        }
                    }
                }
                // Module augmentations of `t` merge into its exports.
                if self.augs.contains_key(t.as_str()) {
                    succ.push(Node::Aug(t.clone()));
                }
            }
            Node::Names(t) => {
                h.update(b"n");
                self.path_into(&mut h, t);
                if let Some(m) = self.named.get(t.as_str()) {
                    let mut names: Vec<&str> = m.keys().copied().collect();
                    names.sort_unstable();
                    h.update(&(names.len() as u64).to_le_bytes());
                    for n in names {
                        str_into(&mut h, n);
                    }
                }
                if let Some(s) = self.files.get(t.as_str()) {
                    if !self.named.contains_key(t.as_str()) {
                        // No exports: a script or an empty module ("is not a module").
                        h.update(&s.api);
                    }
                    // A missing export the module declares locally is TS2459/TS2460.
                    h.update(&s.locals);
                    for st in &s.stars {
                        self.path_into(&mut h, st);
                        succ.push(self.names_node(st));
                    }
                }
                if self.augs.contains_key(t.as_str()) {
                    succ.push(Node::Aug(t.clone()));
                }
            }
            Node::Missing(t) => {
                h.update(b"m");
                self.path_into(&mut h, t);
            }
            Node::LibGlobals => {
                h.update(b"L");
                for g in &self.lib_globals {
                    succ.push(Node::Global((*g).to_owned()));
                }
            }
            Node::AllGlobals => {
                h.update(b"*");
                for g in &self.global_names {
                    succ.push(Node::Global((*g).to_owned()));
                }
            }
        }
        (*h.finalize().as_bytes(), succ)
    }

    /// Tarjan's strongly connected components from `root` (iterative); sets `deep` for every
    /// node reached. Components come out successors first, so their successors' hashes are
    /// known when a component is hashed.
    fn tarjan(&mut self, root: u32) {
        // Per visited node: (index, lowlink, own hash, successors).
        let mut seen: FxHashMap<u32, (u32, u32, Hash, Vec<u32>)> = FxHashMap::default();
        let mut on_stack: FxHashSet<u32> = FxHashSet::default();
        let mut stack: Vec<u32> = Vec::new();
        // (node, next successor position)
        let mut frames: Vec<(u32, usize)> = Vec::new();
        let mut counter = 0u32;
        self.visit(root, &mut seen, &mut counter);
        stack.push(root);
        on_stack.insert(root);
        frames.push((root, 0));
        while let Some(&mut (n, ref mut next)) = frames.last_mut() {
            let succ = seen.get(&n).and_then(|e| e.3.get(*next).copied());
            if let Some(s) = succ {
                *next += 1;
                if self.deep[s as usize].is_some() {
                    continue;
                }
                if let Some(&(si, ..)) = seen.get(&s) {
                    if on_stack.contains(&s)
                        && let Some(e) = seen.get_mut(&n)
                    {
                        e.1 = e.1.min(si);
                    }
                    continue;
                }
                self.visit(s, &mut seen, &mut counter);
                stack.push(s);
                on_stack.insert(s);
                frames.push((s, 0));
                continue;
            }
            frames.pop();
            let (ni, nl) = seen.get(&n).map_or((0, 0), |e| (e.0, e.1));
            if let Some(&(p, _)) = frames.last()
                && let Some(e) = seen.get_mut(&p)
            {
                e.1 = e.1.min(nl);
            }
            if nl != ni {
                continue;
            }
            // `n` roots a component: pop it and hash it.
            let mut members: Vec<u32> = Vec::new();
            while let Some(m) = stack.pop() {
                on_stack.remove(&m);
                members.push(m);
                if m == n {
                    break;
                }
            }
            let member_set: FxHashSet<u32> = members.iter().copied().collect();
            let mut owns: Vec<Hash> = Vec::with_capacity(members.len());
            let mut outs: Vec<Hash> = Vec::new();
            for m in &members {
                if let Some(e) = seen.get(m) {
                    owns.push(e.2);
                    for &s in &e.3 {
                        if !member_set.contains(&s)
                            && let Some(h) = self.deep[s as usize]
                        {
                            outs.push(h);
                        }
                    }
                }
            }
            owns.sort_unstable();
            outs.sort_unstable();
            outs.dedup();
            let mut h = blake3::Hasher::new();
            h.update(b"scc");
            for o in &owns {
                h.update(o);
            }
            h.update(b"->");
            for o in &outs {
                h.update(o);
            }
            let hash = *h.finalize().as_bytes();
            for m in members {
                self.deep[m as usize] = Some(hash);
            }
        }
    }

    fn visit(
        &mut self,
        n: u32,
        seen: &mut FxHashMap<u32, (u32, u32, Hash, Vec<u32>)>,
        counter: &mut u32,
    ) {
        let node = self.nodes[n as usize].clone();
        let (h, succ) = self.expand(&node);
        let mut ids: Vec<u32> = succ.into_iter().map(|s| self.id(s)).collect();
        ids.sort_unstable();
        ids.dedup();
        seen.insert(n, (*counter, *counter, h, ids));
        *counter += 1;
    }
}

fn str_into(h: &mut blake3::Hasher, s: &str) {
    h.update(&(s.len() as u64).to_le_bytes());
    h.update(s.as_bytes());
}
