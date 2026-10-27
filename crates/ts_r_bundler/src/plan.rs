//! Planning: chunks, module order, names, and one [`ModulePlan`] per emitted module.
//!
//! Names are global to the build (one table for all chunks), so a symbol has the same name
//! in every chunk and cross-chunk imports never need aliases. Each name is the symbol's own
//! name unless taken (then `name2`, `name3`, ...), or the next short name when minifying.
//! Names never collide with globals referenced by any module, keywords, or the CommonJS
//! names (`require`, `module`, `exports`).

use rustc_hash::{FxHashMap, FxHashSet};

use crate::expr::Expr;
use crate::features::Feature;
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
    /// The `init_x()` wrapper is an `async` arrow (see [`Link::async_init`]).
    pub wrap_async: bool,
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
    /// Records whose replacement is a function the printer calls with the call's own
    /// argument: a glob's `__tsrGlob({ ... })` (`RecordKind::GlobRequire`).
    pub globs: Vec<u32>,
    pub prologue: Vec<PrologueStmt>,
    /// Name for an anonymous `export default`.
    pub default_name: String,
    /// Names the module's rewritten code refers to (inner bindings must not capture them).
    pub reserved: Vec<String>,
    /// Helper names (for wrappers).
    pub helper_cjs: String,
    pub helper_esm: String,
    /// `--keep-names`: the `__tsrName` helper's name (empty = off).
    pub keep_names: String,
    /// The module runs unwrapped at the top of the file but names its own top-level
    /// bindings, around `top_reserved` (every other top-level name of the file).
    pub own_top_level: bool,
    pub top_reserved: Vec<String>,
    /// The module's `var import_meta = {};` that every `import.meta` refers to (non-ESM
    /// output, or `import-meta` unsupported); empty = keep `import.meta`.
    pub import_meta: String,
    pub mangle: bool,
    /// `--mangle-props`: new names for the module's property names (sorted by name), and
    /// whether written strings in property positions are names (`--mangle-quoted`).
    pub props: Vec<(String, String)>,
    pub props_quoted: bool,
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
    /// Statements to print before a module's code: an importer's `require_x()` / `init_x()`
    /// calls that must run before the module (`(importer, statement)`, in order).
    pub before: FxHashMap<ModuleId, Vec<(ModuleId, String)>>,
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
    pub helper_names: [String; crate::runtime::HELPER_COUNT],
    pub format: Format,
    /// Output directory (absolute).
    pub outdir: String,
    /// Final local CSS names and the class lists JavaScript sees.
    pub css: crate::css::link::CssNames,
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
    import_meta: Vec<String>,
    /// (importer, CommonJS module) -> `import_x` name.
    import_vars: FxHashMap<(ModuleId, ModuleId), String>,
    /// External id -> CommonJS `import_x` var / ESM locals.
    ext_var: FxHashMap<u32, String>,
    ext_locals: FxHashMap<(u32, Imported), String>,
    /// name -> owning module (for cross-chunk imports).
    owner: FxHashMap<String, ModuleId>,
    helper_names: [String; crate::runtime::HELPER_COUNT],
    /// CommonJS entries printed at the top level of their CommonJS chunk (see
    /// [`unwrapped_entries`]).
    unwrapped: Vec<ModuleId>,
    /// Global evaluation order, and each module's position in it.
    order: Vec<ModuleId>,
    position: Vec<usize>,
    /// Per importer: `(record index, position)` where it first reached an unwrapped module
    /// (see [`module_order_with_boundaries`]).
    boundaries: Vec<Vec<(usize, usize)>>,
    /// Import statements moved out of an importer, to print before a module of its chunk.
    before: FxHashMap<ModuleId, Vec<(ModuleId, String)>>,
    /// `--mangle-props`: one short name per property name over every included module,
    /// sorted by name.
    props: Vec<(String, String)>,
}

