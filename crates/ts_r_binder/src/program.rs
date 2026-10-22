//! Program-level linking: the merged global table, ambient modules, module links through
//! `ts_r_resolver`, module augmentations, UMD globals and export resolution.
//!
//! Binding stays per file; this pass only reads bound files. See `docs/design/0003-binder.md`
//! ("Program-level linking").

use rustc_hash::{FxHashMap, FxHashSet};
use ts_r_ast::{Ast, Node, NodeId};
use ts_r_config::FileSystem;
use ts_r_diagnostics::{Diagnostic, FileId};
use ts_r_resolver::Resolver;
use ts_r_resolver::resolver::ImportKind;

use crate::messages as m;
use crate::symbols::{SymbolFacts, SymbolFlags, SymbolId, TableId, excludes};
use crate::{AliasTarget, BoundFile, ImportName, ModuleRequest, RequestKind};

/// One input file of a program. Files are linked in the order given (lib files first).
#[derive(Clone, Copy)]
pub struct ProgramFile<'a> {
    pub path: &'a str,
    pub ast: &'a Ast,
    pub src: &'a str,
    pub bound: &'a BoundFile,
}

/// A symbol of a particular file.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct SymbolRef {
    pub file: FileId,
    pub symbol: SymbolId,
}

/// Where a module specifier leads.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ModuleTarget {
    /// A file of the program.
    File(FileId),
    /// An ambient module: index into [`Program::ambient_modules`].
    Ambient(u32),
    Unresolved,
}

/// Global or ambient-module symbols merged across files.
#[derive(Clone, Debug, Default)]
pub struct Merged {
    pub flags: SymbolFlags,
    /// Accumulated excludes of the merged declarations.
    pub excludes: SymbolFlags,
    /// Contributing symbols in file order.
    pub symbols: Vec<SymbolRef>,
}

impl std::fmt::Debug for ProgramFile<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProgramFile")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

/// The linked program.
#[derive(Debug)]
pub struct Program<'a> {
    pub files: Vec<ProgramFile<'a>>,
    /// Merged globals, sorted by name.
    pub globals: Vec<(Box<str>, Merged)>,
    /// `declare module "x"` declarations merged by name, sorted by name (patterns included).
    pub ambient_modules: Vec<(Box<str>, Merged)>,
    /// Per file, per module request: the target.
    pub links: Vec<Vec<ModuleTarget>>,
    /// Module augmentations: the augmenting symbol and the module it augments.
    pub augmentations: Vec<(SymbolRef, ModuleTarget)>,
    /// Augmentation exports of names the augmented module re-exports (`export { Foo } from`,
    /// `export *`), each with the declaration it merges with instead of the re-export.
    pub augmentation_redirects: Vec<(SymbolRef, SymbolRef)>,
    /// Program-level diagnostics, each with its file set.
    pub diagnostics: Vec<Diagnostic>,
}

/// Union of the excludes of every meaning in `flags` (equals the accumulated excludes of a
/// merged symbol).
pub fn excludes_of(flags: SymbolFlags) -> SymbolFlags {
    let table: [(SymbolFlags, SymbolFlags); 17] = [
        (SymbolFlags::PROPERTY, excludes::PROPERTY),
        (SymbolFlags::METHOD, excludes::METHOD),
        (SymbolFlags::GET_ACCESSOR, excludes::GET_ACCESSOR),
        (SymbolFlags::SET_ACCESSOR, excludes::SET_ACCESSOR),
        (SymbolFlags::ENUM_MEMBER, excludes::ENUM_MEMBER),
        (SymbolFlags::PARAMETER, excludes::PARAMETER),
        (
            SymbolFlags::FUNCTION_SCOPED_VARIABLE,
            excludes::FUNCTION_SCOPED_VARIABLE,
        ),
        (
            SymbolFlags::BLOCK_SCOPED_VARIABLE,
            excludes::BLOCK_SCOPED_VARIABLE,
        ),
        (SymbolFlags::FUNCTION, excludes::FUNCTION),
        (SymbolFlags::CLASS, excludes::CLASS),
        (SymbolFlags::INTERFACE, excludes::INTERFACE),
        (SymbolFlags::TYPE_ALIAS, excludes::TYPE_ALIAS),
        (SymbolFlags::REGULAR_ENUM, excludes::REGULAR_ENUM),
        (SymbolFlags::CONST_ENUM, excludes::CONST_ENUM),
        (SymbolFlags::VALUE_MODULE, excludes::VALUE_MODULE),
        (SymbolFlags::ALIAS, excludes::ALIAS),
        (SymbolFlags::EXPORT_EXPRESSION, excludes::EXPORT_EXPRESSION),
    ];
    let mut out = SymbolFlags::NONE;
    for (f, e) in table {
        if flags.intersects(f) {
            out |= e;
        }
    }
    if flags.intersects(SymbolFlags::PARAMETER) {
        // A parameter is also a function-scoped variable but only excludes parameters.
        out = excludes::PARAMETER;
    }
    out
}

/// Excludes for interface/type-literal members merged across declarations.
fn member_excludes_of(flags: SymbolFlags) -> SymbolFlags {
    let mut out = SymbolFlags::NONE;
    if flags.intersects(SymbolFlags::PROPERTY) {
        out |= excludes::PROPERTY_SIGNATURE;
    }
    if flags.intersects(SymbolFlags::METHOD) {
        out |= excludes::METHOD;
    }
    if flags.intersects(SymbolFlags::GET_ACCESSOR) {
        out |= excludes::GET_ACCESSOR;
    }
    if flags.intersects(SymbolFlags::SET_ACCESSOR) {
        out |= excludes::SET_ACCESSOR;
    }
    out
}

