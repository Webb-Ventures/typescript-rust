//! Planning: chunks, module order, names, and one [`ModulePlan`] per emitted module.
//!
//! Names are global to the build (one table for all chunks), so a symbol has the same name
//! in every chunk and cross-chunk imports never need aliases. Each name is the symbol's own
//! name unless taken (then `name2`, `name3`, ...), or the next short name when minifying.
//! Names never collide with globals referenced by any module, keywords, or the CommonJS
//! names (`require`, `module`, `exports`).

use rustc_hash::{FxHashMap, FxHashSet};

use crate::expr::Expr;
use crate::graph::{DISABLED_PREFIX, Graph, ModuleId, Target};
use crate::link::{Binding, Kind, Link, Wrap};
use crate::meta::{Imported, PartKind, RecordKind, Sym, SymKind};
use crate::options::{BuildOptions, Format, Loader, Platform};
use crate::prepare::file_stem_name;
use crate::runtime::{Helper, Helpers};

/// How an import symbol's references are rewritten when it can't simply be renamed.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Rewrite {
    Name(String),
    /// `obj.prop`
    Member(String, String),
    Undefined,
}

impl Rewrite {
    pub fn expr(&self) -> Expr {
        match self {
            Rewrite::Name(n) => Expr::ident(n),
            Rewrite::Member(o, p) => Expr::member(Expr::ident(o), p),
            Rewrite::Undefined => Expr::Undefined,
        }
    }

    fn root(&self) -> Option<&str> {
        match self {
            Rewrite::Name(n) | Rewrite::Member(n, _) => Some(n),
            Rewrite::Undefined => None,
        }
    }
}

/// A statement the bundler adds before a module's code.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum PrologueStmt {
    Expr(Expr),
    /// `var name = expr;`
    Var(String, Expr),
}

/// Everything the printer needs to emit one module. Hashable: the fragment cache key is the
/// prepared module's key plus this plan's hash.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ModulePlan {
    pub wrap: Wrap,
    /// `require_x` / `init_x` (empty when not wrapped).
    pub wrapper_name: String,
    /// Live parts.
    pub live: Vec<bool>,
    /// Fixed names for module-scope symbols (own symbols and renamed imports), by symbol.
    pub names: Vec<(Sym, String)>,
    /// Import symbols whose references become member expressions or `undefined`.
    pub rewrites: Vec<(Sym, Rewrite)>,
    /// `ns.prop` reads of namespace imports.
    pub ns_rewrites: Vec<(Sym, String, Rewrite)>,
    /// `require()` / `import()` replacements, per record (`None` = keep).
    pub records: Vec<Option<Expr>>,
    pub prologue: Vec<PrologueStmt>,
    /// Name for an anonymous `export default`.
    pub default_name: String,
    /// Names the module's rewritten code refers to (inner bindings must not capture them).
    pub reserved: Vec<String>,
    /// Helper names (for wrappers).
    pub helper_cjs: String,
    pub helper_esm: String,
    /// The module runs unwrapped at the top of the file but names its own top-level
    /// bindings, around `top_reserved` (every other top-level name of the file).
    pub own_top_level: bool,
    pub top_reserved: Vec<String>,
    /// Replace `import.meta` with `{}` (non-ESM output).
    pub import_meta_empty: bool,
    pub mangle: bool,
    pub minify_syntax: bool,
    pub compact: bool,
    pub comments: bool,
    /// Source name for the source map ("" = no map).
    pub map_source: String,
}

/// One output chunk.
#[derive(Clone, Debug, Default)]
pub struct Chunk {
    /// The entry module (user or dynamic entry), if this is an entry chunk.
    pub entry: Option<ModuleId>,
    /// Index into the user entry points (`None` for dynamic entries and shared chunks).
    pub user_entry: Option<usize>,
    /// Entries (by index over user then dynamic entries) that reach this chunk's modules.
    pub key: Vec<u32>,
    /// JavaScript modules, in evaluation order.
    pub modules: Vec<ModuleId>,
    /// CSS modules, in order.
    pub css: Vec<ModuleId>,
    /// Output path relative to the output directory (`/`-separated).
    pub file: String,
    /// `(chunk, names)` imported from other chunks.
    pub imports: Vec<(usize, Vec<String>)>,
    /// Names other chunks import from this one.
    pub exports: Vec<String>,
    pub helpers: Helpers,
    /// Namespace objects declared at the top of the chunk: `(name, getters, module)`.
    pub ns_objects: Vec<(String, Vec<(String, Expr)>)>,
    /// External imports (ESM output): `(path, default local, namespace local, [(imported,
    /// local)])`, plus side-effect-only paths.
    pub ext_imports: Vec<ExternalImport>,
    /// `var import_x = __tsrToEsm(require("x"))` (CJS/IIFE output): `(path, var name)`;
    /// name empty = `require("x");` only.
    pub ext_requires: Vec<(String, String)>,
    /// Code after the modules (entry exports).
    pub tail: Vec<String>,
    /// Statement right after the helpers (`module.exports = ...` for CJS output).
    pub head: Vec<String>,
    /// For IIFE with a global name: the expression returned.
    pub iife_return: Option<Expr>,
    /// Add `import { createRequire } from "module"; var require = createRequire(...)`.
    pub require_shim: bool,
    /// Transform preludes (`__decorate` helpers), deduplicated.
    pub preludes: Vec<String>,
    /// `export * from "x"` lines (ESM entry re-exporting externals).
    pub star_reexports: Vec<String>,
    /// Start the file with `"use strict";` (an unwrapped strict CommonJS entry).
    pub use_strict: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExternalImport {
    pub path: String,
    pub default: Option<String>,
    pub namespace: Option<String>,
    pub named: Vec<(String, String)>,
}

#[derive(Debug, Default)]
pub struct Plan {
    pub chunks: Vec<Chunk>,
    pub plans: Vec<Option<ModulePlan>>,
    pub module_chunk: Vec<Option<usize>>,
    pub helper_names: [String; 6],
    pub format: Format,
    /// Output directory (absolute).
    pub outdir: String,
}

/// Global name allocation.
#[derive(Debug)]
struct Names {
    taken: FxHashSet<String>,
    minify: bool,
    cursor: usize,
}

impl Names {
    fn alloc(&mut self, preferred: &str) -> String {
        if self.minify {
            let taken = &self.taken;
            let n = ts_r_minify::rename::next_free_name(&mut self.cursor, &|c| taken.contains(c));
            self.taken.insert(n.clone());
            return n;
        }
        let base = if ts_r_minify::names::is_identifier(preferred)
            && !ts_r_minify::names::is_reserved(preferred)
        {
            preferred.to_owned()
        } else {
            ts_r_minify::names::sanitize(preferred)
        };
        if !self.taken.contains(&base) {
            self.taken.insert(base.clone());
            return base;
        }
        let mut i = 2u32;
        loop {
            let c = format!("{base}{i}");
            if !self.taken.contains(&c) {
                self.taken.insert(c.clone());
                return c;
            }
            i += 1;
        }
    }
}

const RESERVED: &[&str] = &[
    "require",
    "module",
    "exports",
    "__filename",
    "__dirname",
    "arguments",
    "eval",
    "undefined",
    "NaN",
    "Infinity",
    "globalThis",
    "Promise",
    "Object",
    "WeakMap",
    "Symbol",
    "import",
    // Transform helpers (`ts_r_transform` preludes).
    "__createBinding",
    "__setModuleDefault",
    "__importStar",
    "__exportStar",
    "__importDefault",
    "__decorate",
    "__metadata",
    "__param",
    "__runInitializers",
    "__esDecorate",
    "__setFunctionName",
    "__propKey",
    "__rewriteRelativeImportExtension",
    "__tsrCreateRequire",
];

/// Inputs to [`plan`].
#[derive(Debug)]
pub struct PlanInput<'a> {
    pub graph: &'a Graph,
    pub link: &'a Link,
    pub opts: &'a BuildOptions,
    /// User entries (module ids), in entry-point order.
    pub entries: &'a [ModuleId],
    /// Dynamic-import entries (splitting only).
    pub dynamic_entries: &'a [ModuleId],
    /// Absolute output directory.
    pub outdir: String,
    /// Output file names for user entries (relative to `outdir`, with extension).
    pub entry_files: Vec<String>,
    /// Output names for dynamic entries.
    pub dynamic_files: Vec<String>,
}

