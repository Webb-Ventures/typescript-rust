//! Linking: module formats, wrapping, import/export binding and tree shaking.
//!
//! - **Exports kind.** A module with `export` statements is ESM. Otherwise free `module` /
//!   `exports` references make it CommonJS; otherwise `import` statements make it ESM; a
//!   file with no module syntax is CommonJS when something `require()`s it and ESM (with
//!   no exports) otherwise.
//! - **Wrapping.** CommonJS modules are wrapped in a lazy `require_x()` closure. ESM modules
//!   that are `require()`d (or dynamically imported without code splitting) get a lazy
//!   `init_x()` wrapper; their static ESM dependencies are wrapped too so evaluation order
//!   is kept.
//! - **Bindings.** Every import binding resolves through re-export chains and `export *` to
//!   a symbol in another module ([`Binding::Sym`]), a property of a CommonJS module's
//!   exports, a namespace object, an external import, or `undefined`.
//! - **Tree shaking.** A worklist marks live statements ("parts"): statements with side
//!   effects in included modules, and transitively the declarations of every symbol a live
//!   part uses. Modules whose package says `"sideEffects": false` are only included when
//!   one of their exports is used.

use rustc_hash::{FxHashMap, FxHashSet};

use crate::graph::{Graph, ModuleId, Target};
use crate::meta::*;
use crate::options::{Format, Loader};
use crate::{Message, Severity};

/// How a module's exports are represented.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Esm,
    Cjs,
    /// CSS (no JavaScript).
    Css,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Wrap {
    None,
    /// `var require_x = __tsrCjs(function (exports, module) { ... })`
    Cjs,
    /// `var init_x = __tsrEsm(() => { ... })`
    Esm,
}

/// What an import refers to after linking.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Binding {
    /// A module-scope symbol of an ESM module.
    Sym(ModuleId, Sym),
    /// Property `.1` of a CommonJS module's exports seen through `__tsrToEsm` (the
    /// importer's `import_x` variable); `None` = the whole object.
    Cjs(ModuleId, Option<String>),
    /// The namespace object of an ESM module.
    Ns(ModuleId),
    /// A property of an ESM module's namespace object (names re-exported from CommonJS or
    /// external modules through `export *`).
    NsProp(ModuleId, String),
    /// An import from an external module (index into [`Link::externals`]).
    External(u32, Imported),
    Undefined,
}

/// Linking result. Indexed by module id.
#[derive(Debug, Default)]
pub struct Link {
    pub kinds: Vec<Kind>,
    pub wrap: Vec<Wrap>,
    pub included: Vec<bool>,
    pub live: Vec<Vec<bool>>,
    /// The module's namespace object (`x_exports`) is needed.
    pub needs_ns: Vec<bool>,
    /// Import bindings: (module, import symbol) -> binding.
    pub bindings: Vec<FxHashMap<Sym, Binding>>,
    /// `ns.prop` reads of namespace imports that resolve statically.
    pub ns_members: Vec<FxHashMap<(Sym, String), Binding>>,
    /// `export * from` CommonJS / external modules, per module (re-exported at run time
    /// through the namespace object).
    pub dynamic_stars: Vec<Vec<Target>>,
    /// Resolved exports (own and through `export *`), for modules with a namespace object
    /// and entry points.
    pub exports: Vec<Option<Vec<(String, Binding)>>>,
    /// External module paths.
    pub externals: Vec<String>,
    /// Externals imported only for side effects (`import "x"`), per module.
    pub side_effect_externals: Vec<Vec<u32>>,
    /// `__tsrToEsm` node mode per module (`.mjs`/`.mts` or `"type": "module"` importer).
    pub node_mode: Vec<bool>,
    pub messages: Vec<Message>,
}

/// Linker settings.
#[derive(Clone, Debug)]
pub struct LinkOptions {
    pub format: Format,
    pub tree_shaking: bool,
    /// Dynamic `import()` targets become their own entry chunks.
    pub splitting: bool,
    /// The entries' exports are observable (ESM/CJS output, or IIFE with a global name).
    pub keep_entry_exports: bool,
}