/// The CommonJS entries that can run directly at the top of their CommonJS output file, with
/// Node's own `module`, `exports` and `require`, instead of through a `require_x` wrapper:
/// nothing requires the entry (no cycle back to it, no other entry), no top-level `return` or
/// `arguments`, no direct `eval`, and its `"use strict"` (written first in the file) suits
/// every other module of the bundle. It is the last module, where the wrapper call used to run.
fn unwrapped_entries(input: &PlanInput<'_>, format: Format) -> Vec<ModuleId> {
    let (g, l, o) = (input.graph, input.link, input.opts);
    if format != Format::Cjs || o.splitting || !input.dynamic_entries.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<ModuleId> = Vec::new();
    for &e in input.entries {
        if out.contains(&e) || input.entries.iter().filter(|&&x| x == e).count() > 1 {
            continue;
        }
        let ei = e as usize;
        if l.kinds[ei] != Kind::Cjs || l.wrap[ei] != Wrap::Cjs {
            continue;
        }
        let meta = &g.modules[ei].meta;
        if meta.needs_function
            || meta.has_direct_eval
            || meta.use_strict && !strict_file_ok(input, e)
        {
            continue;
        }
        if g.modules
            .iter()
            .enumerate()
            .any(|(i, m)| l.included[i] && m.targets.contains(&Target::Module(e)))
        {
            continue;
        }
        // Only when it is smaller: the wrapper (`var a=b((c,d)=>{`, `});`,
        // `module.exports=a();`) and, when no other module needs it, the CommonJS helper go;
        // the free `module` and `exports` references lose their one-letter parameter names.
        let helper_elsewhere =
            (0..g.modules.len()).any(|i| i != ei && l.included[i] && l.wrap[i] == Wrap::Cjs);
        let saved = 38 + if helper_elsewhere { 0 } else { 75 };
        if meta.cjs_ref_bytes < saved {
            out.push(e);
        }
    }
    out
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
    module_order_with_boundaries(g, link, roots).0
}