struct Planner<'a> {
    g: &'a Graph,
    l: &'a Link,
    o: &'a BuildOptions,
    format: Format,
    names: Names,
    sym_names: Vec<FxHashMap<Sym, String>>,
    wrapper: Vec<String>,
    ns_name: Vec<String>,
    default_name: Vec<String>,
    /// (importer, CommonJS module) -> `import_x` name.
    import_vars: FxHashMap<(ModuleId, ModuleId), String>,
    /// External id -> CommonJS `import_x` var / ESM locals.
    ext_var: FxHashMap<u32, String>,
    ext_locals: FxHashMap<(u32, Imported), String>,
    /// name -> owning module (for cross-chunk imports).
    owner: FxHashMap<String, ModuleId>,
    helper_names: [String; 6],
    /// A CommonJS entry printed at the top level of a CommonJS chunk (see [`unwrapped_entry`]).
    unwrapped: Option<ModuleId>,
}

/// The CommonJS entry that can run directly at the top of a CommonJS output file, with
/// Node's own `module`, `exports` and `require`, instead of through a `require_x` wrapper:
/// one entry, nothing requires it (no cycle back to it), no top-level `return` or `arguments`,
/// no direct `eval`, and its `"use strict"` (written first in the file) suits every other
/// module of the bundle. It is the last module, where the wrapper call used to run.
fn unwrapped_entry(input: &PlanInput<'_>, format: Format) -> Option<ModuleId> {
    let (g, l, o) = (input.graph, input.link, input.opts);
    let [e] = *input.entries else {
        return None;
    };
    let ei = e as usize;
    if format != Format::Cjs
        || o.splitting
        || !input.dynamic_entries.is_empty()
        || l.kinds[ei] != Kind::Cjs
        || l.wrap[ei] != Wrap::Cjs
    {
        return None;
    }
    let meta = &g.modules[ei].meta;
    if meta.needs_function || meta.has_direct_eval || meta.use_strict && !strict_file_ok(input, e) {
        return None;
    }
    for (i, m) in g.modules.iter().enumerate() {
        if l.included[i] && m.targets.contains(&Target::Module(e)) {
            return None;
        }
    }
    // Only when it is smaller: the wrapper (`var a=b((c,d)=>{`, `});`, `module.exports=a();`)
    // and, when no other module needs it, the CommonJS helper go; the free `module` and
    // `exports` references lose their one-letter parameter names.
    let helper_elsewhere =
        (0..g.modules.len()).any(|i| i != ei && l.included[i] && l.wrap[i] == Wrap::Cjs);
    let saved = 38 + if helper_elsewhere { 0 } else { 75 };
    (meta.cjs_ref_bytes < saved).then_some(e)
}

/// Whether a `"use strict"` at the top of a CommonJS output file (the entry's directive,
/// which applies to the whole file) suits every other module: ES modules are strict code
/// already, CommonJS modules need their own directive, and non-JavaScript loaders print
/// strict-safe code. A banner would end the directive prologue.
fn strict_file_ok(input: &PlanInput<'_>, e: ModuleId) -> bool {
    let (g, l) = (input.graph, input.link);
    input.opts.banner.is_none()
        && g.modules.iter().enumerate().all(|(i, m)| {
            !l.included[i]
                || i == e as usize
                || m.meta.has_esm_syntax
                || m.meta.use_strict
                || !matches!(
                    m.loader,
                    Loader::Js | Loader::Jsx | Loader::Ts | Loader::Tsx
                )
        })
}

/// Global evaluation order: depth-first post-order from the entries over every record.
pub fn module_order(g: &Graph, link: &Link, roots: &[ModuleId]) -> Vec<ModuleId> {
    let n = g.modules.len();
    let mut visited = vec![false; n];
    let mut order = Vec::new();
    for &r in roots {
        if visited[r as usize] {
            continue;
        }
        // (module, next record index)
        let mut stack: Vec<(ModuleId, usize)> = vec![(r, 0)];
        visited[r as usize] = true;
        while let Some(&mut (m, ref mut i)) = stack.last_mut() {
            let targets = &g.modules[m as usize].targets;
            if *i < targets.len() {
                let t = &targets[*i];
                *i += 1;
                if let Target::Module(u) = t
                    && !visited[*u as usize]
                    && link.included[*u as usize]
                {
                    visited[*u as usize] = true;
                    stack.push((*u, 0));
                }
            } else {
                order.push(m);
                stack.pop();
            }
        }
    }
    order
}