/// A same-named contribution to a merge.
struct Item {
    name: Box<str>,
    r: SymbolRef,
    umd: bool,
    /// From a `declare global` block.
    augment: bool,
}

impl Item {
    fn new(name: &str, file: FileId, symbol: SymbolId, umd: bool) -> Item {
        Item {
            name: name.into(),
            r: SymbolRef { file, symbol },
            umd,
            augment: false,
        }
    }
}

fn kind_for(kind: RequestKind) -> ImportKind {
    match kind {
        RequestKind::DynamicImport => ImportKind::Dynamic,
        RequestKind::ImportEquals => ImportKind::ImportEquals,
        _ => ImportKind::Static,
    }
}

fn is_relative(spec: &str) -> bool {
    spec.starts_with("./")
        || spec.starts_with("../")
        || spec == "."
        || spec == ".."
        || spec.starts_with('/')
}

/// Bound on alias / export-star chains (cycles are also detected).
const MAX_CHAIN: usize = 128;

/// Bound on the modules one `export *` search visits (each at most once): wide barrels
/// re-export far more than `MAX_CHAIN` modules (@sinclair/typebox: 71 `export *` of
/// `index` files that each `export *` again). The search recurses, so its depth stays
/// bounded by `MAX_CHAIN` separately.
const MAX_STAR_MODULES: usize = 1 << 16;

