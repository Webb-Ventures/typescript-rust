//! Builds a [`ModuleMeta`] from a prepared module: import records, import bindings,
//! exports, and one part per top-level statement with the symbols it declares and uses and
//! whether it has side effects.

use rustc_hash::{FxHashMap, FxHashSet};
use ts_r_ast::{Ast, ImportPhase, MetaPropKind, Modifiers, Node, NodeId, NodeList, VarKind};
use ts_r_minify::fold::binding_idents;
use ts_r_minify::purity::Purity;
use ts_r_minify::{ScopeId, ScopeKind, Scopes, SymbolKind, build};

use crate::meta::*;
use crate::prepare::Prepared;

/// Scans a prepared module. `annotations` honours `/* @__PURE__ */` comments.
pub fn scan(p: &Prepared, annotations: bool, pure_calls: &[String]) -> ModuleMeta {
    let ast = &p.ast;
    let scopes = &p.scopes;
    let mut m = ModuleMeta::default();
    let module_syms: FxHashSet<u32> = scopes.module_symbols().iter().map(|s| s.0).collect();
    // Reference identifiers (not declarations) that resolve to module-scope symbols.
    let refs: FxHashSet<u32> = scopes
        .refs
        .iter()
        .filter(|r| r.symbol.is_some_and(|s| module_syms.contains(&s.0)))
        .map(|r| r.node.raw())
        .collect();
    for &s in scopes.module_symbols() {
        let sym = scopes.symbol(s);
        let kind = match sym.kind {
            SymbolKind::Var | SymbolKind::Param | SymbolKind::CatchParam => SymKind::Var,
            SymbolKind::Let | SymbolKind::Const | SymbolKind::Using => SymKind::Lexical,
            SymbolKind::Function | SymbolKind::FunctionExprName => SymKind::Function,
            SymbolKind::Class | SymbolKind::ClassExprName => SymKind::Class,
            SymbolKind::Import => SymKind::Import,
            SymbolKind::Synthetic => SymKind::Default,
        };
        m.syms.push(SymInfo {
            sym: s.0,
            name: ast.atom(sym.name).to_owned(),
            kind,
        });
    }
    m.default_sym = p.default_sym.map(|s| s.0);
    m.cjs_ref_bytes = scopes
        .globals
        .iter()
        .map(|(&a, &n)| match ast.atom(a) {
            "module" => 5 * n,
            "exports" => 6 * n,
            _ => 0,
        })
        .sum();
    let mut globals: Vec<String> = scopes
        .globals
        .keys()
        .map(|&a| ast.atom(a).to_owned())
        .collect();
    globals.sort();
    m.uses_cjs = globals.iter().any(|g| g == "module" || g == "exports");
    m.globals = globals;
    m.has_direct_eval = scopes.has_direct_eval;

    let stmts = build::program_stmts(ast);
    m.use_strict = stmts
        .iter()
        .take_while(|&&s| matches!(ast.node(s), Node::Directive { .. }))
        .any(|&s| matches!(*ast.node(s), Node::Directive { value } if ast.atom(value) == "use strict"));
    m.needs_function = has_top_level_return(ast, &stmts) || has_top_level_arguments(ast, scopes);
    // Imports first, so namespace bindings are known while walking statements.
    let mut ns_syms: FxHashSet<u32> = FxHashSet::default();
    let mut stmt_records: FxHashMap<u32, u32> = FxHashMap::default();
    for &s in &stmts {
        let rec_kind = match *ast.node(s) {
            Node::ImportDecl {
                type_only: false,
                source,
                phase,
                attributes,
                ..
            } => Some((RecordKind::Import, source, phase_code(phase), attributes)),
            Node::ExportNamed {
                type_only: false,
                source: Some(source),
                attributes,
                ..
            }
            | Node::ExportAll {
                type_only: false,
                source,
                attributes,
                ..
            } => Some((RecordKind::ReExport, source, PHASE_EVALUATION, attributes)),
            _ => None,
        };
        let Some((kind, source, phase, attributes)) = rec_kind else {
            continue;
        };
        let Some(spec) = ast.name_of(source) else {
            continue;
        };
        let span = ast.span(source);
        let idx = m.records.len() as u32;
        m.records.push(Record {
            kind,
            spec: spec.to_owned(),
            start: span.start,
            end: span.end,
            node: s.raw(),
            in_try: false,
            phase,
            attrs: attributes
                .map(|a| attributes_text(ast, a))
                .unwrap_or_default(),
        });
        stmt_records.insert(s.raw(), idx);
        if let Node::ImportDecl { specifiers, .. } = *ast.node(s) {
            for &sp in ast.list(specifiers) {
                let (local, name) = match *ast.node(sp) {
                    Node::ImportDefaultSpecifier { local } => {
                        (local, Imported::Name("default".into()))
                    }
                    Node::ImportNamespaceSpecifier { local } => (local, Imported::Namespace),
                    Node::ImportSpecifier {
                        type_only: false,
                        imported,
                        local,
                    } => {
                        let n = ast
                            .name_of(imported.unwrap_or(local))
                            .unwrap_or_default()
                            .to_owned();
                        (local, Imported::Name(n))
                    }
                    _ => continue,
                };
                let Some(sym) = scopes.symbol_of(local) else {
                    continue;
                };
                if name == Imported::Namespace {
                    ns_syms.insert(sym.0);
                }
                m.imports.push(ImportBinding {
                    sym: sym.0,
                    record: idx,
                    name,
                });
            }
        }
    }

    let purity = Purity {
        ast,
        src: &p.src,
        scopes: Some(scopes),
        annotations,
        pure_calls,
    };
    let mut exports: Vec<(String, ExportTarget)> = Vec::new();
    let mut walker = Walker {
        ast,
        scopes,
        refs: &refs,
        module_syms: &module_syms,
        ns_syms: &ns_syms,
        stack: Vec::new(),
        guarded: FxHashSet::default(),
        chained: FxHashSet::default(),
    };
    for &s in &stmts {
        let node = *ast.node(s);
        let mut part = Part {
            node: s.raw(),
            kind: PartKind::Stmt,
            declares: Vec::new(),
            uses: Vec::new(),
            ns_members: Vec::new(),
            records: Vec::new(),
            side_effects: false,
        };
        match node {
            Node::ImportDecl { type_only, .. } => {
                part.kind = PartKind::Import;
                if !type_only && !p.injected.contains(&s) {
                    m.has_esm_syntax = true;
                }
                if let Some(&r) = stmt_records.get(&s.raw()) {
                    part.records.push(r);
                    for i in &m.imports {
                        if i.record == r {
                            part.declares.push(i.sym);
                        }
                    }
                }
            }
            Node::ExportNamed {
                type_only,
                specifiers,
                source,
                ..
            } => {
                if !type_only {
                    m.has_esm_syntax = true;
                    m.has_esm_exports = true;
                }
                match source {
                    Some(_) => {
                        part.kind = PartKind::ExportFrom;
                        if let Some(&r) = stmt_records.get(&s.raw()) {
                            part.records.push(r);
                            for &sp in ast.list(specifiers) {
                                if let Node::ExportSpecifier {
                                    type_only: false,
                                    local,
                                    exported,
                                } = *ast.node(sp)
                                {
                                    let l = ast.name_of(local).unwrap_or_default().to_owned();
                                    let e = ast
                                        .name_of(exported.unwrap_or(local))
                                        .unwrap_or_default()
                                        .to_owned();
                                    exports.push((
                                        e,
                                        ExportTarget::Reexport {
                                            record: r,
                                            name: Imported::Name(l),
                                        },
                                    ));
                                }
                            }
                        }
                    }
                    None => {
                        part.kind = PartKind::ExportLocal;
                        if !type_only {
                            for &sp in ast.list(specifiers) {
                                if let Node::ExportSpecifier {
                                    type_only: false,
                                    local,
                                    exported,
                                } = *ast.node(sp)
                                    && let Some(sym) = scopes.symbol_of(local)
                                {
                                    let e = ast
                                        .name_of(exported.unwrap_or(local))
                                        .unwrap_or_default()
                                        .to_owned();
                                    exports.push((e, ExportTarget::Local(sym.0)));
                                }
                            }
                        }
                    }
                }
            }
            Node::ExportAll {
                type_only,
                exported,
                ..
            } => {
                part.kind = PartKind::ExportFrom;
                if !type_only {
                    m.has_esm_syntax = true;
                    m.has_esm_exports = true;
                }
                if let Some(&r) = stmt_records.get(&s.raw()) {
                    part.records.push(r);
                    match exported {
                        Some(e) => {
                            let e = ast.name_of(e).unwrap_or_default().to_owned();
                            exports.push((
                                e,
                                ExportTarget::Reexport {
                                    record: r,
                                    name: Imported::Namespace,
                                },
                            ));
                        }
                        None => m.star_exports.push(r),
                    }
                }
            }
            Node::Directive { .. } => {
                part.kind = PartKind::Directive;
            }
            _ => {
                walker.walk(s, &mut part, &mut m);
                part.side_effects = !purity.stmt(s);
                // Destructuring and `using` are treated as effects by `Purity`.
                let mods = node.modifiers();
                if mods.contains(Modifiers::EXPORT) {
                    m.has_esm_syntax = true;
                    m.has_esm_exports = true;
                    collect_decl_exports(ast, scopes, s, p.default_sym.map(|d| d.0), &mut exports);
                }
                if let Node::ExportDefaultExpr { .. } = node {
                    m.has_esm_syntax = true;
                    m.has_esm_exports = true;
                    if let Some(d) = p.default_sym {
                        exports.push(("default".into(), ExportTarget::Local(d.0)));
                        part.declares.push(d.0);
                    }
                }
                if mods.contains(Modifiers::DEFAULT)
                    && let Some(d) = p.default_sym
                    && matches!(
                        node,
                        Node::FunctionDecl { name: None, .. } | Node::ClassDecl { name: None, .. }
                    )
                {
                    part.declares.push(d.0);
                }
                part.declares.sort_unstable();
                part.declares.dedup();
                part.uses.sort_unstable();
                part.uses.dedup();
                part.ns_members.sort();
                part.ns_members.dedup();
            }
        }
        m.parts.push(part);
    }
    exports.sort_by(|a, b| a.0.cmp(&b.0));
    exports.dedup_by(|a, b| a.0 == b.0);
    m.exports = exports;
    m
}