/// Builds the plan.
pub fn plan(input: &PlanInput<'_>) -> Plan {
    let g = input.graph;
    let l = input.link;
    let o = input.opts;
    let format = o.effective_format();
    let n = g.modules.len();
    let mut taken: FxHashSet<String> = RESERVED.iter().map(|s| (*s).to_owned()).collect();
    for (i, m) in g.modules.iter().enumerate() {
        if l.included[i] {
            taken.extend(m.meta.globals.iter().cloned());
        }
    }
    let mut p = Planner {
        g,
        l,
        o,
        format,
        names: Names {
            taken,
            minify: o.minify_identifiers,
            cursor: 0,
        },
        sym_names: vec![FxHashMap::default(); n],
        wrapper: vec![String::new(); n],
        ns_name: vec![String::new(); n],
        default_name: vec![String::new(); n],
        import_vars: FxHashMap::default(),
        ext_var: FxHashMap::default(),
        ext_locals: FxHashMap::default(),
        owner: FxHashMap::default(),
        helper_names: Default::default(),
        unwrapped: unwrapped_entry(input, format),
    };
    for h in Helper::ALL {
        p.helper_names[h as usize] = p.names.alloc(h.name());
    }
    let roots: Vec<ModuleId> = input
        .entries
        .iter()
        .chain(input.dynamic_entries)
        .copied()
        .collect();
    let order = module_order(g, l, &roots);
    for &m in &order {
        p.name_module(m);
    }
    // Chunks.
    let chunks = p.make_chunks(input, &order);
    let mut module_chunk = vec![None; n];
    for (ci, c) in chunks.iter().enumerate() {
        for &m in c.modules.iter().chain(&c.css) {
            module_chunk[m as usize] = Some(ci);
        }
    }
    let mut out = Plan {
        chunks,
        plans: vec![None; n],
        module_chunk,
        helper_names: p.helper_names.clone(),
        format,
        outdir: input.outdir.clone(),
    };
    // Module plans and chunk-level code.
    let mut chunk_refs: Vec<FxHashSet<String>> = vec![FxHashSet::default(); out.chunks.len()];
    #[allow(clippy::needless_range_loop)] // `out.chunks` is mutated in the body.
    for ci in 0..out.chunks.len() {
        let mods = out.chunks[ci].modules.clone();
        let mut helpers = Helpers::with_names(p.helper_names.clone());
        let mut refs: FxHashSet<String> = FxHashSet::default();
        let mut ext_used: Vec<(u32, Imported)> = Vec::new();
        let mut ext_side: Vec<u32> = Vec::new();
        let mut preludes: Vec<String> = Vec::new();
        let mut require_shim = false;
        for &m in &mods {
            let mp = p.module_plan(
                m,
                ci,
                &out,
                &mut helpers,
                &mut refs,
                &mut ext_used,
                &mut require_shim,
            );
            for &e in &l.side_effect_externals[m as usize] {
                if !ext_side.contains(&e) {
                    ext_side.push(e);
                }
            }
            if let Some(prep) = &g.modules[m as usize].prepared
                && !prep.prelude.is_empty()
                && !preludes.contains(&prep.prelude)
            {
                preludes.push(prep.prelude.clone());
            }
            out.plans[m as usize] = Some(mp);
        }
        // Namespace objects of this chunk's modules.
        let mut ns_objects = Vec::new();
        for &m in &mods {
            let mi = m as usize;
            if !l.needs_ns[mi] || l.kinds[mi] != Kind::Esm {
                continue;
            }
            helpers.mark(Helper::Export);
            let mut getters = Vec::new();
            for (name, b) in l.exports[mi].iter().flatten() {
                let r = p.rewrite_for(m, b, &mut refs, &mut helpers, &mut ext_used);
                getters.push((name.clone(), Expr::Arrow(Box::new(r.expr()))));
            }
            ns_objects.push((p.ns_name[mi].clone(), getters));
        }
        // Entry code.
        let c = &out.chunks[ci];
        let mut head = Vec::new();
        let mut tail = Vec::new();
        let mut iife_return = None;
        let mut star_reexports = Vec::new();
        let compact = o.minify_whitespace;
        let sp = if compact { "" } else { " " };
        if let Some(e) = c.entry {
            let ei = e as usize;
            let is_user = c.user_entry.is_some();
            let keep = !is_user || format != Format::Iife || o.global_name.is_some();
            match (l.kinds[ei], l.wrap[ei]) {
                (Kind::Cjs, _) if p.unwrapped == Some(e) => {
                    out.chunks[ci].use_strict = g.modules[ei].meta.use_strict;
                }
                (Kind::Cjs, _) => {
                    let call = Expr::call(&p.wrapper[ei], vec![]);
                    refs.insert(p.wrapper[ei].clone());
                    match format {
                        Format::Esm => {
                            tail.push(format!("export default {};", call.to_text(compact)))
                        }
                        Format::Cjs => {
                            tail.push(format!("module.exports{sp}={sp}{};", call.to_text(compact)))
                        }
                        Format::Iife => {
                            if keep && o.global_name.is_some() {
                                iife_return = Some(call);
                            } else {
                                tail.push(format!("{};", call.to_text(compact)));
                            }
                        }
                    }
                }
                (Kind::Esm, wrap) => {
                    if format == Format::Cjs
                        && wrap == Wrap::None
                        && is_user
                        && g.modules[ei].meta.use_strict
                        && input.entries.len() == 1
                        && strict_file_ok(input, e)
                    {
                        out.chunks[ci].use_strict = true;
                    }
                    if wrap == Wrap::Esm {
                        refs.insert(p.wrapper[ei].clone());
                        tail.push(format!("{}();", p.wrapper[ei]));
                    }
                    if keep && (format == Format::Esm || l.needs_ns[ei]) {
                        match format {
                            Format::Esm => {
                                let mut specs = Vec::new();
                                for (name, b) in l.exports[ei].iter().flatten() {
                                    let r =
                                        p.rewrite_for(e, b, &mut refs, &mut helpers, &mut ext_used);
                                    let local = match r {
                                        Rewrite::Name(n) => n,
                                        Rewrite::Undefined => continue,
                                        other => {
                                            let v = p.names.alloc(&format!("export_{name}"));
                                            tail.insert(
                                                0,
                                                format!(
                                                    "var {v}{sp}={sp}{};",
                                                    other.expr().to_text(compact)
                                                ),
                                            );
                                            v
                                        }
                                    };
                                    if local == *name {
                                        specs.push(local);
                                    } else {
                                        let quoted =
                                            if ts_r_codegen::literals::is_identifier_name(name) {
                                                name.clone()
                                            } else {
                                                Expr::Str(name.clone()).to_text(true)
                                            };
                                        specs.push(format!("{local} as {quoted}"));
                                    }
                                }
                                if !specs.is_empty() {
                                    tail.push(format!(
                                        "export{sp}{{{sp}{}{sp}}};",
                                        specs.join(if compact { "," } else { ", " })
                                    ));
                                }
                                for t in &l.dynamic_stars[ei] {
                                    if let Target::External(path) = t {
                                        star_reexports.push(format!(
                                            "export *{sp}from{sp}{};",
                                            Expr::Str(
                                                crate::graph::split_external(path).0.to_owned()
                                            )
                                            .to_text(true)
                                        ));
                                    }
                                }
                            }
                            Format::Cjs => {
                                helpers.mark(Helper::ToCjs);
                                refs.insert(p.ns_name[ei].clone());
                                head.push(format!(
                                    "module.exports{sp}={sp}{}({});",
                                    p.helper_names[Helper::ToCjs as usize],
                                    p.ns_name[ei]
                                ));
                            }
                            Format::Iife => {
                                helpers.mark(Helper::ToCjs);
                                refs.insert(p.ns_name[ei].clone());
                                iife_return = Some(Expr::call(
                                    &p.helper_names[Helper::ToCjs as usize],
                                    vec![Expr::ident(&p.ns_name[ei])],
                                ));
                            }
                        }
                    }
                }
                (Kind::Css, _) => {}
            }
        }
        // Externals.
        let mut ext_imports: Vec<ExternalImport> = Vec::new();
        let mut ext_requires: Vec<(String, String)> = Vec::new();
        let ext_path = |id: u32| l.externals[id as usize].clone();
        if format == Format::Esm {
            for (e, imp) in &ext_used {
                let path = ext_path(*e);
                let idx = match ext_imports.iter().position(|x| x.path == path) {
                    Some(i) => i,
                    None => {
                        ext_imports.push(ExternalImport {
                            path: path.clone(),
                            ..ExternalImport::default()
                        });
                        ext_imports.len() - 1
                    }
                };
                let local = p.ext_locals[&(*e, imp.clone())].clone();
                let x = &mut ext_imports[idx];
                match imp {
                    Imported::Namespace => x.namespace = Some(local),
                    Imported::Name(n) if n == "default" => x.default = Some(local),
                    Imported::Name(n) => {
                        if !x.named.iter().any(|(a, _)| a == n) {
                            x.named.push((n.clone(), local));
                        }
                    }
                }
            }
            for e in &ext_side {
                let path = ext_path(*e);
                if !ext_imports.iter().any(|x| x.path == path) {
                    ext_imports.push(ExternalImport {
                        path,
                        ..ExternalImport::default()
                    });
                }
            }
        } else {
            for (e, _) in &ext_used {
                let path = ext_path(*e);
                if !ext_requires.iter().any(|(p2, _)| *p2 == path) {
                    helpers.mark(Helper::ToEsm);
                    ext_requires.push((path, p.ext_var[e].clone()));
                }
            }
            for e in &ext_side {
                let path = ext_path(*e);
                if !ext_requires.iter().any(|(p2, _)| *p2 == path) {
                    ext_requires.push((path, String::new()));
                }
            }
        }
        // Externals load in evaluation order: each module's records in source order, an
        // internal static import or require walked at its position (as the module it names
        // runs before the rest of the importer).
        {
            let order = external_order(g, out.chunks[ci].entry, &mods);
            let rank = |p: &str| order.iter().position(|o| *o == p).unwrap_or(usize::MAX);
            ext_requires.sort_by_key(|(p, _)| rank(p));
            ext_imports.sort_by_key(|x| rank(&x.path));
        }
        // CSS modules reachable from this chunk.
        let c = &mut out.chunks[ci];
        c.helpers = helpers;
        c.ns_objects = ns_objects;
        c.head = head;
        c.tail = tail;
        c.iife_return = iife_return;
        c.ext_imports = ext_imports;
        c.ext_requires = ext_requires;
        c.require_shim = require_shim && format == Format::Esm && o.platform == Platform::Node;
        c.preludes = preludes;
        c.star_reexports = star_reexports;
        chunk_refs[ci] = refs;
    }
    // Cross-chunk imports and exports.
    if input.opts.splitting && out.chunks.len() > 1 {
        let mut exports: Vec<FxHashSet<String>> = vec![FxHashSet::default(); out.chunks.len()];
        #[allow(clippy::needless_range_loop)] // `out.chunks` is mutated in the body.
        for ci in 0..out.chunks.len() {
            let mut by_chunk: FxHashMap<usize, Vec<String>> = FxHashMap::default();
            for name in &chunk_refs[ci] {
                if let Some(&m) = p.owner.get(name)
                    && let Some(oc) = out.module_chunk[m as usize]
                    && oc != ci
                {
                    by_chunk.entry(oc).or_default().push(name.clone());
                }
            }
            // Chunks reached from the same entries (strict supersets of this chunk's key)
            // run first, even when no names are imported from them.
            let key = out.chunks[ci].key.clone();
            for (oc, other) in out.chunks.iter().enumerate() {
                if oc != ci
                    && other.key.len() > key.len()
                    && key.iter().all(|k| other.key.contains(k))
                    && !other.modules.is_empty()
                {
                    by_chunk.entry(oc).or_default();
                }
            }
            let mut imports: Vec<(usize, Vec<String>)> = by_chunk.into_iter().collect();
            imports.sort_by(|a, b| {
                out.chunks[b.0]
                    .key
                    .len()
                    .cmp(&out.chunks[a.0].key.len())
                    .then(a.0.cmp(&b.0))
            });
            for (oc, names) in &mut imports {
                names.sort();
                exports[*oc].extend(names.iter().cloned());
            }
            out.chunks[ci].imports = imports;
        }
        for (ci, e) in exports.into_iter().enumerate() {
            let mut v: Vec<String> = e.into_iter().collect();
            v.sort();
            out.chunks[ci].exports = v;
        }
    }
    // The unwrapped entry names its own top-level bindings (nothing imports them): they
    // only have to stay clear of every other top-level name of the file.
    if let Some(e) = p.unwrapped
        && let Some(mp) = &mut out.plans[e as usize]
    {
        let mut all: Vec<String> = p.names.taken.iter().cloned().collect();
        all.sort();
        mp.top_reserved = all;
    }
    out
}