/// Modules (and the export names, interned by [`Linker::name_id`]) on a resolution path.
type Seen = FxHashSet<(ModuleId, u32)>;

#[derive(Debug)]
enum Task {
    Include(ModuleId),
    Part(ModuleId, u32),
    Sym(ModuleId, Sym),
    Binding(Binding),
    AllExports(ModuleId),
}

struct Linker<'g> {
    g: &'g Graph,
    o: LinkOptions,
    l: Link,
    ext_ids: FxHashMap<String, u32>,
    /// sym -> declaring parts, per module.
    decl_parts: Vec<FxHashMap<Sym, Vec<u32>>>,
    /// Import symbol -> binding index in meta.imports, per module.
    import_idx: Vec<FxHashMap<Sym, u32>>,
    work: Vec<Task>,
    all_exports_done: Vec<bool>,
    reported: FxHashSet<(ModuleId, u32, String)>,
    /// Set by `resolve_export` when a name matched several `export *` sources.
    ambiguous: bool,
    /// Finished `resolve_export` results: the binding, and for `None` whether the name was
    /// ambiguous. Without it, modules reached through several `export *` paths are
    /// re-resolved once per path, which is exponential in the depth of such diamonds.
    export_memo: FxHashMap<(ModuleId, u32), (Option<Binding>, bool)>,
    /// Finished `export_exists` results.
    exists_memo: FxHashMap<(ModuleId, u32), bool>,
    /// Export names interned for the memo and path keys (no string copies per lookup).
    name_ids: FxHashMap<String, u32>,
    /// Times a resolution met a module already on its path. A result is only memoized
    /// when this did not change while computing it, because a cycle cut depends on the
    /// path it was reached from.
    cycle_hits: u64,
}

/// Links `g` for the user entry modules and (with code splitting) the dynamic-import
/// targets that become entries.
pub fn link(g: &Graph, entries: &[ModuleId], dynamic_entries: &[ModuleId], o: LinkOptions) -> Link {
    let n = g.modules.len();
    let mut lk = Linker {
        g,
        o,
        l: Link {
            kinds: vec![Kind::Esm; n],
            wrap: vec![Wrap::None; n],
            included: vec![false; n],
            live: g
                .modules
                .iter()
                .map(|m| vec![false; m.meta.parts.len()])
                .collect(),
            needs_ns: vec![false; n],
            bindings: vec![FxHashMap::default(); n],
            ns_members: vec![FxHashMap::default(); n],
            dynamic_stars: vec![Vec::new(); n],
            exports: vec![None; n],
            externals: Vec::new(),
            side_effect_externals: vec![Vec::new(); n],
            node_mode: g.modules.iter().map(|m| m.node_mode).collect(),
            messages: Vec::new(),
        },
        ext_ids: FxHashMap::default(),
        decl_parts: Vec::with_capacity(n),
        import_idx: Vec::with_capacity(n),
        work: Vec::new(),
        all_exports_done: vec![false; n],
        reported: FxHashSet::default(),
        ambiguous: false,
        export_memo: FxHashMap::default(),
        exists_memo: FxHashMap::default(),
        name_ids: FxHashMap::default(),
        cycle_hits: 0,
    };
    for m in &g.modules {
        let mut d: FxHashMap<Sym, Vec<u32>> = FxHashMap::default();
        for (i, p) in m.meta.parts.iter().enumerate() {
            for &s in &p.declares {
                d.entry(s).or_default().push(i as u32);
            }
        }
        lk.decl_parts.push(d);
        lk.import_idx.push(
            m.meta
                .imports
                .iter()
                .enumerate()
                .map(|(i, b)| (b.sym, i as u32))
                .collect(),
        );
    }
    lk.compute_kinds();
    lk.compute_wraps();
    // `require()` of an external may become an import of it (`require_to_import`).
    for m in &g.modules {
        for (r, t) in m.meta.records.iter().zip(&m.targets) {
            if let (RecordKind::Require, Target::External(p)) = (r.kind, t) {
                lk.ext_id(p);
            }
        }
    }
    for &e in entries {
        lk.work.push(Task::Include(e));
        if lk.o.keep_entry_exports {
            lk.work.push(Task::AllExports(e));
            // Without `export` statements there is nothing to hand to `module.exports` (or
            // the IIFE's global): esbuild leaves it alone too.
            if lk.o.format != Format::Esm
                && lk.l.kinds[e as usize] == Kind::Esm
                && g.modules[e as usize].meta.has_esm_exports
            {
                lk.l.needs_ns[e as usize] = true;
            }
        }
    }
    for &e in dynamic_entries {
        lk.work.push(Task::Include(e));
        lk.work.push(Task::AllExports(e));
    }
    lk.run();
    // Named imports must exist even when unused (esbuild reports those too).
    for m in 0..n {
        if !lk.l.included[m] {
            continue;
        }
        let module = &g.modules[m];
        for imp in &module.meta.imports {
            let Imported::Name(name) = &imp.name else {
                continue;
            };
            if let Some(Target::Module(t)) = module.targets.get(imp.record as usize)
                && !lk.export_exists(*t, name, &mut FxHashSet::default())
            {
                lk.report_missing(m as ModuleId, imp.record, name, *t);
            }
        }
    }
    // Namespace objects need every export resolved.
    for m in 0..n {
        if lk.l.needs_ns[m] && lk.l.exports[m].is_none() {
            let ex = lk.resolve_all_exports(m as ModuleId);
            lk.l.exports[m] = Some(ex);
        }
    }
    for &e in entries.iter().chain(dynamic_entries) {
        if lk.l.exports[e as usize].is_none() {
            let ex = lk.resolve_all_exports(e);
            lk.l.exports[e as usize] = Some(ex);
        }
    }
    lk.l
}