fn collect_decl_exports(
    ast: &Ast,
    scopes: &Scopes,
    s: NodeId,
    default_sym: Option<u32>,
    out: &mut Vec<(String, ExportTarget)>,
) {
    let node = *ast.node(s);
    let mods = node.modifiers();
    match node {
        Node::FunctionDecl { name, .. } | Node::ClassDecl { name, .. } => {
            let sym = match name {
                Some(n) => scopes.symbol_of(n).map(|s| s.0),
                None => default_sym,
            };
            let Some(sym) = sym else { return };
            let export_name = if mods.contains(Modifiers::DEFAULT) {
                "default".to_owned()
            } else {
                name.and_then(|n| ast.name_of(n))
                    .unwrap_or_default()
                    .to_owned()
            };
            out.push((export_name, ExportTarget::Local(sym)));
        }
        Node::VarDecl { decls, .. } => {
            for &d in ast.list(decls) {
                if let Node::VarDeclarator { name, .. } = *ast.node(d) {
                    let mut ids = Vec::new();
                    binding_idents(ast, name, &mut ids);
                    for id in ids {
                        if let Some(sym) = scopes.symbol_of(id) {
                            let n = ast.name_of(id).unwrap_or_default().to_owned();
                            out.push((n, ExportTarget::Local(sym.0)));
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

/// For a call `callee(args)` that handles a promise rejection (`p.catch(f)`, `p.then(f, g)`),
/// the `import()` at the start of `p`'s `.then`/`.catch`/`.finally` chain, if any
/// (`import('x').then(f).catch(g)`). Adds the chain's calls below this one to `chained`:
/// they lead to the same start, so the caller need not walk from them again.
fn handled_dynamic_import(
    ast: &Ast,
    callee: NodeId,
    args: NodeList,
    chained: &mut FxHashSet<u32>,
) -> Option<NodeId> {
    let Node::Member { obj, prop, .. } = *ast.node(callee) else {
        return None;
    };
    let handles = match ast.name_of(prop)? {
        "catch" => !ast.list(args).is_empty(),
        "then" => ast.list(args).len() >= 2,
        _ => false,
    };
    if !handles {
        return None;
    }
    // Every step moves to a child, so this ends.
    let mut cur = obj;
    loop {
        match *ast.node(cur) {
            Node::ImportCall { .. } => return Some(cur),
            Node::Paren { expr } => cur = expr,
            Node::Call { callee, .. } => match *ast.node(callee) {
                Node::Member { obj, prop, .. }
                    if matches!(ast.name_of(prop), Some("then" | "catch" | "finally")) =>
                {
                    chained.insert(cur.raw());
                    cur = obj;
                }
                _ => return None,
            },
            _ => return None,
        }
    }
}

const IN_TRY: u8 = 1;
const IN_FN: u8 = 2;

struct Walker<'a> {
    ast: &'a Ast,
    scopes: &'a Scopes,
    refs: &'a FxHashSet<u32>,
    module_syms: &'a FxHashSet<u32>,
    ns_syms: &'a FxHashSet<u32>,
    stack: Vec<(NodeId, u8)>,
    /// `import()` calls whose rejection a promise chain handles (`.catch(f)` or
    /// `.then(f, g)`): a missing module is a warning, as inside `try`.
    guarded: FxHashSet<u32>,
    /// Calls already passed while walking down a promise chain from an outer call (the
    /// walk visits outer calls first): skipping them keeps long chains linear.
    chained: FxHashSet<u32>,
}

impl Walker<'_> {
    /// Collects declarations, uses, records, TLA and `import.meta` in one statement.
    fn walk(&mut self, root: NodeId, part: &mut Part, m: &mut ModuleMeta) {
        let ast = self.ast;
        self.stack.clear();
        self.stack.push((root, 0));
        while let Some((id, flags)) = self.stack.pop() {
            let node = *ast.node(id);
            let mut child_flags = flags;
            match node {
                Node::Ident { .. } => {
                    if let Some(sym) = self.scopes.symbol_of(id)
                        && self.module_syms.contains(&sym.0)
                    {
                        if self.refs.contains(&id.raw()) {
                            part.uses.push(sym.0);
                        } else if self.scopes.symbol(sym).scope == ScopeId::MODULE {
                            part.declares.push(sym.0);
                        }
                    }
                    continue;
                }
                Node::Member { obj, prop, .. } => {
                    if crate::print::is_import_meta(ast, id) {
                        // A JSX factory of `import.meta`.
                        m.uses_import_meta = true;
                        continue;
                    }
                    if let Node::Ident { .. } = ast.node(obj)
                        && let Some(sym) = self.scopes.symbol_of(obj)
                        && self.ns_syms.contains(&sym.0)
                        && let Node::Ident { name } = *ast.node(prop)
                    {
                        part.ns_members.push((sym.0, ast.atom(name).to_owned()));
                        continue;
                    }
                }
                Node::Call { callee, args, .. } => {
                    if !self.chained.contains(&id.raw())
                        && let Some(import) =
                            handled_dynamic_import(ast, callee, args, &mut self.chained)
                    {
                        self.guarded.insert(import.raw());
                    }
                    if let Node::Ident { name } = *ast.node(callee)
                        && ast.atom(name) == "require"
                        && self.scopes.symbol_of(callee).is_none()
                        && let [arg] = ast.list(args)
                        && let Some(spec) = string_arg(ast, *arg)
                    {
                        let span = ast.span(*arg);
                        part.records.push(m.records.len() as u32);
                        m.records.push(Record {
                            kind: RecordKind::Require,
                            spec,
                            start: span.start,
                            end: span.end,
                            node: id.raw(),
                            in_try: flags & IN_TRY != 0,
                            phase: PHASE_EVALUATION,
                            attrs: String::new(),
                        });
                    }
                }
                Node::ImportCall { arg, phase, .. } => {
                    if let Some(spec) = string_arg(ast, arg) {
                        let span = ast.span(arg);
                        part.records.push(m.records.len() as u32);
                        m.records.push(Record {
                            kind: RecordKind::Dynamic,
                            spec,
                            start: span.start,
                            end: span.end,
                            node: id.raw(),
                            in_try: flags & IN_TRY != 0 || self.guarded.contains(&id.raw()),
                            phase: phase_code(phase),
                            attrs: String::new(),
                        });
                    }
                }
                Node::MetaProperty {
                    kind: MetaPropKind::ImportMeta,
                } => m.uses_import_meta = true,
                Node::Await { .. } if flags & IN_FN == 0 => m.has_tla = true,
                Node::ForOf { is_await: true, .. } if flags & IN_FN == 0 => m.has_tla = true,
                Node::VarDecl {
                    kind: VarKind::AwaitUsing,
                    ..
                } if flags & IN_FN == 0 => m.has_tla = true,
                Node::FunctionDecl { .. }
                | Node::FunctionExpr { .. }
                | Node::Arrow { .. }
                | Node::ClassDecl { .. }
                | Node::ClassExpr { .. } => child_flags |= IN_FN,
                Node::Try {
                    block,
                    handler,
                    finalizer,
                } => {
                    if let Some(f) = finalizer {
                        self.stack.push((f, flags));
                    }
                    if let Some(h) = handler {
                        self.stack.push((h, flags));
                    }
                    self.stack.push((block, flags | IN_TRY));
                    continue;
                }
                _ => {}
            }
            let start = self.stack.len();
            node.for_each_child(ast, &mut |c| self.stack.push((c, child_flags)));
            // Source order when popping.
            self.stack[start..].reverse();
        }
    }
}

fn phase_code(phase: ImportPhase) -> u8 {
    match phase {
        ImportPhase::Evaluation => PHASE_EVALUATION,
        ImportPhase::Defer => PHASE_DEFER,
        ImportPhase::Source => PHASE_SOURCE,
    }
}

/// Import attributes as printed after the specifier, without the leading space:
/// `with { type: "json" }` (or `assert { ... }`), empty when there are none.
fn attributes_text(ast: &Ast, id: NodeId) -> String {
    let Node::ImportAttributes { assert, entries } = *ast.node(id) else {
        return String::new();
    };
    let mut parts = Vec::new();
    for &e in ast.list(entries) {
        let Node::ImportAttribute { key, value } = *ast.node(e) else {
            continue;
        };
        let (Some(k), Some(v)) = (ast.name_of(key), ast.name_of(value)) else {
            continue;
        };
        let k = if matches!(ast.node(key), Node::Ident { .. }) {
            k.to_owned()
        } else {
            crate::expr::Expr::Str(k.to_owned()).to_text(true)
        };
        parts.push(format!(
            "{k}: {}",
            crate::expr::Expr::Str(v.to_owned()).to_text(true)
        ));
    }
    let kw = if assert { "assert" } else { "with" };
    if parts.is_empty() {
        format!("{kw} {{}}")
    } else {
        format!("{kw} {{ {} }}", parts.join(", "))
    }
}

fn string_arg(ast: &Ast, id: NodeId) -> Option<String> {
    match *ast.node(id) {
        Node::StringLit { value } => Some(ast.atom(value).to_owned()),
        Node::TemplateLit { parts } => match ast.list(parts) {
            [only] => match *ast.node(*only) {
                Node::TemplateElement {
                    cooked,
                    invalid_escape: false,
                } => Some(ast.atom(cooked).to_owned()),
                _ => None,
            },
            _ => None,
        },
        _ => None,
    }
}

/// A `return` statement outside every function (allowed at the top of a CommonJS module).
fn has_top_level_return(ast: &Ast, stmts: &[NodeId]) -> bool {
    // Statements and the clauses that hold statements; `return` can't cross a function or
    // class body, and expressions hold statements only inside one.
    let container = |n: &Node| {
        n.is_statement() || matches!(n, Node::SwitchCase { .. } | Node::CatchClause { .. })
    };
    let mut stack: Vec<NodeId> = stmts.to_vec();
    while let Some(id) = stack.pop() {
        match ast.node(id) {
            Node::Return { .. } => return true,
            Node::FunctionDecl { .. }
            | Node::ClassDecl { .. }
            | Node::ExprStmt { .. }
            | Node::VarDecl { .. } => {}
            n if container(n) => ast.for_each_child(id, |c| {
                if container(ast.node(c)) {
                    stack.push(c);
                }
            }),
            _ => {}
        }
    }
    false
}

/// `this` or `new.target` outside every non-arrow function (conservatively: class bodies
/// count as outside), which an arrow wrapper would change.
pub(crate) fn has_top_level_this(ast: &Ast, stmts: &[NodeId]) -> bool {
    let mut stack: Vec<NodeId> = stmts.to_vec();
    while let Some(id) = stack.pop() {
        match ast.node(id) {
            Node::This {}
            | Node::MetaProperty {
                kind: MetaPropKind::NewTarget,
            } => return true,
            Node::FunctionDecl { .. } | Node::FunctionExpr { .. } => {}
            _ => ast.for_each_child(id, |c| stack.push(c)),
        }
    }
    false
}

/// `arguments` outside every function (a CommonJS module wrapper's arguments). A reference
/// that resolves to a module-level `var arguments` counts too: in a function wrapper that
/// declaration is the arguments object, in an arrow a new `undefined` local.
pub(crate) fn has_top_level_arguments(ast: &Ast, scopes: &Scopes) -> bool {
    scopes.refs.iter().any(|r| {
        matches!(*ast.node(r.node), Node::Ident { name } if ast.atom(name) == "arguments") && {
            let mut s = Some(r.scope);
            loop {
                let Some(id) = s else { break true };
                let sc = scopes.scope(id);
                if matches!(
                    sc.kind,
                    ScopeKind::Function | ScopeKind::StaticBlock | ScopeKind::Class
                ) {
                    break false;
                }
                s = sc.parent;
            }
        }
    })
}