impl Planner<'_> {
    fn stem(&self, m: ModuleId) -> String {
        let module = &self.g.modules[m as usize];
        match module.path.strip_prefix(DISABLED_PREFIX) {
            Some(rest) => ts_r_minify::names::sanitize(rest),
            None => file_stem_name(&module.pretty),
        }
    }

    fn name_module(&mut self, m: ModuleId) {
        let mi = m as usize;
        let l = self.l;
        if !l.included[mi] || l.kinds[mi] == Kind::Css {
            return;
        }
        let stem = self.stem(m);
        let unwrapped = self.unwrapped == Some(m);
        match l.wrap[mi] {
            Wrap::Cjs if unwrapped => {}
            Wrap::Cjs => {
                let n = self.names.alloc(&format!("require_{stem}"));
                self.owner.insert(n.clone(), m);
                self.wrapper[mi] = n;
            }
            Wrap::Esm => {
                let n = self.names.alloc(&format!("init_{stem}"));
                self.owner.insert(n.clone(), m);
                self.wrapper[mi] = n;
            }
            Wrap::None => {}
        }
        if l.needs_ns[mi] && l.kinds[mi] == Kind::Esm {
            let n = self.names.alloc(&format!("{stem}_exports"));
            self.owner.insert(n.clone(), m);
            self.ns_name[mi] = n;
        }
        if l.wrap[mi] == Wrap::Cjs {
            return;
        }
        let meta = &self.g.modules[mi].meta;
        let all = l.wrap[mi] == Wrap::Esm;
        let mut declared: FxHashSet<Sym> = FxHashSet::default();
        for (pi, part) in meta.parts.iter().enumerate() {
            if all || l.live[mi][pi] {
                declared.extend(part.declares.iter().copied());
            }
        }
        for s in &meta.syms {
            if s.kind == SymKind::Import || !declared.contains(&s.sym) {
                continue;
            }
            let n = self.names.alloc(&s.name);
            self.owner.insert(n.clone(), m);
            if Some(s.sym) == meta.default_sym {
                self.default_name[mi] = n.clone();
            }
            self.sym_names[mi].insert(s.sym, n);
        }
        // An anonymous default export always needs its name.
        if let Some(d) = meta.default_sym
            && self.default_name[mi].is_empty()
        {
            let n = self.names.alloc(&format!("{stem}_default"));
            self.owner.insert(n.clone(), m);
            self.default_name[mi] = n.clone();
            self.sym_names[mi].insert(d, n);
        }
    }

    fn import_var(&mut self, importer: ModuleId, t: ModuleId) -> String {
        if let Some(n) = self.import_vars.get(&(importer, t)) {
            return n.clone();
        }
        let n = self.names.alloc(&format!("import_{}", self.stem(t)));
        self.owner.insert(n.clone(), importer);
        self.import_vars.insert((importer, t), n.clone());
        n
    }

    fn ext_stem(&self, e: u32) -> String {
        let path = crate::graph::split_external(&self.l.externals[e as usize]).0;
        let last = path
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or(path);
        let base = if path.starts_with('@') || !path.starts_with('.') {
            path
        } else {
            last
        };
        let base = base.split('.').next().unwrap_or(base);
        ts_r_minify::names::sanitize(base)
    }

    /// How references to binding `b` look in module `m`'s code. Records referenced names
    /// in `refs` and needed helpers.
    fn rewrite_for(
        &mut self,
        m: ModuleId,
        b: &Binding,
        refs: &mut FxHashSet<String>,
        helpers: &mut Helpers,
        ext_used: &mut Vec<(u32, Imported)>,
    ) -> Rewrite {
        let r = match b {
            Binding::Sym(t, s) => match self.sym_names[*t as usize].get(s) {
                Some(n) => Rewrite::Name(n.clone()),
                None => Rewrite::Undefined,
            },
            Binding::Ns(t) => Rewrite::Name(self.ns_name[*t as usize].clone()),
            Binding::NsProp(t, p) => Rewrite::Member(self.ns_name[*t as usize].clone(), p.clone()),
            Binding::Cjs(t, prop) => {
                helpers.mark(Helper::ToEsm);
                let v = self.import_var(m, *t);
                match prop {
                    None => Rewrite::Name(v),
                    Some(p) => Rewrite::Member(v, p.clone()),
                }
            }
            Binding::External(e, imp) => {
                if !ext_used.contains(&(*e, imp.clone())) {
                    ext_used.push((*e, imp.clone()));
                }
                if self.format == Format::Esm {
                    let key = (*e, imp.clone());
                    let local = match self.ext_locals.get(&key) {
                        Some(l) => l.clone(),
                        None => {
                            let stem = self.ext_stem(*e);
                            let pref = match imp {
                                Imported::Namespace => stem,
                                Imported::Name(n) if n == "default" => format!("{stem}_default"),
                                Imported::Name(n) => n.clone(),
                            };
                            let l = self.names.alloc(&pref);
                            self.ext_locals.insert(key, l.clone());
                            l
                        }
                    };
                    Rewrite::Name(local)
                } else {
                    helpers.mark(Helper::ToEsm);
                    let v = match self.ext_var.get(e) {
                        Some(v) => v.clone(),
                        None => {
                            let v = self.names.alloc(&format!("import_{}", self.ext_stem(*e)));
                            self.ext_var.insert(*e, v.clone());
                            v
                        }
                    };
                    match imp {
                        Imported::Namespace => Rewrite::Name(v),
                        Imported::Name(n) => Rewrite::Member(v, n.clone()),
                    }
                }
            }
            Binding::Undefined => Rewrite::Undefined,
        };
        if let Some(root) = r.root() {
            refs.insert(root.to_owned());
        }
        r
    }

    #[allow(clippy::too_many_arguments)]
    fn module_plan(
        &mut self,
        m: ModuleId,
        _chunk: usize,
        out: &Plan,
        helpers: &mut Helpers,
        chunk_refs: &mut FxHashSet<String>,
        ext_used: &mut Vec<(u32, Imported)>,
        require_shim: &mut bool,
    ) -> ModulePlan {
        let mi = m as usize;
        let g = self.g;
        let l = self.l;
        let module = &g.modules[mi];
        let meta = &module.meta;
        let mut refs: FxHashSet<String> = FxHashSet::default();
        let mut names: Vec<(Sym, String)> = self.sym_names[mi]
            .iter()
            .map(|(s, n)| (*s, n.clone()))
            .collect();
        let mut rewrites = Vec::new();
        // Modules this one must `init_x()` / `require_x()` first, in order.
        let mut inits: Vec<ModuleId> = Vec::new();
        let mut cjs_uses: Vec<ModuleId> = Vec::new();
        let note = |b: &Binding, inits: &mut Vec<ModuleId>, cjs_uses: &mut Vec<ModuleId>| match b {
            Binding::Sym(t, _) | Binding::Ns(t) | Binding::NsProp(t, _) => {
                if l.wrap[*t as usize] == Wrap::Esm && *t != m && !inits.contains(t) {
                    inits.push(*t);
                }
            }
            Binding::Cjs(t, _) => {
                if !cjs_uses.contains(t) {
                    cjs_uses.push(*t);
                }
            }
            _ => {}
        };
        let mut bindings: Vec<(&Sym, &Binding)> = l.bindings[mi].iter().collect();
        bindings.sort_by_key(|(s, _)| **s);
        for (s, b) in bindings {
            note(b, &mut inits, &mut cjs_uses);
            match self.rewrite_for(m, b, &mut refs, helpers, ext_used) {
                Rewrite::Name(n) => names.push((*s, n)),
                r => rewrites.push((*s, r)),
            }
        }
        let mut ns_list: Vec<(&(Sym, String), &Binding)> = l.ns_members[mi].iter().collect();
        ns_list.sort_by(|a, b| a.0.cmp(b.0));
        let mut ns_rewrites = Vec::new();
        for ((s, prop), b) in ns_list {
            note(b, &mut inits, &mut cjs_uses);
            let r = self.rewrite_for(m, b, &mut refs, helpers, ext_used);
            ns_rewrites.push((*s, prop.clone(), r));
        }
        // Exports seen through this module's namespace object or entry code.
        if l.needs_ns[mi] || out.chunks.iter().any(|c| c.entry == Some(m)) {
            for (_, b) in l.exports[mi].iter().flatten() {
                note(b, &mut inits, &mut cjs_uses);
                if let Binding::Cjs(t, _) = b {
                    self.import_var(m, *t);
                }
            }
            for t in &l.dynamic_stars[mi] {
                if let Target::Module(u) = t
                    && !cjs_uses.contains(u)
                {
                    cjs_uses.push(*u);
                }
            }
        }
        // Records.
        let mut records = Vec::with_capacity(meta.records.len());
        let live_records: FxHashSet<u32> = meta
            .parts
            .iter()
            .enumerate()
            .filter(|(pi, _)| l.live[mi][*pi])
            .flat_map(|(_, p)| p.records.iter().copied())
            .collect();
        let splitting = self.o.splitting;
        for (ri, r) in meta.records.iter().enumerate() {
            let target = &module.targets[ri];
            let e = match (r.kind, target) {
                (RecordKind::Require | RecordKind::Dynamic, _)
                    if !live_records.contains(&(ri as u32)) =>
                {
                    None
                }
                (RecordKind::Require, Target::Module(t)) => {
                    let ti = *t as usize;
                    Some(match l.kinds[ti] {
                        Kind::Cjs => {
                            helpers.mark(Helper::Cjs);
                            refs.insert(self.wrapper[ti].clone());
                            Expr::call(&self.wrapper[ti], vec![])
                        }
                        Kind::Esm => {
                            helpers.mark(Helper::ToCjs);
                            refs.insert(self.wrapper[ti].clone());
                            refs.insert(self.ns_name[ti].clone());
                            let to_cjs = self.helper_names[Helper::ToCjs as usize].clone();
                            Expr::Seq(vec![
                                Expr::call(&self.wrapper[ti], vec![]),
                                Expr::call(&to_cjs, vec![Expr::ident(&self.ns_name[ti])]),
                            ])
                        }
                        Kind::Css => Expr::Object(Vec::new()),
                    })
                }
                (RecordKind::Dynamic, Target::Module(t)) => {
                    let ti = *t as usize;
                    if splitting {
                        let from = &out.chunks[out.module_chunk[mi].unwrap_or(0)].file;
                        let target_chunk = out.chunks.iter().position(|c| c.entry == Some(*t));
                        target_chunk
                            .map(|tc| Expr::Import(relative_import(from, &out.chunks[tc].file)))
                    } else {
                        Some(match l.kinds[ti] {
                            Kind::Cjs => {
                                helpers.mark(Helper::ToEsm);
                                helpers.mark(Helper::Cjs);
                                refs.insert(self.wrapper[ti].clone());
                                let to_esm = self.helper_names[Helper::ToEsm as usize].clone();
                                Expr::promise_then(Expr::call(
                                    &to_esm,
                                    vec![Expr::call(&self.wrapper[ti], vec![])],
                                ))
                            }
                            Kind::Esm => {
                                refs.insert(self.wrapper[ti].clone());
                                refs.insert(self.ns_name[ti].clone());
                                Expr::promise_then(Expr::Seq(vec![
                                    Expr::call(&self.wrapper[ti], vec![]),
                                    Expr::ident(&self.ns_name[ti]),
                                ]))
                            }
                            Kind::Css => Expr::promise_then(Expr::Object(Vec::new())),
                        })
                    }
                }
                (RecordKind::Require, Target::External(x))
                    if self.o.require_to_import && self.format == Format::Esm =>
                {
                    match l.externals.iter().position(|e| e == x) {
                        Some(e) => {
                            let b = Binding::External(e as u32, Imported::Namespace);
                            match self.rewrite_for(m, &b, &mut refs, helpers, ext_used) {
                                Rewrite::Name(ns) => {
                                    helpers.mark(Helper::ToCjs);
                                    let to_cjs = self.helper_names[Helper::ToCjs as usize].clone();
                                    Some(Expr::call(&to_cjs, vec![Expr::ident(&ns)]))
                                }
                                _ => None,
                            }
                        }
                        None => {
                            *require_shim = true;
                            None
                        }
                    }
                }
                (RecordKind::Require, Target::External(_)) => {
                    *require_shim = true;
                    None
                }
                _ => None,
            };
            records.push(e);
        }
        // Prologue: static imports in order, then anything else bindings need.
        let mut prologue = Vec::new();
        let mut declared: FxHashSet<ModuleId> = FxHashSet::default();
        let mut inited: FxHashSet<ModuleId> = FxHashSet::default();
        let node_mode = l.node_mode[mi];
        let cjs_var = |this: &mut Self,
                       t: ModuleId,
                       prologue: &mut Vec<PrologueStmt>,
                       helpers: &mut Helpers,
                       refs: &mut FxHashSet<String>| {
            helpers.mark(Helper::ToEsm);
            helpers.mark(Helper::Cjs);
            let v = this.import_var(m, t);
            let to_esm = this.helper_names[Helper::ToEsm as usize].clone();
            let mut args = vec![Expr::call(&this.wrapper[t as usize], vec![])];
            let json = {
                let tm = &this.g.modules[t as usize];
                tm.loader == Loader::Json && !tm.default_only
            };
            if json {
                args.push(Expr::Num(2));
            } else if node_mode {
                args.push(Expr::Num(1));
            }
            refs.insert(this.wrapper[t as usize].clone());
            refs.insert(v.clone());
            prologue.push(PrologueStmt::Var(v, Expr::call(&to_esm, args)));
        };
        for (ri, r) in meta.records.iter().enumerate() {
            if !matches!(r.kind, RecordKind::Import | RecordKind::ReExport) {
                continue;
            }
            let Target::Module(t) = module.targets[ri] else {
                continue;
            };
            let ti = t as usize;
            if !l.included[ti] {
                continue;
            }
            match l.wrap[ti] {
                Wrap::Cjs => {
                    if declared.contains(&t) {
                        continue;
                    }
                    declared.insert(t);
                    if cjs_uses.contains(&t) {
                        cjs_var(self, t, &mut prologue, helpers, &mut refs);
                    } else {
                        helpers.mark(Helper::Cjs);
                        refs.insert(self.wrapper[ti].clone());
                        prologue.push(PrologueStmt::Expr(Expr::call(&self.wrapper[ti], vec![])));
                    }
                }
                Wrap::Esm => {
                    if inited.insert(t) {
                        refs.insert(self.wrapper[ti].clone());
                        prologue.push(PrologueStmt::Expr(Expr::call(&self.wrapper[ti], vec![])));
                    }
                }
                Wrap::None => {}
            }
        }
        for t in cjs_uses.clone() {
            if declared.insert(t) {
                cjs_var(self, t, &mut prologue, helpers, &mut refs);
            }
        }
        for t in inits {
            if inited.insert(t) {
                refs.insert(self.wrapper[t as usize].clone());
                prologue.push(PrologueStmt::Expr(Expr::call(
                    &self.wrapper[t as usize],
                    vec![],
                )));
            }
        }
        if l.needs_ns[mi] {
            // A CommonJS entry's `module.exports` was copied from its namespace before this
            // code runs: the run-time names go there too.
            let cjs_entry =
                self.format == Format::Cjs && out.chunks.iter().any(|c| c.entry == Some(m));
            for t in &l.dynamic_stars[mi] {
                let src = match t {
                    Target::Module(u) => self.import_var(m, *u),
                    Target::External(path) => {
                        let is_entry_esm = self.format == Format::Esm
                            && out
                                .chunks
                                .iter()
                                .any(|c| c.entry == Some(m) && c.user_entry.is_some());
                        if is_entry_esm {
                            continue;
                        }
                        let e = self.l.externals.iter().position(|x| x == path).unwrap_or(0) as u32;
                        match self.rewrite_for(
                            m,
                            &Binding::External(e, Imported::Namespace),
                            &mut refs,
                            helpers,
                            ext_used,
                        ) {
                            Rewrite::Name(n) => n,
                            _ => continue,
                        }
                    }
                    Target::Missing => continue,
                };
                helpers.mark(Helper::Copy);
                let copy = self.helper_names[Helper::Copy as usize].clone();
                refs.insert(self.ns_name[mi].clone());
                prologue.push(PrologueStmt::Expr(Expr::call(
                    &copy,
                    vec![
                        Expr::ident(&self.ns_name[mi]),
                        Expr::ident(&src),
                        Expr::Str("default".into()),
                    ],
                )));
                if cjs_entry {
                    refs.insert("module".into());
                    prologue.push(PrologueStmt::Expr(Expr::call(
                        &copy,
                        vec![
                            Expr::member(Expr::ident("module"), "exports"),
                            Expr::ident(&src),
                            Expr::Str("default".into()),
                        ],
                    )));
                }
            }
        }
        let wrap = if self.unwrapped == Some(m) {
            Wrap::None
        } else {
            l.wrap[mi]
        };
        match wrap {
            Wrap::Cjs => helpers.mark(Helper::Cjs),
            Wrap::Esm => helpers.mark(Helper::Esm),
            Wrap::None => {}
        }
        if !self.wrapper[mi].is_empty() {
            refs.insert(self.wrapper[mi].clone());
        }
        names.sort();
        names.dedup_by(|a, b| a.0 == b.0);
        rewrites.sort_by(|a: &(Sym, Rewrite), b| a.0.cmp(&b.0));
        let mut reserved: Vec<String> = refs.iter().cloned().collect();
        reserved.extend(self.helper_names.iter().cloned());
        reserved.sort();
        reserved.dedup();
        chunk_refs.extend(refs);
        let live = if l.kinds[mi] != Kind::Esm {
            vec![true; meta.parts.len()]
        } else {
            l.live[mi].clone()
        };
        // Parts that only import/export are never printed.
        let live = live
            .into_iter()
            .zip(&meta.parts)
            .map(|(v, p)| {
                v && !matches!(
                    p.kind,
                    PartKind::Import | PartKind::ExportFrom | PartKind::ExportLocal
                )
            })
            .collect();
        ModulePlan {
            wrap,
            wrapper_name: self.wrapper[mi].clone(),
            live,
            names,
            rewrites,
            ns_rewrites,
            records,
            prologue,
            default_name: self.default_name[mi].clone(),
            reserved,
            helper_cjs: self.helper_names[Helper::Cjs as usize].clone(),
            helper_esm: self.helper_names[Helper::Esm as usize].clone(),
            own_top_level: self.unwrapped == Some(m),
            top_reserved: Vec::new(),
            import_meta_empty: self.format != Format::Esm,
            mangle: self.o.minify_identifiers,
            minify_syntax: self.o.minify_syntax,
            compact: self.o.minify_whitespace,
            comments: !self.o.minify_whitespace,
            map_source: if self.o.sourcemap == crate::options::SourceMapMode::None {
                String::new()
            } else {
                module.pretty.clone()
            },
        }
    }

    fn make_chunks(&mut self, input: &PlanInput<'_>, order: &[ModuleId]) -> Vec<Chunk> {
        let g = self.g;
        let l = self.l;
        let js = |m: ModuleId| l.included[m as usize] && l.kinds[m as usize] != Kind::Css;
        let css = |m: ModuleId| l.included[m as usize] && l.kinds[m as usize] == Kind::Css;
        let ext = &self.o.out_extension_js;
        if !self.o.splitting {
            let mut chunks = Vec::new();
            for (i, &e) in input.entries.iter().enumerate() {
                let reach = reachable(g, l, e, true);
                let modules: Vec<ModuleId> = order
                    .iter()
                    .copied()
                    .filter(|&m| reach[m as usize] && js(m))
                    .collect();
                let cssm: Vec<ModuleId> = order
                    .iter()
                    .copied()
                    .filter(|&m| reach[m as usize] && css(m))
                    .collect();
                chunks.push(Chunk {
                    entry: Some(e),
                    user_entry: Some(i),
                    modules,
                    css: cssm,
                    file: input.entry_files[i].clone(),
                    ..Chunk::default()
                });
            }
            return chunks;
        }
        // Splitting: group modules by the set of entries that reach them statically.
        let all_entries: Vec<ModuleId> = input
            .entries
            .iter()
            .chain(input.dynamic_entries)
            .copied()
            .collect();
        let n = g.modules.len();
        let mut bits: Vec<Vec<u32>> = vec![Vec::new(); n];
        for (ei, &e) in all_entries.iter().enumerate() {
            let reach = reachable(g, l, e, false);
            for m in 0..n {
                if reach[m] {
                    bits[m].push(ei as u32);
                }
            }
        }
        let mut chunks: Vec<Chunk> = Vec::new();
        let mut key_chunk: FxHashMap<Vec<u32>, usize> = FxHashMap::default();
        for (ei, &e) in all_entries.iter().enumerate() {
            let user = ei < input.entries.len();
            let file = if user {
                input.entry_files[ei].clone()
            } else {
                input.dynamic_files[ei - input.entries.len()].clone()
            };
            key_chunk.insert(vec![ei as u32], chunks.len());
            chunks.push(Chunk {
                entry: Some(e),
                user_entry: user.then_some(ei),
                key: vec![ei as u32],
                file,
                ..Chunk::default()
            });
        }
        for &m in order {
            if !l.included[m as usize] || bits[m as usize].is_empty() {
                continue;
            }
            let key = bits[m as usize].clone();
            let ci = match key_chunk.get(&key) {
                Some(&c) => c,
                None => {
                    let c = chunks.len();
                    key_chunk.insert(key.clone(), c);
                    chunks.push(Chunk {
                        key,
                        ..Chunk::default()
                    });
                    c
                }
            };
            if js(m) {
                chunks[ci].modules.push(m);
            } else if css(m) {
                chunks[ci].css.push(m);
            }
        }
        // Shared chunk names: content of the chunk (module keys) decides the hash.
        for c in chunks.iter_mut() {
            if c.entry.is_none() {
                let mut h = blake3::Hasher::new();
                for &m in &c.modules {
                    h.update(&g.modules[m as usize].prep_key);
                    h.update(g.modules[m as usize].pretty.as_bytes());
                }
                let hash = crate::hash_text(h.finalize().as_bytes());
                let name = self
                    .o
                    .chunk_names
                    .replace("[name]", "chunk")
                    .replace("[hash]", &hash)
                    .replace("[dir]/", "")
                    .replace("[dir]", "");
                c.file = format!("{name}{ext}");
            }
        }
        // CSS of shared chunks goes with the entries that load them.
        chunks
    }
}