impl Linker<'_> {
    fn compute_kinds(&mut self) {
        let g = self.g;
        let mut required = vec![false; g.modules.len()];
        for m in &g.modules {
            for (i, r) in m.meta.records.iter().enumerate() {
                if r.kind == RecordKind::Require
                    && let Target::Module(t) = m.targets[i]
                {
                    required[t as usize] = true;
                }
            }
        }
        for (i, m) in g.modules.iter().enumerate() {
            let meta = &m.meta;
            self.l.kinds[i] = if m.loader == Loader::Css {
                Kind::Css
            } else if meta.has_esm_exports {
                Kind::Esm
            } else if meta.uses_cjs {
                Kind::Cjs
            } else if meta.has_esm_syntax {
                Kind::Esm
            } else if required[i] || m.loader == Loader::Empty {
                Kind::Cjs
            } else {
                Kind::Esm
            };
        }
    }

    fn compute_wraps(&mut self) {
        let g = self.g;
        let n = g.modules.len();
        let mut stack: Vec<ModuleId> = Vec::new();
        for i in 0..n {
            if self.l.kinds[i] == Kind::Cjs {
                self.l.wrap[i] = Wrap::Cjs;
            }
        }
        for m in &g.modules {
            for (i, r) in m.meta.records.iter().enumerate() {
                let Target::Module(t) = m.targets[i] else {
                    continue;
                };
                let lazy = match r.kind {
                    RecordKind::Require => true,
                    RecordKind::Dynamic => !self.o.splitting,
                    _ => false,
                };
                if lazy && self.l.kinds[t as usize] == Kind::Esm {
                    stack.push(t);
                }
            }
        }
        while let Some(t) = stack.pop() {
            let ti = t as usize;
            if self.l.wrap[ti] != Wrap::None {
                continue;
            }
            // Only modules that are `require()`d or imported dynamically need a namespace
            // object; `mark_part` asks for it when such a record is live.
            self.l.wrap[ti] = Wrap::Esm;
            let m = &g.modules[ti];
            if m.meta.has_tla {
                self.l.messages.push(Message::new(
                    Severity::Error,
                    Some(m.pretty.clone()),
                    format!(
                        "This require call is not allowed because the imported file {:?} contains a top-level await",
                        m.pretty
                    ),
                ));
            }
            for (i, r) in m.meta.records.iter().enumerate() {
                if matches!(r.kind, RecordKind::Import | RecordKind::ReExport)
                    && let Target::Module(u) = m.targets[i]
                    && self.l.kinds[u as usize] == Kind::Esm
                {
                    stack.push(u);
                }
            }
        }
    }

    fn ext_id(&mut self, path: &str) -> u32 {
        if let Some(&i) = self.ext_ids.get(path) {
            return i;
        }
        let i = self.l.externals.len() as u32;
        self.l.externals.push(path.to_owned());
        self.ext_ids.insert(path.to_owned(), i);
        i
    }

    // ---- binding resolution ----

    /// The binding of import symbol `sym` in module `m` (memoized).
    fn binding(&mut self, m: ModuleId, sym: Sym) -> Binding {
        if let Some(b) = self.l.bindings[m as usize].get(&sym) {
            return b.clone();
        }
        let Some(&idx) = self.import_idx[m as usize].get(&sym) else {
            return Binding::Undefined;
        };
        let imp = self.g.modules[m as usize].meta.imports[idx as usize].clone();
        let mut seen = FxHashSet::default();
        let b = self.resolve_import(m, imp.record, &imp.name, &mut seen);
        self.l.bindings[m as usize].insert(sym, b.clone());
        b
    }

    fn resolve_import(
        &mut self,
        m: ModuleId,
        record: u32,
        name: &Imported,
        seen: &mut Seen,
    ) -> Binding {
        let module = &self.g.modules[m as usize];
        let Some(target) = module.targets.get(record as usize).cloned() else {
            return Binding::Undefined;
        };
        match target {
            Target::Module(t) => match self.l.kinds[t as usize] {
                Kind::Cjs => match name {
                    Imported::Namespace => Binding::Cjs(t, None),
                    Imported::Name(n)
                        if n != "default"
                            && json_keys(&self.g.modules[t as usize])
                                .is_some_and(|keys| !keys.contains(n)) =>
                    {
                        self.report_missing(m, record, n, t);
                        Binding::Undefined
                    }
                    Imported::Name(n) => Binding::Cjs(t, Some(n.clone())),
                },
                Kind::Css => Binding::Undefined,
                Kind::Esm => match name {
                    Imported::Namespace => Binding::Ns(t),
                    Imported::Name(n) => {
                        self.ambiguous = false;
                        let r = self.resolve_export(t, n, seen);
                        match r {
                            Some(b) => b,
                            None => {
                                if self.ambiguous {
                                    self.report_ambiguous(m, record, n);
                                } else {
                                    self.report_missing(m, record, n, t);
                                }
                                Binding::Undefined
                            }
                        }
                    }
                },
            },
            Target::External(p) => {
                let id = self.ext_id(&p);
                Binding::External(id, name.clone())
            }
            Target::Missing => Binding::Undefined,
        }
    }

    fn report_ambiguous(&mut self, m: ModuleId, record: u32, name: &str) {
        if !self.reported.insert((m, record, name.to_owned())) {
            return;
        }
        let module = &self.g.modules[m as usize];
        let ts = matches!(module.loader, Loader::Ts | Loader::Tsx);
        let r = &module.meta.records[record as usize];
        let (sev, text) = if ts {
            (
                Severity::Warning,
                format!(
                    "Import {name:?} will always be undefined because there are multiple matching exports"
                ),
            )
        } else {
            (
                Severity::Error,
                format!("Ambiguous import {name:?} has multiple matching exports"),
            )
        };
        self.l.messages.push(module.message_at(sev, r.start, text));
    }

    fn report_missing(&mut self, m: ModuleId, record: u32, name: &str, target: ModuleId) {
        let module = &self.g.modules[m as usize];
        let tmod = &self.g.modules[target as usize];
        // TypeScript files may import types that were not elided (isolated modules), and
        // modules without exports yield `undefined` (esbuild warns).
        let ts = matches!(module.loader, Loader::Ts | Loader::Tsx)
            || matches!(tmod.loader, Loader::Ts | Loader::Tsx);
        if ts {
            return;
        }
        if !self.reported.insert((m, record, name.to_owned())) {
            return;
        }
        let r = &module.meta.records[record as usize];
        let no_exports = tmod.loader != Loader::Json
            && tmod.meta.exports.is_empty()
            && tmod.meta.star_exports.is_empty();
        let (sev, text) = if no_exports {
            (
                Severity::Warning,
                format!(
                    "Import {name:?} will always be undefined because the file {:?} has no exports",
                    tmod.pretty
                ),
            )
        } else {
            (
                Severity::Error,
                format!(
                    "No matching export in {:?} for import {name:?}",
                    tmod.pretty
                ),
            )
        };
        self.l.messages.push(module.message_at(sev, r.start, text));
    }

    /// Whether module `t` has export `name`, without including anything (cycles and
    /// dynamic cases count as present; ambiguity is reported where the import is used).
    fn export_exists(&mut self, t: ModuleId, name: &str, seen: &mut Seen) -> bool {
        let key = (t, self.name_id(name));
        if !seen.insert(key) {
            self.cycle_hits += 1;
            return true;
        }
        if let Some(&r) = self.exists_memo.get(&key) {
            return r;
        }
        let cycles = self.cycle_hits;
        let r = self.export_exists_uncached(t, name, seen);
        if self.cycle_hits == cycles {
            self.exists_memo.insert(key, r);
        }
        r
    }

    fn export_exists_uncached(&mut self, t: ModuleId, name: &str, seen: &mut Seen) -> bool {
        let g = self.g;
        let module = &g.modules[t as usize];
        match self.l.kinds[t as usize] {
            Kind::Css => return true,
            Kind::Cjs => {
                return name == "default"
                    || json_keys(module).is_none_or(|keys| keys.iter().any(|k| k == name));
            }
            Kind::Esm => {}
        }
        let meta = &module.meta;
        let through = |this: &mut Self, record: u32, imported: &Imported, seen: &mut Seen| match (
            module.targets.get(record as usize),
            imported,
        ) {
            (Some(Target::Module(u)), Imported::Name(n)) => this.export_exists(*u, n, seen),
            _ => true,
        };
        if let Some(e) = meta.export(name) {
            return match e {
                ExportTarget::Local(sym) => match self.import_idx[t as usize].get(sym) {
                    Some(&idx) => {
                        let imp = &meta.imports[idx as usize];
                        through(self, imp.record, &imp.name, seen)
                    }
                    None => true,
                },
                ExportTarget::Reexport { record, name } => through(self, *record, name, seen),
            };
        }
        if name == "default" {
            return false;
        }
        meta.star_exports
            .iter()
            .any(|&r| match module.targets.get(r as usize) {
                Some(Target::Module(u)) => match self.l.kinds[*u as usize] {
                    Kind::Esm => self.export_exists(*u, name, &mut seen.clone()),
                    Kind::Cjs => true,
                    Kind::Css => false,
                },
                Some(Target::External(_)) => true,
                _ => false,
            })
    }

    fn name_id(&mut self, name: &str) -> u32 {
        if let Some(&id) = self.name_ids.get(name) {
            return id;
        }
        let id = self.name_ids.len() as u32;
        self.name_ids.insert(name.to_owned(), id);
        id
    }

    /// Resolves export `name` of ESM module `t`. `None` when it doesn't exist.
    fn resolve_export(&mut self, t: ModuleId, name: &str, seen: &mut Seen) -> Option<Binding> {
        let key = (t, self.name_id(name));
        if !seen.insert(key) {
            self.cycle_hits += 1;
            return Some(Binding::Undefined);
        }
        if let Some((b, ambiguous)) = self.export_memo.get(&key).cloned() {
            // The modules below `t` were queued for inclusion when this was computed.
            self.work.push(Task::Include(t));
            if b.is_none() {
                self.ambiguous = ambiguous;
            }
            return b;
        }
        let cycles = self.cycle_hits;
        let b = self.resolve_export_uncached(t, name, seen);
        if self.cycle_hits == cycles {
            let ambiguous = b.is_none() && self.ambiguous;
            self.export_memo.insert(key, (b.clone(), ambiguous));
        }
        b
    }

    fn resolve_export_uncached(
        &mut self,
        t: ModuleId,
        name: &str,
        seen: &mut Seen,
    ) -> Option<Binding> {
        // Modules an import passes through are part of the bundle (esbuild keeps the side
        // effects of re-exporting files even in `sideEffects: false` packages).
        self.work.push(Task::Include(t));
        let meta = &self.g.modules[t as usize].meta;
        if self.l.kinds[t as usize] == Kind::Cjs {
            return Some(Binding::Cjs(t, Some(name.to_owned())));
        }
        if let Some(e) = meta.export(name).cloned() {
            return Some(match e {
                ExportTarget::Local(sym) => {
                    if let Some(&idx) = self.import_idx[t as usize].get(&sym) {
                        let imp = meta.imports[idx as usize].clone();
                        self.resolve_import(t, imp.record, &imp.name, seen)
                    } else {
                        Binding::Sym(t, sym)
                    }
                }
                ExportTarget::Reexport { record, name } => {
                    self.resolve_import(t, record, &name, seen)
                }
            });
        }
        if name == "default" {
            return None;
        }
        let stars = meta.star_exports.clone();
        let mut dynamic = false;
        let mut found: Option<Binding> = None;
        // A branch that only matches through a nested `export *` of CommonJS or an external
        // (`NsProp`) is a maybe: a static match elsewhere wins over it, as in esbuild (langium:
        // `export *` of its own `inject` and of a module that `export *`s a CommonJS file).
        // Two different maybes and no static match: either may supply the name at run time,
        // so picking one would be wrong code; it is ambiguous, as before maybes existed.
        let mut maybe: Option<Binding> = None;
        let mut maybes_differ = false;
        for r in stars {
            let target = self.g.modules[t as usize].targets[r as usize].clone();
            match target {
                Target::Module(u) => match self.l.kinds[u as usize] {
                    Kind::Esm => {
                        let mut branch = seen.clone();
                        let b = self.resolve_export(u, name, &mut branch);
                        if let Some(b @ Binding::NsProp(..)) = b {
                            match &maybe {
                                None => maybe = Some(b),
                                Some(m) => maybes_differ |= *m != b,
                            }
                        } else if let Some(b) = b {
                            match &found {
                                None => found = Some(b),
                                Some(f) if *f == b => {}
                                Some(_) => {
                                    self.ambiguous = true;
                                    return None;
                                }
                            }
                        } else if self.ambiguous {
                            return None;
                        }
                    }
                    Kind::Cjs => dynamic = true,
                    Kind::Css => {}
                },
                Target::External(_) => dynamic = true,
                Target::Missing => {}
            }
        }
        if found.is_some() {
            return found;
        }
        if maybes_differ {
            self.ambiguous = true;
            return None;
        }
        if maybe.is_some() {
            return maybe;
        }
        if dynamic {
            self.l.needs_ns[t as usize] = true;
            return Some(Binding::NsProp(t, name.to_owned()));
        }
        None
    }

    /// Every export of ESM module `m` (own, then `export *` names not shadowed), sorted.
    fn resolve_all_exports(&mut self, m: ModuleId) -> Vec<(String, Binding)> {
        if self.l.kinds[m as usize] != Kind::Esm {
            return Vec::new();
        }
        let mut names: Vec<String> = Vec::new();
        let mut seen_names: FxHashSet<String> = FxHashSet::default();
        let mut visited: FxHashSet<ModuleId> = FxHashSet::default();
        let mut dyn_stars: Vec<Target> = Vec::new();
        self.collect_export_names(
            m,
            true,
            &mut names,
            &mut seen_names,
            &mut visited,
            &mut dyn_stars,
        );
        if !dyn_stars.is_empty() {
            self.l.needs_ns[m as usize] = true;
        }
        self.l.dynamic_stars[m as usize] = dyn_stars;
        names.sort();
        let mut out = Vec::with_capacity(names.len());
        for n in names {
            let mut seen = FxHashSet::default();
            self.ambiguous = false;
            if let Some(b) = self.resolve_export(m, &n, &mut seen) {
                out.push((n, b));
            }
        }
        out
    }

    fn collect_export_names(
        &mut self,
        m: ModuleId,
        top: bool,
        names: &mut Vec<String>,
        seen: &mut FxHashSet<String>,
        visited: &mut FxHashSet<ModuleId>,
        dyn_stars: &mut Vec<Target>,
    ) {
        if !visited.insert(m) {
            return;
        }
        let module = &self.g.modules[m as usize];
        for (n, _) in &module.meta.exports {
            if (top || n != "default") && seen.insert(n.clone()) {
                names.push(n.clone());
            }
        }
        // CommonJS and external `export *` sources are copied at run time, at any depth: a
        // barrel's `export * from './dep.cjs'` reaches the module that `export *`s the barrel.
        // (The copy skips names already defined, so static exports keep precedence.)
        for &r in &module.meta.star_exports {
            match &module.targets[r as usize] {
                Target::Module(u) => match self.l.kinds[*u as usize] {
                    Kind::Esm => {
                        self.collect_export_names(*u, false, names, seen, visited, dyn_stars)
                    }
                    Kind::Cjs => {
                        if !dyn_stars.contains(&Target::Module(*u)) {
                            dyn_stars.push(Target::Module(*u));
                        }
                    }
                    Kind::Css => {}
                },
                Target::External(p) => {
                    let p = p.clone();
                    self.ext_id(&p);
                    if !dyn_stars.contains(&Target::External(p.clone())) {
                        dyn_stars.push(Target::External(p));
                    }
                }
                Target::Missing => {}
            }
        }
    }

    // ---- tree shaking ----

    fn run(&mut self) {
        while let Some(t) = self.work.pop() {
            match t {
                Task::Include(m) => self.include(m),
                Task::Part(m, p) => self.mark_part(m, p),
                Task::Sym(m, s) => self.mark_sym(m, s),
                Task::Binding(b) => self.mark_binding(b),
                Task::AllExports(m) => self.mark_all_exports(m),
            }
        }
    }

    fn include(&mut self, m: ModuleId) {
        let mi = m as usize;
        if self.l.included[mi] {
            return;
        }
        self.l.included[mi] = true;
        let module = &self.g.modules[mi];
        // ES modules are tree-shaken whether or not they are wrapped in `init_x()`, unless
        // a direct `eval` can name any of their bindings.
        let all =
            !self.o.tree_shaking || self.l.kinds[mi] != Kind::Esm || module.meta.has_direct_eval;
        for (i, p) in module.meta.parts.iter().enumerate() {
            // `sideEffects: false` only lets an unused file be dropped; once a file is in
            // the bundle its side effects run.
            if all || p.side_effects {
                self.work.push(Task::Part(m, i as u32));
            }
        }
        // Static imports run their targets' side effects.
        for (i, r) in module.meta.records.iter().enumerate() {
            if !matches!(r.kind, RecordKind::Import | RecordKind::ReExport) {
                continue;
            }
            match &module.targets[i] {
                Target::Module(t) => {
                    let tm = &self.g.modules[*t as usize];
                    if tm.side_effects || !self.o.tree_shaking {
                        self.work.push(Task::Include(*t));
                    }
                }
                Target::External(p) => {
                    let p = p.clone();
                    let id = self.ext_id(&p);
                    if !self.l.side_effect_externals[mi].contains(&id) {
                        self.l.side_effect_externals[mi].push(id);
                    }
                }
                Target::Missing => {}
            }
        }
    }

    fn mark_part(&mut self, m: ModuleId, p: u32) {
        let mi = m as usize;
        let live = &mut self.l.live[mi][p as usize];
        if *live {
            return;
        }
        *live = true;
        let g = self.g;
        let part = &g.modules[mi].meta.parts[p as usize];
        for &s in &part.uses {
            self.work.push(Task::Sym(m, s));
        }
        for (s, prop) in &part.ns_members {
            let b = self.binding(m, *s);
            match b {
                Binding::Ns(t) => {
                    let mut seen = FxHashSet::default();
                    self.ambiguous = false;
                    let r = self.resolve_export(t, prop, &mut seen);
                    let r = match r {
                        Some(r) => r,
                        None => {
                            // `ns.missing` is `undefined` at run time; not an error.
                            Binding::Undefined
                        }
                    };
                    self.l.ns_members[mi].insert((*s, prop.clone()), r.clone());
                    self.work.push(Task::Include(t));
                    self.work.push(Task::Binding(r));
                }
                other => self.work.push(Task::Binding(other)),
            }
        }
        for &r in &part.records {
            let rec = &g.modules[mi].meta.records[r as usize];
            if let Target::Module(t) = g.modules[mi].targets[r as usize] {
                self.work.push(Task::Include(t));
                let lazy_esm = self.l.kinds[t as usize] == Kind::Esm
                    && (rec.kind == RecordKind::Require
                        || (rec.kind == RecordKind::Dynamic && !self.o.splitting));
                let entry_esm = self.l.kinds[t as usize] == Kind::Esm
                    && rec.kind == RecordKind::Dynamic
                    && self.o.splitting;
                if lazy_esm || entry_esm {
                    self.l.needs_ns[t as usize] = lazy_esm || self.l.needs_ns[t as usize];
                    self.work.push(Task::AllExports(t));
                }
            }
        }
    }

    fn mark_sym(&mut self, m: ModuleId, s: Sym) {
        let mi = m as usize;
        if self.import_idx[mi].contains_key(&s) {
            let b = self.binding(m, s);
            self.mark_binding(b);
            return;
        }
        if let Some(parts) = self.decl_parts[mi].get(&s) {
            for &p in parts {
                if !self.l.live[mi][p as usize] {
                    self.work.push(Task::Part(m, p));
                }
            }
        }
    }

    fn mark_binding(&mut self, b: Binding) {
        match b {
            Binding::Sym(t, s) => {
                self.work.push(Task::Include(t));
                self.mark_sym(t, s);
            }
            Binding::Cjs(t, _) => self.work.push(Task::Include(t)),
            Binding::Ns(t) | Binding::NsProp(t, _) => {
                self.l.needs_ns[t as usize] = true;
                self.work.push(Task::Include(t));
                self.work.push(Task::AllExports(t));
            }
            Binding::External(..) | Binding::Undefined => {}
        }
    }

    fn mark_all_exports(&mut self, m: ModuleId) {
        let mi = m as usize;
        if self.all_exports_done[mi] {
            return;
        }
        self.all_exports_done[mi] = true;
        self.work.push(Task::Include(m));
        if self.l.kinds[mi] != Kind::Esm {
            return;
        }
        let ex = self.resolve_all_exports(m);
        for (_, b) in &ex {
            self.work.push(Task::Binding(b.clone()));
        }
        self.l.exports[mi] = Some(ex);
    }
}

/// Top-level keys of a JSON module whose value is an object (`None` for other modules).
fn json_keys(module: &crate::graph::Module) -> Option<Vec<String>> {
    if module.loader != Loader::Json {
        return None;
    }
    // The JSON loader's source is `module.exports = (\n<json>\n);`.
    let js = &module.source.js;
    let body = js
        .strip_prefix("module.exports = (")?
        .trim_end()
        .strip_suffix(");")?;
    let parsed = ts_r_config::jsonc::parse(body, &module.path);
    let members = parsed.root?.as_object()?.to_vec();
    Some(members.into_iter().map(|m| m.key).collect())
}