/// [`module_order`], plus where each module first reached an unwrapped module: `(importer,
/// record index, position in the order)`, in visiting order. The code of that module's
/// subtree starts at that position, so the importer's `require_x()` / `init_x()` calls for
/// its earlier imports must run there (as an ES module evaluates its imports in order).
fn module_order_with_boundaries(
    g: &Graph,
    link: &Link,
    roots: &[ModuleId],
) -> (Vec<ModuleId>, Vec<(ModuleId, usize, usize)>) {
    let n = g.modules.len();
    let mut boundaries = Vec::new();
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
                    if link.wrap[*u as usize] == Wrap::None {
                        boundaries.push((m, *i - 1, order.len()));
                    }
                    visited[*u as usize] = true;
                    stack.push((*u, 0));
                }
            } else {
                order.push(m);
                stack.pop();
            }
        }
    }
    (order, boundaries)
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
        import_meta: vec![String::new(); n],
        import_vars: FxHashMap::default(),
        ext_var: FxHashMap::default(),
        ext_locals: FxHashMap::default(),
        owner: FxHashMap::default(),
        helper_names: Default::default(),
        unwrapped: unwrapped_entries(input, format),
        order: Vec::new(),
        position: vec![usize::MAX; n],
        boundaries: vec![Vec::new(); n],
        before: FxHashMap::default(),
        props: Vec::new(),
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
    let (order, boundaries) = module_order_with_boundaries(g, l, &roots);
    for &m in &order {
        p.name_module(m);
    }
    for (i, &m) in order.iter().enumerate() {
        p.position[m as usize] = i;
    }
    for (m, ri, at) in boundaries {
        p.boundaries[m as usize].push((ri, at));
    }
    p.order = order.clone();
    if o.mangle_props.is_some() {
        p.props =
            ts_r_minify::props::assign(order.iter().map(|&m| &g.modules[m as usize].meta.props));
    }
    // Chunks.
    let chunks = p.make_chunks(input, &order);
    let mut module_chunk = vec![None; n];
    for (ci, c) in chunks.iter().enumerate() {
        for &m in &c.modules {
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
        css: crate::css::link::assign(g, l, o.minify_identifiers),
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
        for &m in &mods {
            if let Some(b) = p.before.remove(&m) {
                out.chunks[ci].before.insert(m, b);
            }
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
                (Kind::Cjs, _) if p.unwrapped.contains(&e) => {
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
                        let wait = if l.async_init[ei] { "await " } else { "" };
                        tail.push(format!("{wait}{}();", p.wrapper[ei]));
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
            let rank = |p: &str| order.iter().position(|o| o.path == p).unwrap_or(usize::MAX);
            ext_requires.sort_by_key(|(p, _)| rank(p));
            ext_imports.sort_by_key(|x| rank(&x.path));
            // CommonJS and IIFE output: an external that an unwrapped module of this chunk
            // loads first is required where that import runs, as esbuild does: before the
            // code of the next module to finish in evaluation order (the importer's own, or
            // a dependency it imports next). The rest load at the top of the chunk.
            let c = &mut out.chunks[ci];
            ext_requires.retain(|(path, var)| {
                let Some(at) = order.iter().find(|o| o.path == path) else {
                    return true;
                };
                if l.wrap[at.importer as usize] != Wrap::None
                    || !mods.contains(&at.importer)
                    || !mods.contains(&at.before)
                {
                    return true;
                }
                let path = crate::graph::split_external(path).0;
                let call = format!("require({})", Expr::Str(path.to_owned()).to_text(true));
                let text = if var.is_empty() {
                    format!("{call};")
                } else {
                    format!(
                        "var {var}{sp}={sp}{}({call});",
                        p.helper_names[Helper::ToEsm as usize]
                    )
                };
                c.before
                    .entry(at.before)
                    .or_default()
                    .push((at.importer, text));
                false
            });
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
    if !p.unwrapped.is_empty() {
        let mut all: Vec<String> = p.names.taken.iter().cloned().collect();
        all.sort();
        for &e in &p.unwrapped {
            if let Some(mp) = &mut out.plans[e as usize] {
                mp.top_reserved = all.clone();
            }
        }
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
        if !l.included[mi] {
            return;
        }
        let stem = self.stem(m);
        let unwrapped = self.unwrapped.contains(&m);
        // esbuild declares an empty object per module for `import.meta` where the output
        // can't have it, and warns at each use (`prepare.rs`).
        if self.g.modules[mi].meta.uses_import_meta
            && (self.format != Format::Esm || self.o.unsupported().has(Feature::ImportMeta))
        {
            let n = self.names.alloc("import_meta");
            self.owner.insert(n.clone(), m);
            self.import_meta[mi] = n;
        }
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

    /// `init_x()`, or `await init_x()` when `x`'s wrapper is async.
    fn init_call(&self, t: ModuleId) -> Expr {
        let call = Expr::call(&self.wrapper[t as usize], vec![]);
        if self.l.async_init[t as usize] {
            Expr::Await(Box::new(call))
        } else {
            call
        }
    }

    fn import_var(&mut self, importer: ModuleId, t: ModuleId) -> String {
        if let Some(n) = self.import_vars.get(&(importer, t)) {
            return n.clone();
        }
        // `import * as ns from "./cjs"` names the variable `ns`, as esbuild does.
        let module = &self.g.modules[importer as usize];
        let alias = module
            .meta
            .imports
            .iter()
            .find(|b| {
                b.name == Imported::Namespace
                    && module.targets.get(b.record as usize) == Some(&Target::Module(t))
            })
            .and_then(|b| module.meta.syms.iter().find(|s| s.sym == b.sym))
            .map(|s| s.name.clone());
        let n = match alias {
            Some(a) => self.names.alloc(&a),
            None => self.names.alloc(&format!("import_{}", self.stem(t))),
        };
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
        let base = if !crate::graph::is_copy_path(path)
            && (path.starts_with('@') || !path.starts_with('.'))
        {
            path
        } else {
            last
        };
        let base = base.split('.').next().unwrap_or(base);
        ts_r_minify::names::sanitize(base)
    }
    /// For each live glob record, `__tsrGlob({ "./src/a.js": () => e, ... })`, where `e` is
    /// the replacement of the record of that file (`graph::expand_globs` gave it the glob's
    /// call node), which is then dropped: the glob's call replaces the whole call. Returns
    /// the glob records.
    fn glob_maps(
        &self,
        meta: &crate::meta::ModuleMeta,
        targets: &[Target],
        live: &FxHashSet<u32>,
        records: &mut [Option<Expr>],
        helpers: &mut Helpers,
    ) -> Vec<u32> {
        let mut owner: FxHashMap<u32, usize> = FxHashMap::default();
        for (ri, r) in meta.records.iter().enumerate() {
            if r.kind.is_glob() && live.contains(&(ri as u32)) {
                owner.insert(r.node, ri);
            }
        }
        if owner.is_empty() {
            return Vec::new();
        }
        let mut maps: Vec<Vec<(String, Expr)>> = vec![Vec::new(); meta.records.len()];
        for (ri, r) in meta.records.iter().enumerate() {
            if !matches!(r.kind, RecordKind::Require | RecordKind::Dynamic) {
                continue;
            }
            let Some(&gi) = owner.get(&r.node) else {
                continue;
            };
            // An external file (`--external:./src/a.js`) has no replacement, as a kept call:
            // its entry is the call itself, as esbuild writes it.
            let e = records[ri]
                .take()
                .or_else(|| match (r.kind, targets.get(ri)) {
                    (RecordKind::Require, Some(Target::External(x))) => {
                        Some(Expr::call("require", vec![Expr::Str(x.clone())]))
                    }
                    (RecordKind::Dynamic, Some(Target::External(x))) => {
                        Some(Expr::Import(x.clone()))
                    }
                    _ => None,
                });
            if let Some(e) = e {
                maps[gi].push((r.spec.clone(), Expr::Arrow(Box::new(e))));
            }
        }
        let mut globs: Vec<u32> = owner.values().map(|&gi| gi as u32).collect();
        globs.sort_unstable();
        helpers.mark(Helper::Glob);
        let name = &self.helper_names[Helper::Glob as usize];
        for &gi in &globs {
            let map = std::mem::take(&mut maps[gi as usize]);
            records[gi as usize] = Some(Expr::call(name, vec![Expr::Object(map)]));
        }
        globs
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
        chunk: usize,
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
                    })
                }
                (RecordKind::Dynamic, Target::Module(t)) => {
                    let ti = *t as usize;
                    if splitting {
                        let from = &out.chunks[out.module_chunk[mi].unwrap_or(0)].file;
                        let target_chunk = out.chunks.iter().position(|c| c.entry == Some(*t));
                        target_chunk.map(|tc| {
                            let to = &out.chunks[tc].file;
                            Expr::Import(chunk_import(from, to, &self.o.public_path))
                        })
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
                                let init = Expr::call(&self.wrapper[ti], vec![]);
                                let ns = Expr::ident(&self.ns_name[ti]);
                                if l.async_init[ti] {
                                    Expr::promise_then_after(init, ns)
                                } else {
                                    Expr::promise_then(Expr::Seq(vec![init, ns]))
                                }
                            }
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
                // A `copy` file: the call names the copy (`graph::copy_imports_external`).
                (RecordKind::Require, Target::External(x)) if crate::graph::is_copy_path(x) => {
                    *require_shim = true;
                    Some(Expr::call("require", vec![Expr::Str(x.clone())]))
                }
                (RecordKind::Dynamic, Target::External(x)) if crate::graph::is_copy_path(x) => {
                    Some(Expr::Import(x.clone()))
                }
                (RecordKind::Require, Target::External(_)) => {
                    *require_shim = true;
                    None
                }
                // `BuildOptions::dynamic_import_url` turned the file into a URL.
                (RecordKind::Dynamic, Target::External(x))
                    if self.o.dynamic_import_url.is_some() && *x != r.spec =>
                {
                    Some(Expr::Import(x.clone()))
                }
                _ => None,
            };
            records.push(e);
        }
        let globs = self.glob_maps(meta, &module.targets, &live_records, &mut records, helpers);
        // Prologue: static imports in order, then anything else bindings need.
        let mut prologue = Vec::new();
        if !self.import_meta[mi].is_empty() {
            prologue.push(PrologueStmt::Var(
                self.import_meta[mi].clone(),
                Expr::Object(Vec::new()),
            ));
        }
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
            let tm = &this.g.modules[t as usize];
            if tm.loader == Loader::Json {
                args.push(Expr::Num(if tm.default_only { 3 } else { 2 }));
            } else if node_mode {
                args.push(Expr::Num(1));
            }
            refs.insert(this.wrapper[t as usize].clone());
            refs.insert(v.clone());
            prologue.push(PrologueStmt::Var(v, Expr::call(&to_esm, args)));
        };
        // An unwrapped ES module's imports run in order, between the unwrapped modules it
        // imports: the calls for imports before one go in front of that module's code.
        let mut hoist = l.kinds[mi] == Kind::Esm
            && l.wrap[mi] == Wrap::None
            && !self.unwrapped.contains(&m)
            && !self.boundaries[mi].is_empty();
        let bounds = std::mem::take(&mut self.boundaries[mi]);
        let mut next_bound = 0;
        let stub_start = prologue.len();
        let mut stub_targets: Vec<ModuleId> = Vec::new();
        let compact = self.o.minify_whitespace;
        for (ri, r) in meta.records.iter().enumerate() {
            while hoist && next_bound < bounds.len() && bounds[next_bound].0 <= ri {
                let at = bounds[next_bound].1;
                next_bound += 1;
                if prologue.len() == stub_start {
                    continue;
                }
                let Some(&first) = self.order.get(at) else {
                    hoist = false;
                    break;
                };
                if first == m
                    || out.module_chunk[first as usize] != Some(chunk)
                    || stub_targets
                        .iter()
                        .any(|&t| self.position[t as usize] >= at)
                {
                    hoist = false;
                    break;
                }
                let sp = if compact { "" } else { " " };
                let moved = prologue.split_off(stub_start).into_iter().map(|st| {
                    let text = match st {
                        PrologueStmt::Expr(e) => format!("{};", e.to_text(compact)),
                        PrologueStmt::Var(n, e) => {
                            format!("var {n}{sp}={sp}{};", e.to_text(compact))
                        }
                    };
                    (m, text)
                });
                self.before.entry(first).or_default().extend(moved);
                stub_targets.clear();
            }
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
            if l.wrap[ti] != Wrap::None {
                stub_targets.push(t);
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
                        prologue.push(PrologueStmt::Expr(self.init_call(t)));
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
                prologue.push(PrologueStmt::Expr(self.init_call(t)));
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
        let wrap = if self.unwrapped.contains(&m) {
            Wrap::None
        } else {
            l.wrap[mi]
        };
        match wrap {
            Wrap::Cjs => helpers.mark(Helper::Cjs),
            Wrap::Esm => helpers.mark(Helper::Esm),
            Wrap::None => {}
        }
        // Renames are only known when the module prints: with `--keep-names`, every chunk
        // that has modules defines the helper.
        if self.o.keep_names {
            helpers.mark(Helper::Name);
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
        let live = l.live[mi].clone();
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
            wrap_async: wrap == Wrap::Esm && l.async_init[mi],
            wrapper_name: self.wrapper[mi].clone(),
            live,
            names,
            rewrites,
            ns_rewrites,
            records,
            globs,
            prologue,
            default_name: self.default_name[mi].clone(),
            reserved,
            helper_cjs: self.helper_names[Helper::Cjs as usize].clone(),
            helper_esm: self.helper_names[Helper::Esm as usize].clone(),
            keep_names: if self.o.keep_names {
                self.helper_names[Helper::Name as usize].clone()
            } else {
                String::new()
            },
            own_top_level: self.unwrapped.contains(&m),
            top_reserved: Vec::new(),
            import_meta: self.import_meta[mi].clone(),
            mangle: self.o.minify_identifiers,
            props: {
                let mut v: Vec<(String, String)> = meta
                    .props
                    .mangled
                    .iter()
                    .filter_map(|(n, _)| {
                        let i = self
                            .props
                            .binary_search_by(|(k, _)| k.as_str().cmp(n))
                            .ok()?;
                        Some(self.props[i].clone())
                    })
                    .collect();
                v.sort();
                v
            },
            props_quoted: self.o.mangle_quoted,
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
        let js = |m: ModuleId| l.included[m as usize];
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
                let cssm = css_roots(g, e, true);
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
            }
        }
        // Every entry chunk gets the stylesheets its static imports reach; shared chunks
        // have none (esbuild does not split CSS).
        for c in chunks.iter_mut() {
            if let Some(e) = c.entry {
                c.css = css_roots(g, e, false);
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

/// The stylesheets an entry uses, in the order JavaScript first imports them (each once;
/// `css::order` expands their `@import`s): a CSS entry is its own root. Observed in
/// esbuild: `a.js` importing `x.css` and `y.css`, then `b.js` importing `z.css` and `x.css`,
/// orders `x`, `y`, `z`.
pub fn css_roots(g: &Graph, e: ModuleId, dynamic: bool) -> Vec<ModuleId> {
    if g.modules[e as usize].loader.is_css() {
        return vec![e];
    }
    let mut seen = vec![false; g.modules.len()];
    let mut roots = Vec::new();
    // Depth-first in import order, iteratively: (module, next record).
    let mut stack = vec![(e, 0usize)];
    seen[e as usize] = true;
    while let Some(&mut (m, ref mut next)) = stack.last_mut() {
        let module = &g.modules[m as usize];
        if *next >= module.targets.len() {
            stack.pop();
            continue;
        }
        let i = *next;
        *next += 1;
        let kind = module.meta.records[i].kind;
        if kind.is_css() || (!dynamic && kind == RecordKind::Dynamic) {
            continue;
        }
        // Through JavaScript that tree shaking dropped too (see `link`).
        if let Target::Module(u) = module.targets[i]
            && !seen[u as usize]
        {
            seen[u as usize] = true;
            if g.modules[u as usize].loader.is_css() {
                roots.push(u);
            } else {
                stack.push((u, 0));
            }
        }
    }
    roots
}

/// Modules reachable from `e` over included targets (`dynamic`: follow `import()` too).
fn reachable(g: &Graph, l: &Link, e: ModuleId, dynamic: bool) -> Vec<bool> {
    let mut seen = vec![false; g.modules.len()];
    let mut stack = vec![e];
    seen[e as usize] = true;
    while let Some(m) = stack.pop() {
        let module = &g.modules[m as usize];
        for (i, t) in module.targets.iter().enumerate() {
            let kind = module.meta.records[i].kind;
            if kind.is_css() || (!dynamic && kind == RecordKind::Dynamic) {
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

/// The import path from one chunk to another (both relative to the output directory):
/// under `public_path` when one is set (esbuild prefixes every cross-chunk import with
/// it, as it does file-loader URLs), else [`relative_import`].
pub fn chunk_import(from: &str, to: &str, public_path: &str) -> String {
    if public_path.is_empty() {
        relative_import(from, to)
    } else {
        format!("{}/{to}", public_path.trim_end_matches('/'))
    }
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
fn external_order<'g>(
    g: &'g Graph,
    entry: Option<ModuleId>,
    mods: &[ModuleId],
) -> Vec<ExternalAt<'g>> {
    let mut order: Vec<ExternalAt<'g>> = Vec::new();
    let mut seen: FxHashSet<&'g str> = FxHashSet::default();
    let mut visited = vec![false; g.modules.len()];
    // Externals waiting for the next module whose code ends (post-order).
    let mut pending = 0;
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
                for e in &mut order[pending..] {
                    e.before = m;
                }
                pending = order.len();
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
                        order.push(ExternalAt {
                            path: x.as_str(),
                            importer: m,
                            before: m,
                        });
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

/// An external in evaluation order: the module that loads it first, and the module before
/// whose code it loads (the next one whose code ends, in evaluation order).
struct ExternalAt<'g> {
    path: &'g str,
    importer: ModuleId,
    before: ModuleId,
}