/// Modules reachable from `e` over included targets (`dynamic`: follow `import()` too).
fn reachable(g: &Graph, l: &Link, e: ModuleId, dynamic: bool) -> Vec<bool> {
    let mut seen = vec![false; g.modules.len()];
    let mut stack = vec![e];
    seen[e as usize] = true;
    while let Some(m) = stack.pop() {
        let module = &g.modules[m as usize];
        for (i, t) in module.targets.iter().enumerate() {
            if !dynamic && module.meta.records[i].kind == RecordKind::Dynamic {
                continue;
            }
            if let Target::Module(u) = t
                && !seen[*u as usize]
                && l.included[*u as usize]
            {
                seen[*u as usize] = true;
                stack.push(*u);
            }
        }
    }
    seen
}

/// `./x.js`-style import path from one output file to another (both relative to the output
/// directory).
pub fn relative_import(from: &str, to: &str) -> String {
    let from_dir = ts_r_config::path::directory_of(from);
    let rel = if from_dir.is_empty() {
        to.to_owned()
    } else {
        ts_r_config::path::relative(&format!("/{from_dir}"), &format!("/{to}"), true)
    };
    if rel.starts_with("../") || rel.starts_with("./") {
        rel
    } else {
        format!("./{rel}")
    }
}

/// External import keys in evaluation order, walking from the chunk's entry and then its
/// other modules. Iterative so deep import chains cannot overflow the stack.
fn external_order<'g>(g: &'g Graph, entry: Option<ModuleId>, mods: &[ModuleId]) -> Vec<&'g str> {
    let mut order: Vec<&'g str> = Vec::new();
    let mut seen: FxHashSet<&'g str> = FxHashSet::default();
    let mut visited = vec![false; g.modules.len()];
    for root in entry.into_iter().chain(mods.iter().copied()) {
        if visited[root as usize] {
            continue;
        }
        visited[root as usize] = true;
        // (module, next record index)
        let mut stack: Vec<(ModuleId, usize)> = vec![(root, 0)];
        while let Some(top) = stack.last_mut() {
            let (m, i) = *top;
            let module = &g.modules[m as usize];
            if i >= module.targets.len() {
                stack.pop();
                continue;
            }
            top.1 += 1;
            let dynamic = module
                .meta
                .records
                .get(i)
                .is_some_and(|r| r.kind == RecordKind::Dynamic);
            match &module.targets[i] {
                Target::External(x) => {
                    if seen.insert(x.as_str()) {
                        order.push(x.as_str());
                    }
                }
                Target::Module(t) if !dynamic && !visited[*t as usize] => {
                    visited[*t as usize] = true;
                    stack.push((*t, 0));
                }
                _ => {}
            }
        }
    }
    order
}