impl<'a> Program<'a> {
    /// Links with a `ts_r_resolver` resolver.
    pub fn link_with<F: FileSystem>(
        files: Vec<ProgramFile<'a>>,
        resolver: &Resolver<F>,
    ) -> Program<'a> {
        Program::link(files, |spec, containing, req| {
            let mode = req
                .resolution_mode
                .unwrap_or_else(|| resolver.mode_for(containing, kind_for(req.kind)));
            resolver
                .resolve_module(spec, containing, mode)
                .map(|r| r.resolved_file_name.clone())
        })
    }

    /// Links with a custom resolution function (specifier, containing file, request) → path.
    pub fn link(
        files: Vec<ProgramFile<'a>>,
        resolve: impl Fn(&str, &str, &ModuleRequest) -> Option<String>,
    ) -> Program<'a> {
        let mut p = Program {
            files,
            globals: Vec::new(),
            ambient_modules: Vec::new(),
            links: Vec::new(),
            augmentations: Vec::new(),
            augmentation_redirects: Vec::new(),
            diagnostics: Vec::new(),
        };
        p.merge_globals();
        p.merge_ambient_modules();
        p.link_modules(&resolve);
        p.check_augmented_export_equals();
        p.merge_modules();
        p.diagnostics.sort_by(|a, b| {
            (a.file, a.span.start, a.code, &a.message).cmp(&(
                b.file,
                b.span.start,
                b.code,
                &b.message,
            ))
        });
        p.diagnostics.dedup_by(|a, b| {
            a.file == b.file && a.span == b.span && a.code == b.code && a.message == b.message
        });
        p
    }

    fn file(&self, f: FileId) -> &ProgramFile<'a> {
        &self.files[f.0 as usize]
    }

    fn merge_globals(&mut self) {
        let mut items = Vec::new();
        // `declare global` blocks merge after every script's globals, as in tsc (the
        // declaration order `Parameters<typeof setTimeout>` sees).
        let mut augments = Vec::new();
        for (fi, f) in self.files.iter().enumerate() {
            let file = FileId(fi as u32);
            let b = f.bound;
            if !b.is_module {
                for &(n, s) in b.script_globals() {
                    items.push(Item::new(b.name_text(f.ast, n), file, s, false));
                }
            }
            for &(n, s) in b.table(b.global_augmentations) {
                // `declare global { var globalThis }` augments rather than conflicts.
                let mut it = Item::new(b.name_text(f.ast, n), file, s, false);
                it.augment = true;
                augments.push(it);
            }
            if b.is_module
                && let Some((n, _)) = b.umd_name
            {
                items.push(Item::new(b.name_text(f.ast, n), file, b.file_symbol, true));
            }
        }
        items.extend(augments);
        let mut diags = Vec::new();
        for it in &items {
            if !it.umd && !it.augment && &*it.name == "globalThis" {
                for (file, span) in self.decl_spans(it.r, false) {
                    diags.push(
                        Diagnostic::new(m::CONFLICTS_WITH_GLOBAL_THIS, span, &["globalThis"])
                            .with_file(file),
                    );
                }
            }
            // Script-level variables, functions and namespaces named `undefined`.
            if !it.umd && !it.augment && &*it.name == "undefined" {
                let f = self.file(it.r.file);
                for &d in f.bound.decls(it.r.symbol) {
                    if matches!(
                        f.ast.node(d),
                        Node::Ident { .. }
                            | Node::VarDeclarator { .. }
                            | Node::FunctionDecl { .. }
                            | Node::Module { .. }
                    ) {
                        let span = decl_span(f.ast, d);
                        diags.push(
                            Diagnostic::new(m::CONFLICTS_WITH_GLOBAL_THIS, span, &["undefined"])
                                .with_file(it.r.file),
                        );
                    }
                }
            }
        }
        let map = self.merge_level(items, false, false, &mut diags, 0, &[]);
        let mut globals: Vec<(Box<str>, Merged)> = map.into_iter().collect();
        globals.sort_by(|a, b| a.0.cmp(&b.0));
        self.globals = globals;
        self.diagnostics.extend(diags);
    }

    /// Spans to report a contribution at: its declaration names (the `export as namespace`
    /// name for a UMD global).
    fn decl_spans(&self, r: SymbolRef, umd: bool) -> Vec<(FileId, ts_r_ast::Span)> {
        let f = self.file(r.file);
        if umd {
            return f
                .bound
                .umd_name
                .map(|(_, node)| {
                    let n = crate::decl_name_node(f.ast, node).unwrap_or(node);
                    vec![(r.file, f.ast.span(n))]
                })
                .unwrap_or_default();
        }
        f.bound
            .decls(r.symbol)
            .iter()
            .map(|&d| (r.file, decl_span(f.ast, d)))
            .collect()
    }

    /// Merges same-named contributions from different symbols (files, augmentations) in order,
    /// reporting conflicts on every declaration involved, then merges the exports (namespaces,
    /// classes, enums, modules) and interface members of whatever merged.
    /// `alias_clash`: an alias conflicts with any declaration (module augmentation merges).
    fn merge_level(
        &self,
        items: Vec<Item>,
        members: bool,
        alias_clash: bool,
        diags: &mut Vec<Diagnostic>,
        depth: u32,
        report_as: &[(SymbolRef, SymbolRef)],
    ) -> FxHashMap<Box<str>, Merged> {
        let mut map: FxHashMap<Box<str>, Merged> = FxHashMap::default();
        let mut umd_refs: FxHashSet<SymbolRef> = FxHashSet::default();
        let mut script_names: FxHashSet<Box<str>> = FxHashSet::default();
        for it in items {
            let flags = if it.umd {
                SymbolFlags::VALUE_MODULE | SymbolFlags::NAMESPACE_MODULE
            } else {
                self.file(it.r.file).bound.symbol(it.r.symbol).flags
            };
            let excl = if members {
                member_excludes_of(flags)
            } else {
                excludes_of(flags)
            };
            let entry = map.entry(it.name.clone()).or_default();
            if entry.symbols.contains(&it.r) {
                continue;
            }
            // Expando properties (`f.x = 1`) never conflict: a declared member of the same
            // name (a merged namespace's export) is the member.
            if !it.umd {
                let expando = |r: &SymbolRef| {
                    self.file(r.file)
                        .bound
                        .symbol(r.symbol)
                        .facts
                        .intersects(SymbolFacts::EXPANDO)
                };
                if !entry.symbols.is_empty() {
                    let incoming = expando(&it.r);
                    let existing = entry.symbols.iter().all(expando);
                    if incoming && !existing {
                        continue;
                    }
                    if existing && !incoming {
                        *entry = Merged::default();
                    }
                }
            }
            // `export as namespace X` is dropped when an earlier script file (lib files
            // first) already declares a global `X` with any meaning (observed: lie's
            // `export as namespace Promise` is no error; a later `declare var X` still is).
            // `declare global` blocks merge after every UMD name, so they still conflict.
            if it.umd && script_names.contains(&it.name) {
                continue;
            }
            if !it.umd && !it.augment {
                script_names.insert(it.name.clone());
            }
            // Augmenting a re-exported name: the alias never merges with a declaration.
            let alias_clash = alias_clash
                && !entry.symbols.is_empty()
                && (flags.intersects(SymbolFlags::ALIAS)
                    != entry.flags.intersects(SymbolFlags::ALIAS));
            if !entry.symbols.is_empty()
                && (alias_clash || entry.flags.intersects(excl) || entry.excludes.intersects(flags))
            {
                let enum_conflict = (flags.intersects(SymbolFlags::ENUM)
                    || entry.flags.intersects(SymbolFlags::ENUM))
                    && !flags.intersects(SymbolFlags::ENUM_MEMBER);
                let msg = if enum_conflict {
                    m::ENUM_MERGE_CONFLICT
                } else if flags.intersects(SymbolFlags::BLOCK_SCOPED_VARIABLE)
                    || entry.flags.intersects(SymbolFlags::BLOCK_SCOPED_VARIABLE)
                {
                    m::CANNOT_REDECLARE_BLOCK_SCOPED
                } else {
                    m::DUPLICATE_IDENTIFIER
                };
                let args: &[&str] = if enum_conflict { &[] } else { &[&it.name] };
                let mut all = entry.symbols.clone();
                all.push(it.r);
                for r in all {
                    let umd = umd_refs.contains(&r) || (r == it.r && it.umd);
                    // A declaration reached through a named re-export is reported there.
                    let r = report_as.iter().find(|x| x.0 == r).map_or(r, |x| x.1);
                    for (file, span) in self.decl_spans(r, umd) {
                        diags.push(Diagnostic::new(msg, span, args).with_file(file));
                    }
                }
                continue;
            }
            if it.umd {
                umd_refs.insert(it.r);
            }
            entry.flags |= flags;
            entry.excludes |= excl;
            entry.symbols.push(it.r);
        }
        if depth >= 32 {
            return map;
        }
        let mut names: Vec<&Box<str>> = map
            .iter()
            .filter(|(_, m)| m.symbols.len() > 1)
            .map(|(n, _)| n)
            .collect();
        names.sort();
        for name in names {
            let merged = &map[name];
            if merged
                .flags
                .intersects(SymbolFlags::NAMESPACE | SymbolFlags::CLASS)
            {
                let mut items = Vec::new();
                for &r in &merged.symbols {
                    let f = self.file(r.file);
                    let t = f.bound.symbol(r.symbol).exports;
                    for &(n, s) in f.bound.table(t) {
                        items.push(Item::new(f.bound.name_text(f.ast, n), r.file, s, false));
                    }
                }
                self.merge_level(items, false, false, diags, depth + 1, &[]);
            }
            if merged.flags.intersects(SymbolFlags::INTERFACE) {
                let mut items = Vec::new();
                for &r in &merged.symbols {
                    let f = self.file(r.file);
                    for t in f.bound.member_tables(r.symbol) {
                        for &(n, s) in f.bound.table(t) {
                            items.push(Item::new(f.bound.name_text(f.ast, n), r.file, s, false));
                        }
                    }
                }
                self.merge_level(items, true, false, diags, depth + 1, &[]);
            }
        }
        map
    }

    /// TS2671: a module whose `export =` names an entity without namespace meaning cannot
    /// be augmented; the augmentation is dropped.
    fn check_augmented_export_equals(&mut self) {
        let mut diags = Vec::new();
        let mut dropped = Vec::new();
        for (i, &(aug, target)) in self.augmentations.iter().enumerate() {
            if target == ModuleTarget::Unresolved {
                continue;
            }
            let own = self
                .module_symbols(target)
                .into_iter()
                .filter(|m| !self.augmentations.iter().any(|a| a.0 == *m))
                .find_map(|m| self.lookup_in(m, |b| b.symbol(m.symbol).exports, "export="));
            let Some(eq) = own else { continue };
            let Some(entity) = self.resolve_alias(eq) else {
                continue;
            };
            // Observed (tsc 7 CLI): an entity without namespace meaning is reported (a
            // variable, a class); an enum or a function or class merged with a namespace is
            // not. A lone function is left alone: tsc reports it too, but the conformance
            // baseline `augmentExportEquals2` does not.
            let ef = self.file(entity.file);
            let flags = ef.bound.symbol(entity.symbol).flags;
            if flags.intersects(SymbolFlags::NAMESPACE | SymbolFlags::FUNCTION)
                || !flags.intersects(SymbolFlags::VARIABLE | SymbolFlags::CLASS)
            {
                continue;
            }
            let f = self.file(aug.file);
            for &d in f.bound.decls(aug.symbol) {
                if let Node::Module { name, .. } = *f.ast.node(d)
                    && let Node::StringLit { value } = *f.ast.node(name)
                {
                    diags.push(
                        Diagnostic::new(
                            m::AUGMENT_NON_MODULE_ENTITY,
                            f.ast.span(name),
                            &[f.ast.atom(value)],
                        )
                        .with_file(aug.file),
                    );
                }
            }
            dropped.push(i);
        }
        for i in dropped.into_iter().rev() {
            self.augmentations[i].1 = ModuleTarget::Unresolved;
        }
        self.diagnostics.extend(diags);
    }

    /// Merges the exports of modules declared or augmented in several places.
    fn merge_modules(&mut self) {
        let mut targets: Vec<ModuleTarget> = self.augmentations.iter().map(|&(_, t)| t).collect();
        for (i, (_, m)) in self.ambient_modules.iter().enumerate() {
            if m.symbols.len() > 1 {
                targets.push(ModuleTarget::Ambient(i as u32));
            }
        }
        let mut seen = Vec::new();
        let mut diags = Vec::new();
        for t in targets {
            if t == ModuleTarget::Unresolved || seen.contains(&t) {
                continue;
            }
            seen.push(t);
            let mut items: Vec<Item> = Vec::new();
            let mut own: FxHashMap<Box<str>, SymbolRef> = FxHashMap::default();
            // Re-exporting aliases an augmentation redirects to their declaration.
            let mut redirected_aliases: Vec<SymbolRef> = Vec::new();
            let mut redirects: Vec<(SymbolRef, SymbolRef)> = Vec::new();
            let mut report_as: Vec<(SymbolRef, SymbolRef)> = Vec::new();
            let augs: Vec<SymbolRef> = self
                .augmentations
                .iter()
                .filter(|a| a.1 == t)
                .map(|a| a.0)
                .collect();
            // A module whose `export =` is a namespace: augmentations merge with that
            // namespace's exports (and through its re-exports, with their declarations).
            if let Some(eq) = self.resolve_export(t, "export=")
                && let Some(entity) = self.resolve_alias(eq)
                && !augs.contains(&entity)
            {
                let ef = self.file(entity.file);
                if ef
                    .bound
                    .symbol(entity.symbol)
                    .flags
                    .intersects(SymbolFlags::NAMESPACE_MODULE | SymbolFlags::VALUE_MODULE)
                {
                    let et = ef.bound.symbol(entity.symbol).exports;
                    for &(n, s) in ef.bound.table(et) {
                        own.entry(ef.bound.name_text(ef.ast, n).into())
                            .or_insert(SymbolRef {
                                file: entity.file,
                                symbol: s,
                            });
                    }
                }
            }
            for r in self.module_symbols(t) {
                let f = self.file(r.file);
                let et = f.bound.symbol(r.symbol).exports;
                let is_aug = augs.contains(&r);
                for &(n, s) in f.bound.table(et) {
                    let text = f.bound.name_text(f.ast, n);
                    let here = SymbolRef {
                        file: r.file,
                        symbol: s,
                    };
                    if !is_aug {
                        own.entry(text.into()).or_insert(here);
                    } else {
                        // An augmentation of a name the module re-exports merges with the
                        // re-exported declaration, not with the re-export.
                        let reexport = match own.get(text) {
                            Some(&o) => {
                                let of = self.file(o.file);
                                if of
                                    .bound
                                    .symbol(o.symbol)
                                    .flags
                                    .intersects(SymbolFlags::ALIAS)
                                {
                                    Some((o, self.resolve_alias(o)))
                                } else {
                                    None
                                }
                            }
                            None => self
                                .resolve_star_export(t, text)
                                .map(|x| (x, self.resolve_alias(x))),
                        };
                        if let Some((alias, Some(target))) = reexport {
                            let tf = self.file(target.file);
                            let tflags = tf.bound.symbol(target.symbol).flags;
                            if !tflags.intersects(SymbolFlags::ALIAS) {
                                if !items.iter().any(|i| i.r == target && &*i.name == text) {
                                    items.push(Item::new(text, target.file, target.symbol, false));
                                }
                                if alias != target {
                                    redirected_aliases.push(alias);
                                    if own.contains_key(text) {
                                        report_as.push((target, alias));
                                    }
                                }
                                redirects.push((here, target));
                            }
                        }
                    }
                    items.push(Item::new(text, r.file, s, false));
                }
            }
            items.retain(|i| !redirected_aliases.contains(&i.r));
            self.merge_level(items, false, true, &mut diags, 0, &report_as);
            self.augmentation_redirects.extend(redirects);
        }
        self.augmentation_redirects.sort();
        self.augmentation_redirects.dedup();
        self.diagnostics.extend(diags);
    }

    fn merge_ambient_modules(&mut self) {
        let mut map: FxHashMap<Box<str>, Merged> = FxHashMap::default();
        for (fi, f) in self.files.iter().enumerate() {
            let b = f.bound;
            for &(n, s) in b.table(b.ambient_modules) {
                let e = map.entry(b.name_text(f.ast, n).into()).or_default();
                e.flags |= b.symbol(s).flags;
                e.symbols.push(SymbolRef {
                    file: FileId(fi as u32),
                    symbol: s,
                });
            }
        }
        let mut v: Vec<(Box<str>, Merged)> = map.into_iter().collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        self.ambient_modules = v;
    }

    /// Finds an ambient module by exact name, then by the `*` pattern with the longest prefix.
    pub fn find_ambient_module(&self, spec: &str) -> Option<u32> {
        if let Ok(i) = self
            .ambient_modules
            .binary_search_by(|(n, _)| (**n).cmp(spec))
        {
            return Some(i as u32);
        }
        let mut best: Option<(usize, u32)> = None;
        for (i, (n, _)) in self.ambient_modules.iter().enumerate() {
            if let Some((pre, suf)) = n.split_once('*')
                && spec.len() >= pre.len() + suf.len()
                && spec.starts_with(pre)
                && spec.ends_with(suf)
                && best.is_none_or(|(l, _)| pre.len() > l)
            {
                best = Some((pre.len(), i as u32));
            }
        }
        best.map(|(_, i)| i)
    }

    fn link_modules(&mut self, resolve: &impl Fn(&str, &str, &ModuleRequest) -> Option<String>) {
        let index: FxHashMap<&str, FileId> = self
            .files
            .iter()
            .enumerate()
            .map(|(i, f)| (f.path, FileId(i as u32)))
            .collect();
        let mut links = Vec::with_capacity(self.files.len());
        let mut augs = Vec::new();
        let mut diags = Vec::new();
        for (fi, f) in self.files.iter().enumerate() {
            let b = f.bound;
            let mut v = Vec::with_capacity(b.module_requests.len());
            for r in &b.module_requests {
                let spec = f.ast.atom(r.specifier);
                let mut target = ModuleTarget::Unresolved;
                if !is_relative(spec)
                    && let Ok(i) = self
                        .ambient_modules
                        .binary_search_by(|(n, _)| (**n).cmp(spec))
                {
                    target = ModuleTarget::Ambient(i as u32);
                }
                // Relative specifiers inside an ambient module declaration are never followed
                // (observed: `export * from "./a"` there re-exports nothing, and
                // `import x = require("./a")` is TS2307 even when `a.d.ts` exists).
                // In an augmentation, a relative specifier is only found when the file
                // imports it elsewhere too (observed: TS2307 otherwise).
                let unlisted = || {
                    !b.module_requests.iter().any(|o| {
                        !o.in_augmentation
                            && !o.in_ambient_module
                            && f.ast.atom(o.specifier) == spec
                    })
                };
                if target == ModuleTarget::Unresolved
                    && !(r.in_ambient_module && is_relative(spec))
                    && !(r.in_augmentation && is_relative(spec) && unlisted())
                    && let Some(path) = resolve(spec, f.path, r)
                    && let Some(&t) = index.get(path.as_str())
                {
                    target = ModuleTarget::File(t);
                }
                if target == ModuleTarget::Unresolved
                    && !is_relative(spec)
                    && let Some(i) = self.find_ambient_module(spec)
                {
                    target = ModuleTarget::Ambient(i);
                }
                v.push(target);
            }
            // Augmentations (`declare module "x"` in a module).
            for &(_, s) in b.table(b.module_augmentations) {
                // Match the symbol to its request by a declaration's name node. Nested
                // augmentations (already TS2435) have no request and are not resolved.
                let req_of = |d: NodeId| match *f.ast.node(d) {
                    Node::Module { name, .. } => {
                        b.module_requests.iter().position(|r| r.node == name)
                    }
                    _ => None,
                };
                let Some(req) = b.decls(s).iter().find_map(|&d| req_of(d)) else {
                    continue;
                };
                let target = v[req];
                // Declaration files tolerate unresolved augmentations.
                if target == ModuleTarget::Unresolved && !f.ast.source_kind.is_declaration() {
                    for &d in b.decls(s) {
                        if req_of(d).is_some()
                            && let Node::Module { name, .. } = *f.ast.node(d)
                        {
                            let text = match *f.ast.node(name) {
                                Node::StringLit { value } => f.ast.atom(value),
                                _ => "",
                            };
                            diags.push(
                                Diagnostic::new(
                                    m::INVALID_AUGMENTATION_MODULE,
                                    f.ast.span(name),
                                    &[text],
                                )
                                .with_file(FileId(fi as u32)),
                            );
                            // Once per augmented name in a file (observed with tsc 7).
                            break;
                        }
                    }
                }
                augs.push((
                    SymbolRef {
                        file: FileId(fi as u32),
                        symbol: s,
                    },
                    target,
                ));
            }
            links.push(v);
        }
        self.links = links;
        self.augmentations = augs;
        self.diagnostics.extend(diags);
    }

    // -------------------------------------------------------------------------------------
    // Queries
    // -------------------------------------------------------------------------------------

    /// The merged global named `name`.
    pub fn global(&self, name: &str) -> Option<&Merged> {
        self.globals
            .binary_search_by(|(n, _)| (**n).cmp(name))
            .ok()
            .map(|i| &self.globals[i].1)
    }

    /// The target of module request `request` of `file`.
    pub fn module_target(&self, file: FileId, request: u32) -> ModuleTarget {
        self.links
            .get(file.0 as usize)
            .and_then(|v| v.get(request as usize))
            .copied()
            .unwrap_or(ModuleTarget::Unresolved)
    }

    /// The module symbols of a target (one per file for an ambient module; augmentations
    /// included).
    /// Whether `target` is a program file that is not a module (no `import`/`export`, not a
    /// JavaScript CommonJS module, not JSON): importing it as a whole resolves to nothing.
    pub fn is_script_target(&self, target: ModuleTarget) -> bool {
        let ModuleTarget::File(f) = target else {
            return false;
        };
        let file = self.file(f);
        let json = file.path.len() >= 5
            && file.path.as_bytes()[file.path.len() - 5..].eq_ignore_ascii_case(b".json");
        !file.bound.is_module && !file.bound.commonjs && !json
    }

    pub fn module_symbols(&self, target: ModuleTarget) -> Vec<SymbolRef> {
        let mut out = match target {
            ModuleTarget::File(f) => vec![SymbolRef {
                file: f,
                symbol: self.file(f).bound.file_symbol,
            }],
            ModuleTarget::Ambient(i) => self
                .ambient_modules
                .get(i as usize)
                .map(|(_, m)| m.symbols.clone())
                .unwrap_or_default(),
            ModuleTarget::Unresolved => Vec::new(),
        };
        if target != ModuleTarget::Unresolved {
            for &(s, t) in &self.augmentations {
                if t == target {
                    out.push(s);
                }
            }
        }
        out
    }

    fn lookup_in(
        &self,
        sym: SymbolRef,
        table: impl Fn(&BoundFile) -> TableId,
        name: &str,
    ) -> Option<SymbolRef> {
        let f = self.file(sym.file);
        let t = table(f.bound);
        f.bound.lookup_str(f.ast, t, name).map(|s| SymbolRef {
            file: sym.file,
            symbol: s,
        })
    }

    fn exports_table(&self, sym: SymbolRef) -> TableId {
        self.file(sym.file).bound.symbol(sym.symbol).exports
    }

    /// Resolves export `name` of a module target, without resolving the alias it finds.
    /// Own exports shadow `export *`; `default` is never re-exported by `export *`.
    pub fn resolve_export(&self, target: ModuleTarget, name: &str) -> Option<SymbolRef> {
        self.resolve_export_in(target, name, &mut Vec::new())
    }

    /// `resolve_export` inside an alias resolution (`active`: see `resolve_alias_in`).
    fn resolve_export_in(
        &self,
        target: ModuleTarget,
        name: &str,
        active: &mut Vec<SymbolRef>,
    ) -> Option<SymbolRef> {
        let mut visited = FxHashSet::default();
        self.resolve_export_inner(target, name, &mut visited, 0, active)
    }

    fn resolve_export_inner(
        &self,
        target: ModuleTarget,
        name: &str,
        visited: &mut FxHashSet<(u32, u32)>,
        depth: usize,
        active: &mut Vec<SymbolRef>,
    ) -> Option<SymbolRef> {
        let key = match target {
            ModuleTarget::File(f) => (0, f.0),
            ModuleTarget::Ambient(i) => (1, i),
            ModuleTarget::Unresolved => return None,
        };
        if depth > MAX_CHAIN || !visited.insert(key) || visited.len() > MAX_STAR_MODULES {
            return None;
        }
        let mods = self.module_symbols(target);
        for &m in &mods {
            let t = self.exports_table(m);
            if let Some(s) = self.lookup_in(m, |_| t, name) {
                return Some(s);
            }
        }
        if name != "default" {
            for &m in &mods {
                let f = self.file(m.file);
                for star in &f.bound.export_stars {
                    if star.container != m.symbol {
                        continue;
                    }
                    let t = self.module_target(m.file, star.request);
                    if let Some(s) = self.resolve_export_inner(t, name, visited, depth + 1, active)
                    {
                        return Some(s);
                    }
                }
            }
        }
        // `export =` of a namespace-like entity exposes its exports. A default import of such
        // a module is the entity itself (synthetic default), not a member named `default`.
        if name == "default" {
            return None;
        }
        for &m in &mods {
            let t = self.exports_table(m);
            if let Some(eq) = self.lookup_in(m, |_| t, "export=")
                && let Some(target) = self.resolve_alias_in(eq, active)
            {
                let et = self.exports_table(target);
                if let Some(s) = self.lookup_in(target, |_| et, name) {
                    return Some(s);
                }
            }
        }
        None
    }

    /// Export `name` of a module found only through its `export *` declarations (not its own
    /// exports or augmentations).
    fn resolve_star_export(&self, target: ModuleTarget, name: &str) -> Option<SymbolRef> {
        if name == "default" {
            return None;
        }
        let augs: Vec<SymbolRef> = self
            .augmentations
            .iter()
            .filter(|a| a.1 == target)
            .map(|a| a.0)
            .collect();
        let mut visited = FxHashSet::default();
        for m in self.module_symbols(target) {
            if augs.contains(&m) {
                continue;
            }
            let f = self.file(m.file);
            for star in &f.bound.export_stars {
                if star.container == m.symbol {
                    let t = self.module_target(m.file, star.request);
                    if let Some(s) =
                        self.resolve_export_inner(t, name, &mut visited, 1, &mut Vec::new())
                    {
                        return Some(s);
                    }
                }
            }
        }
        None
    }

    /// Whether export `name` of a module is found only through `export type *` (at any depth
    /// of `export *` declarations), so that it can only be used as a type. One plain
    /// `export *` providing the name too makes it a value.
    /// (tsc: tsc/internal/checker/checker.go getExportsOfModuleWorker, its type-only star map.)
    pub fn star_export_is_type_only(&self, target: ModuleTarget, name: &str) -> bool {
        // Every path counts (the same module may be re-exported both ways), so `path` holds
        // only the modules being visited, and `budget` bounds the total work.
        let mut budget = MAX_STAR_MODULES;
        self.star_type_only_inner(target, name, &mut Vec::new(), &mut budget) == Some(true)
    }

    /// `Some(type_only)` when `name` resolves in `target`, `None` when it doesn't.
    fn star_type_only_inner(
        &self,
        target: ModuleTarget,
        name: &str,
        path: &mut Vec<(u32, u32)>,
        budget: &mut usize,
    ) -> Option<bool> {
        let key = match target {
            ModuleTarget::File(f) => (0, f.0),
            ModuleTarget::Ambient(i) => (1, i),
            ModuleTarget::Unresolved => return None,
        };
        if *budget == 0 || path.len() > MAX_CHAIN || path.contains(&key) {
            return None;
        }
        *budget -= 1;
        path.push(key);
        let r = self.star_type_only_step(target, name, path, budget);
        path.pop();
        r
    }

    fn star_type_only_step(
        &self,
        target: ModuleTarget,
        name: &str,
        path: &mut Vec<(u32, u32)>,
        budget: &mut usize,
    ) -> Option<bool> {
        let mods = self.module_symbols(target);
        for &m in &mods {
            let t = self.exports_table(m);
            if self.lookup_in(m, |_| t, name).is_some() {
                return Some(false);
            }
        }
        if name == "default" {
            return None;
        }
        let mut found = None;
        for &m in &mods {
            let f = self.file(m.file);
            for star in &f.bound.export_stars {
                if star.container != m.symbol {
                    continue;
                }
                let t = self.module_target(m.file, star.request);
                if let Some(inner) = self.star_type_only_inner(t, name, path, budget) {
                    let here = f
                        .bound
                        .module_requests
                        .get(star.request as usize)
                        .is_some_and(|r| r.type_only);
                    if !(inner || here) {
                        return Some(false);
                    }
                    found = Some(true);
                }
            }
        }
        found
    }

    /// Every export name of a module target (own, augmentations, and `export *`), sorted.
    pub fn export_names(&self, target: ModuleTarget) -> Vec<(Box<str>, SymbolRef)> {
        let mut out: FxHashMap<Box<str>, SymbolRef> = FxHashMap::default();
        let mut visited = FxHashSet::default();
        self.collect_exports(target, true, &mut out, &mut visited, 0);
        let mut v: Vec<_> = out.into_iter().collect();
        v.sort();
        v
    }

    fn collect_exports(
        &self,
        target: ModuleTarget,
        own: bool,
        out: &mut FxHashMap<Box<str>, SymbolRef>,
        visited: &mut FxHashSet<(u32, u32)>,
        depth: usize,
    ) {
        let key = match target {
            ModuleTarget::File(f) => (0, f.0),
            ModuleTarget::Ambient(i) => (1, i),
            ModuleTarget::Unresolved => return,
        };
        if depth > MAX_CHAIN || !visited.insert(key) || visited.len() > MAX_STAR_MODULES {
            return;
        }
        let mods = self.module_symbols(target);
        for &m in &mods {
            let f = self.file(m.file);
            let t = self.exports_table(m);
            for &(n, s) in f.bound.table(t) {
                let text = f.bound.name_text(f.ast, n);
                if !own && (text == "default" || text == "export=") {
                    continue;
                }
                out.entry(text.into()).or_insert(SymbolRef {
                    file: m.file,
                    symbol: s,
                });
            }
        }
        for &m in &mods {
            let f = self.file(m.file);
            for star in &f.bound.export_stars {
                if star.container == m.symbol {
                    let t = self.module_target(m.file, star.request);
                    self.collect_exports(t, false, out, visited, depth + 1);
                }
            }
        }
    }

    /// Follows an alias chain to a non-alias symbol. Returns the input for non-aliases and
    /// `None` for unresolvable or circular aliases.
    pub fn resolve_alias(&self, sym: SymbolRef) -> Option<SymbolRef> {
        self.resolve_alias_in(sym, &mut Vec::new())
    }

    /// The aliases [`Self::resolve_alias`] follows from `sym`, in order (`sym` first; empty
    /// when `sym` is no alias).
    pub fn alias_chain(&self, sym: SymbolRef) -> Vec<SymbolRef> {
        let mut active = Vec::new();
        let _ = self.follow_alias(sym, &mut active);
        active
    }

    /// `active` holds every alias whose resolution is in progress, in this chain and in the
    /// callers' (an entity name or an `export =` lookup can need another alias resolved
    /// first). Meeting one again is a cycle, and the stack is bounded by `MAX_CHAIN`, which
    /// also bounds the recursion through entity and export resolution.
    fn resolve_alias_in(&self, sym: SymbolRef, active: &mut Vec<SymbolRef>) -> Option<SymbolRef> {
        let base = active.len();
        let r = self.follow_alias(sym, active);
        active.truncate(base);
        r
    }

    /// Whether resolving alias `sym` comes back to `sym` itself (`import A = B; import B =
    /// A;`, a module re-exporting from itself); not an alias that only leads into a cycle.
    pub fn is_circular_alias(&self, sym: SymbolRef) -> bool {
        let mut cycle = false;
        let _ = self.follow_alias_ext(sym, &mut Vec::new(), &mut cycle);
        cycle
    }

    fn follow_alias(&self, sym: SymbolRef, active: &mut Vec<SymbolRef>) -> Option<SymbolRef> {
        self.follow_alias_ext(sym, active, &mut false)
    }

    fn follow_alias_ext(
        &self,
        sym: SymbolRef,
        active: &mut Vec<SymbolRef>,
        cycle: &mut bool,
    ) -> Option<SymbolRef> {
        let mut cur = sym;
        loop {
            let f = self.file(cur.file);
            let s = f.bound.symbol(cur.symbol);
            if !s.flags.intersects(SymbolFlags::ALIAS) {
                return Some(cur);
            }
            if active.contains(&cur) {
                *cycle = active.first() == Some(&cur);
                return None;
            }
            if active.len() >= MAX_CHAIN {
                return None;
            }
            active.push(cur);
            let info = f.bound.alias(cur.symbol)?;
            cur = match info.target {
                AliasTarget::Module { request, import } => {
                    let target = self.module_target(cur.file, request);
                    if matches!(import, ImportName::Namespace | ImportName::ExportEquals)
                        && self.is_script_target(target)
                    {
                        return None;
                    }
                    match import {
                        ImportName::Namespace => {
                            return self.module_symbols(target).first().copied();
                        }
                        ImportName::ExportEquals => {
                            match self.resolve_export_in(target, "export=", active) {
                                Some(s) => s,
                                None => return self.module_symbols(target).first().copied(),
                            }
                        }
                        ImportName::Default => {
                            match self.resolve_export_in(target, "default", active) {
                                Some(s) => s,
                                None => self.resolve_export_in(target, "export=", active)?,
                            }
                        }
                        ImportName::Named(n) => {
                            let text = f.bound.name_text(f.ast, n);
                            self.resolve_export_in(target, text, active)?
                        }
                    }
                }
                AliasTarget::Entity(node) => self.resolve_entity_in(cur.file, node, active)?,
            };
        }
    }

    /// Resolves an entity name (`a`, `A.B`, `A.B.C`) written in `file`.
    pub fn resolve_entity(&self, file: FileId, node: NodeId) -> Option<SymbolRef> {
        self.resolve_entity_in(file, node, &mut Vec::new())
    }

    /// Walks the name iteratively: member chains come from the parser unbounded in length.
    fn resolve_entity_in(
        &self,
        file: FileId,
        node: NodeId,
        active: &mut Vec<SymbolRef>,
    ) -> Option<SymbolRef> {
        let f = self.file(file);
        let mut rights = Vec::new();
        let mut cur = node;
        let mut l = loop {
            match *f.ast.node(cur) {
                Node::Ident { name } => {
                    if let Some(s) = f.bound.resolved(cur) {
                        break SymbolRef { file, symbol: s };
                    }
                    let text = f.ast.atom(name);
                    break self.global(text).and_then(|g| g.symbols.first().copied())?;
                }
                Node::QualifiedName { left, right }
                | Node::Member {
                    obj: left,
                    prop: right,
                    ..
                } => {
                    rights.push(right);
                    cur = left;
                }
                Node::Paren { expr } => cur = expr,
                _ => return None,
            }
        };
        for &right in rights.iter().rev() {
            let resolved = self.resolve_alias_in(l, active)?;
            let rname = f.ast.name_of(right)?;
            let lf = self.file(resolved.file);
            let ls = lf.bound.symbol(resolved.symbol);
            l = if ls.flags.intersects(SymbolFlags::MODULE_FILE) {
                let target = if resolved.symbol == lf.bound.file_symbol {
                    ModuleTarget::File(resolved.file)
                } else {
                    let mname = lf.bound.name_text(lf.ast, ls.name);
                    self.find_ambient_module(mname)
                        .map_or(ModuleTarget::Unresolved, ModuleTarget::Ambient)
                };
                self.resolve_export_in(target, rname, active)?
            } else {
                let t = ls.exports;
                self.lookup_in(resolved, |_| t, rname).filter(|s| {
                    !self
                        .file(s.file)
                        .bound
                        .symbol(s.symbol)
                        .facts
                        .intersects(SymbolFacts::DETACHED)
                })?
            };
        }
        Some(l)
    }
}

/// Span used to report a declaration: its name when it has one.
fn decl_span(ast: &Ast, d: NodeId) -> ts_r_ast::Span {
    ast.span(crate::decl_name_node(ast, d).unwrap_or(d))
}
