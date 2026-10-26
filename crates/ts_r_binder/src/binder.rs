//! The per-file binding walk. See `docs/design/0003-binder.md`.

use rustc_hash::FxHashMap;
use ts_r_ast::{
    AssignOp, Ast, BinaryOp, FnFlags, HeritageKind, MethodKind, Modifiers, ModuleKind, Node,
    NodeId, NodeKind, NodeList, Span, UnaryOp, VarKind,
};
use ts_r_diagnostics::{Diagnostic, DiagnosticMessage};

use crate::flow::{FlowGraph, FlowId, FlowKind, FlowNode, FunctionFlow, flow_flags};
use crate::messages as m;
use crate::names::{Name, Names, js_number_to_string};
use crate::symbols::{Symbol, SymbolFacts, SymbolFlags, SymbolId, TableId, TableRange, excludes};
use crate::{
    AliasInfo, AliasTarget, BindOptions, BoundFile, ExportStar, ImportName, ModuleRequest, Refs,
    RequestKind, Scope, ScopeId, ScopeKind, meaning,
};

const NONE: u32 = u32::MAX;
/// Hard nesting limit for the walk.
const MAX_DEPTH: u32 = 3000;
/// Native stack the walk may use, measured from where binding started.
const STACK_BUDGET: usize = 1 << 20;

#[inline(always)]
fn stack_addr() -> usize {
    let marker = 0u8;
    std::hint::black_box(&marker) as *const u8 as usize
}

/// A reference waiting for resolution.
#[derive(Clone, Copy)]
pub(crate) struct PendingRef {
    pub node: NodeId,
    pub name: Name,
    pub scope: ScopeId,
    pub meaning: u8,
    pub write: bool,
    /// In the parameter list of the function whose scope is `scope`.
    pub in_params: bool,
    /// In the type parameter list of the function whose scope is `scope`.
    pub in_type_params: bool,
}

#[derive(Clone, Copy)]
struct SymExtra {
    head: u32,
    tail: u32,
    excludes: SymbolFlags,
}

#[derive(Clone, Copy)]
struct LabelEntry {
    name: ts_r_ast::Atom,
    node: NodeId,
    body: NodeId,
    break_label: FlowId,
    continue_label: Option<FlowId>,
    seq: u32,
    used: bool,
}

struct FinallyFrame {
    entry: FlowId,
    seq: u32,
    /// Jumps that cross this finally: (target label, target seq, flow at the jump).
    pending: Vec<(FlowId, u32, FlowId)>,
}

/// Jump state saved and reset at function boundaries.
#[derive(Default)]
struct JumpState {
    breaks: Vec<(FlowId, u32)>,
    continues: Vec<(FlowId, u32)>,
    labels: Vec<LabelEntry>,
    return_label: Option<(FlowId, u32)>,
    exception: Option<FlowId>,
    finally: Vec<FinallyFrame>,
}

/// Lexical context saved and restored around containers.
#[derive(Clone, Copy)]
struct Ctx {
    scope: ScopeId,
    var_scope: ScopeId,
    /// Scope of the module/namespace whose exports receive `export` declarations.
    container_scope: ScopeId,
    export_container: Option<SymbolId>,
    implicit_export: bool,
    ambient: bool,
    class_depth: u32,
    /// Strict mode code (modules, classes, `alwaysStrict`, `"use strict"`).
    strict: bool,
    in_arguments_fn: bool,
    /// Inside a type: references are not recorded in the flow graph.
    in_type: bool,
    /// Collecting `infer` declarations into this scope.
    infer_scope: Option<ScopeId>,
    /// Nesting of namespaces/ambient modules (for TS2435/TS2669).
    module_depth: u32,
    in_ambient_module: bool,
    /// Inside `declare global { }`.
    in_global_augmentation: bool,
    /// Binding the parameter list of the function with this scope.
    params_of: Option<ScopeId>,
    /// Binding the type parameter list of the function with this scope.
    type_params_of: Option<ScopeId>,
}

pub(crate) struct Binder<'a> {
    ast: &'a Ast,
    src: &'a str,
    opts: BindOptions,
    is_module: bool,
    names: Names,
    symbols: Vec<Symbol>,
    sym_extra: Vec<SymExtra>,
    decl_links: Vec<(NodeId, u32)>,
    table_map: FxHashMap<(u32, Name), SymbolId>,
    table_count: u32,
    module_export_tables: Vec<TableId>,
    scopes: Vec<Scope>,
    /// Per scope: created inside its parent function's parameter list.
    scope_in_params: Vec<bool>,
    /// Per scope: created inside its parent function's type parameter list.
    scope_in_type_params: Vec<bool>,
    scope_of_node: Vec<(NodeId, ScopeId)>,
    symbol_of_decl: Vec<(NodeId, SymbolId)>,
    decl_members: Vec<(NodeId, TableId)>,
    iface_tables: FxHashMap<SymbolId, TableId>,
    /// Exports table → locals table of the module/namespace body being bound.
    partners: FxHashMap<u32, TableId>,
    /// The function expression about to be bound as an immediately invoked one.
    iife: Option<NodeId>,
    /// Members of a class expression are being bound (not of a class declaration).
    in_class_expr: bool,
    /// A method's function about to be bound whose flow does not continue the outer flow:
    /// a constructor, or a member of a class declaration.
    detached_function: Option<NodeId>,
    /// Binding the object or callee of a property access, element access or call (the
    /// spine of a possible optional chain).
    in_spine: bool,
    /// The flows where the optional chain being bound short-circuits (its `?.` objects
    /// nullish), joined at the chain's end.
    chain_skips: Option<Vec<FlowId>>,
    /// Inside `!`: literal conditions are not folded.
    no_fold: u32,
    ctx: Ctx,
    refs: Vec<PendingRef>,
    aliases: Vec<AliasInfo>,
    module_requests: Vec<ModuleRequest>,
    export_stars: Vec<ExportStar>,
    ambient_modules: TableId,
    module_augmentations: TableId,
    global_augmentations: TableId,
    global_symbol: Option<SymbolId>,
    umd_name: Option<(Name, NodeId)>,
    diags: Vec<Diagnostic>,
    // Flow.
    flow_nodes: Vec<FlowNode>,
    label_ante: Vec<Vec<FlowId>>,
    flow_lists: Vec<FlowId>,
    node_flow: Vec<(NodeId, FlowId)>,
    functions: Vec<FunctionFlow>,
    fallthroughs: Vec<(NodeId, FlowId)>,
    stmt_flow: Vec<(NodeId, FlowId)>,
    current: FlowId,
    jumps: JumpState,
    seq: u32,
    unreachable: Vec<Span>,
    unreachable_suppress: u32,
    unused_labels: Vec<NodeId>,
    // Limits.
    depth: u32,
    stack_base: usize,
    too_deep: bool,
    default_name: Name,
    /// Expando candidates: `f.x = v` / `f["x"] = v` as (assignment target, object
    /// identifier, property name, function-level scope).
    expando_assigns: Vec<(NodeId, NodeId, Name, ScopeId)>,
    /// A JavaScript CommonJS module (no ECMAScript module syntax, a CommonJS indicator).
    commonjs: bool,
    /// CommonJS export assignments: `exports.x = v` / `module.exports.x = v` (name `x`) and
    /// `module.exports = v` (`export=`), as (assignment, target, root identifier
    /// `exports`/`module`, name).
    commonjs_exports: Vec<(NodeId, NodeId, NodeId, Name)>,
    /// Declarations that can take expando properties (function declarations; `const`
    /// declarations initialized with a function, any variable in JavaScript), with their
    /// function-level scope.
    expando_targets: Vec<(NodeId, ScopeId)>,
    /// JavaScript variables initialized with a class expression (`var C = class {}`), as
    /// (variable name, class expression): their expandos are the class's statics.
    expando_classes: Vec<(NodeId, NodeId)>,
    /// Late-bound expando candidates: `f[k] = v` with `k` a name (`sym`, `K.a`), as
    /// (assignment target, object identifier, function-level scope).
    expando_late: Vec<(NodeId, NodeId, ScopeId)>,
    /// Late-bound expando assignments that apply, by target symbol.
    late_expandos: Vec<(SymbolId, NodeId)>,
}

fn skip_parens(ast: &Ast, mut id: NodeId) -> NodeId {
    while let Node::Paren { expr } = *ast.node(id) {
        id = expr;
    }
    id
}

/// `a`, `a.b.c`: an identifier or a chain of property accesses on one (bounded depth).
fn is_entity_name_expr(ast: &Ast, mut id: NodeId) -> bool {
    for _ in 0..64 {
        match *ast.node(id) {
            Node::Ident { .. } => return true,
            Node::Member { obj, prop, .. } if matches!(ast.node(prop), Node::Ident { .. }) => {
                id = obj;
            }
            _ => return false,
        }
    }
    false
}

const RESERVED_IN_STRICT: &[&str] = &[
    "implements",
    "interface",
    "let",
    "package",
    "private",
    "protected",
    "public",
    "static",
    "yield",
];

impl<'a> Binder<'a> {
    pub(crate) fn new(ast: &'a Ast, src: &'a str, opts: &BindOptions) -> Binder<'a> {
        let is_module = opts.force_module || file_is_module(ast);
        let mut names = Names::default();
        let default_name = names.name_for(ast, "default");
        let mut b = Binder {
            ast,
            src,
            opts: *opts,
            is_module,
            names,
            symbols: Vec::with_capacity(ast.nodes.len() / 16 + 8),
            sym_extra: Vec::with_capacity(ast.nodes.len() / 16 + 8),
            decl_links: Vec::with_capacity(ast.nodes.len() / 16 + 8),
            table_map: FxHashMap::default(),
            table_count: 1,
            module_export_tables: Vec::new(),
            scopes: Vec::new(),
            scope_in_params: Vec::new(),
            scope_in_type_params: Vec::new(),
            scope_of_node: Vec::new(),
            symbol_of_decl: Vec::new(),
            decl_members: Vec::new(),
            iface_tables: FxHashMap::default(),
            partners: FxHashMap::default(),
            iife: None,
            in_class_expr: false,
            detached_function: None,
            in_spine: false,
            chain_skips: None,
            no_fold: 0,
            ctx: Ctx {
                scope: ScopeId(0),
                var_scope: ScopeId(0),
                container_scope: ScopeId(0),
                export_container: None,
                implicit_export: false,
                ambient: ast.source_kind.is_declaration(),
                class_depth: 0,
                strict: true,
                in_arguments_fn: false,
                in_type: false,
                infer_scope: None,
                module_depth: 0,
                in_ambient_module: false,
                in_global_augmentation: false,
                params_of: None,
                type_params_of: None,
            },
            refs: Vec::new(),
            aliases: Vec::new(),
            module_requests: Vec::new(),
            export_stars: Vec::new(),
            ambient_modules: TableId::NONE,
            module_augmentations: TableId::NONE,
            global_augmentations: TableId::NONE,
            global_symbol: None,
            umd_name: None,
            diags: Vec::new(),
            flow_nodes: Vec::with_capacity(ast.nodes.len() / 8 + 4),
            label_ante: Vec::new(),
            flow_lists: Vec::new(),
            node_flow: Vec::with_capacity(ast.nodes.len() / 4 + 4),
            functions: Vec::new(),
            fallthroughs: Vec::new(),
            stmt_flow: Vec::new(),
            current: FlowId::UNREACHABLE,
            jumps: JumpState::default(),
            seq: 1,
            unreachable: Vec::new(),
            unreachable_suppress: 0,
            unused_labels: Vec::new(),
            depth: 0,
            stack_base: stack_addr(),
            too_deep: false,
            default_name,
            expando_assigns: Vec::new(),
            commonjs: is_module && !ast.source_kind.is_typescript() && !has_esm_syntax(ast),
            commonjs_exports: Vec::new(),
            expando_targets: Vec::new(),
            expando_classes: Vec::new(),
            expando_late: Vec::new(),
            late_expandos: Vec::new(),
        };
        b.ambient_modules = b.new_table();
        b.module_augmentations = b.new_table();
        b.global_augmentations = b.new_table();
        b
    }

    pub(crate) fn run(mut self) -> BoundFile {
        let root = self.ast.root;
        // Symbol 0: the file.
        let file_sym = self.new_symbol(
            Name::FILE,
            SymbolFlags::MODULE_FILE | SymbolFlags::VALUE_MODULE,
            root,
            SymbolFacts::NONE,
            SymbolFlags::NONE,
        );
        let exports = self.ensure_exports(file_sym);
        if self.is_module {
            self.module_export_tables.push(exports);
        }
        let file_scope = self.new_scope(ScopeKind::File, root, Some(file_sym));
        self.ctx.scope = file_scope;
        self.ctx.var_scope = file_scope;
        self.ctx.container_scope = file_scope;
        if self.is_module {
            self.ctx.export_container = Some(file_sym);
            let locals = self.locals(file_scope);
            self.partners.insert(exports.0, locals);
        }
        // Flow: [0] unreachable, [1] file start.
        self.flow_nodes.push(FlowNode {
            kind: FlowKind::Unreachable,
            flags: 0,
            node: None,
            a: 0,
            b: 0,
        });
        self.current = self.add_flow(FlowKind::Start, None, 0, 0);
        if let Node::Program { stmts } = *self.ast.node(root) {
            let list = self.ast.list(stmts);
            self.ctx.strict = self.is_module
                || self.opts.always_strict != Some(false)
                || self.has_use_strict(list);
            // A declaration-file module without an export clause or export assignment exports
            // every top-level declaration (observed: `type A = 1; export class B {}` in a.d.ts
            // lets `import { A } from "./a"` resolve).
            self.ctx.implicit_export =
                self.is_module && self.ctx.ambient && !has_explicit_exports(self.ast, list);
            self.bind_stmts(list);
        }
        self.finish(file_sym)
    }

    // ---------------------------------------------------------------------------------------
    // Symbols and tables
    // ---------------------------------------------------------------------------------------

    fn new_table(&mut self) -> TableId {
        let t = TableId(self.table_count);
        self.table_count += 1;
        t
    }

    fn new_symbol(
        &mut self,
        name: Name,
        flags: SymbolFlags,
        decl: NodeId,
        facts: SymbolFacts,
        excl: SymbolFlags,
    ) -> SymbolId {
        let id = SymbolId(self.symbols.len() as u32);
        let link = self.decl_links.len() as u32;
        self.decl_links.push((decl, NONE));
        self.symbols.push(Symbol {
            name,
            flags,
            facts,
            decl_start: 0,
            decl_len: 1,
            value_decl: flags.intersects(SymbolFlags::VALUE).then_some(decl),
            parent: None,
            members: TableId::NONE,
            exports: TableId::NONE,
        });
        self.sym_extra.push(SymExtra {
            head: link,
            tail: link,
            excludes: excl,
        });
        self.symbol_of_decl.push((decl, id));
        id
    }

    fn add_decl(&mut self, sym: SymbolId, decl: NodeId) {
        let link = self.decl_links.len() as u32;
        self.decl_links.push((decl, NONE));
        let tail = self.sym_extra[sym.index()].tail;
        self.decl_links[tail as usize].1 = link;
        self.sym_extra[sym.index()].tail = link;
        self.symbols[sym.index()].decl_len += 1;
        self.symbol_of_decl.push((decl, sym));
    }

    fn ensure_exports(&mut self, sym: SymbolId) -> TableId {
        if self.symbols[sym.index()].exports.is_none() {
            let t = self.new_table();
            self.symbols[sym.index()].exports = t;
        }
        self.symbols[sym.index()].exports
    }

    /// A member table of its own for one class or interface declaration: members of different
    /// declarations of a merged interface (or class + interface) do not conflict at bind time.
    /// The symbol's `members` is the first declaration's table.
    fn decl_members_table(&mut self, sym: SymbolId, decl: NodeId) -> TableId {
        let t = self.new_table();
        if self.symbols[sym.index()].members.is_none() {
            self.symbols[sym.index()].members = t;
        }
        self.decl_members.push((decl, t));
        t
    }

    fn ensure_members(&mut self, sym: SymbolId) -> TableId {
        if self.symbols[sym.index()].members.is_none() {
            let t = self.new_table();
            self.symbols[sym.index()].members = t;
        }
        self.symbols[sym.index()].members
    }

    /// Declares `decl` as `name` in `table`, merging or reporting a conflict.
    #[allow(clippy::too_many_arguments)]
    fn declare(
        &mut self,
        table: TableId,
        name: Name,
        decl: NodeId,
        flags: SymbolFlags,
        excl: SymbolFlags,
        facts: SymbolFacts,
        parent: Option<SymbolId>,
    ) -> SymbolId {
        // Missing names (left by parser recovery) are declared like any other, as in tsc,
        // so two of them in one scope are duplicates: `function f( { return 'a'; }`.
        if table.is_none() || name == Name::COMPUTED {
            let s = self.new_symbol(name, flags, decl, facts, excl);
            self.symbols[s.index()].parent = parent;
            return s;
        }
        if let Some(&existing) = self.table_map.get(&(table.0, name)) {
            let ex = self.symbols[existing.index()];
            let ex_excl = self.sym_extra[existing.index()].excludes;
            // `export default <expr>` only conflicts with what was bound before it; a later
            // declaration decides by its own excludes.
            let one_way = ex
                .flags
                .intersects(SymbolFlags::ALIAS | SymbolFlags::EXPORT_EXPRESSION)
                && ex.facts.intersects(SymbolFacts::DEFAULT_EXPORT);
            if ex.flags.intersects(excl) || (!one_way && ex_excl.intersects(flags)) {
                self.report_conflict(table, existing, decl, flags);
                let s = self.new_symbol(name, flags, decl, facts | SymbolFacts::DETACHED, excl);
                self.symbols[s.index()].parent = parent;
                return s;
            }
            let sym = &mut self.symbols[existing.index()];
            sym.flags |= flags;
            sym.facts |= facts;
            if sym.value_decl.is_none() && flags.intersects(SymbolFlags::VALUE) {
                sym.value_decl = Some(decl);
            }
            self.sym_extra[existing.index()].excludes |= excl;
            self.add_decl(existing, decl);
            self.check_partner(table, name, decl, flags, excl, facts);
            return existing;
        }
        let s = self.new_symbol(name, flags, decl, facts, excl);
        self.symbols[s.index()].parent = parent;
        self.table_map.insert((table.0, name), s);
        self.check_partner(table, name, decl, flags, excl, facts);
        s
    }

    /// Exported and local declarations of one module or namespace body live in different
    /// tables. An exported declaration still conflicts with a local of the same body that was
    /// bound before it (by its excludes); a local bound after an export never conflicts (the
    /// checker reports TS2395 instead).
    fn check_partner(
        &mut self,
        table: TableId,
        name: Name,
        decl: NodeId,
        flags: SymbolFlags,
        excl: SymbolFlags,
        facts: SymbolFacts,
    ) {
        if !facts.intersects(SymbolFacts::EXPORTED) {
            return;
        }
        let Some(&other) = self.partners.get(&table.0) else {
            return;
        };
        if name == self.default_name
            || name.is_reserved()
            || flags.intersects(SymbolFlags::ALIAS)
            || facts.intersects(SymbolFacts::EXPORT_SPECIFIER)
            || self.ast.node(decl).modifiers().contains(Modifiers::DEFAULT)
        {
            return;
        }
        let Some(o) = self.lookup_table(other, name) else {
            return;
        };
        let os = self.symbols[o.index()];
        if os.flags.intersects(SymbolFlags::ALIAS)
            || os.facts.intersects(SymbolFacts::EXPORT_SPECIFIER)
        {
            return;
        }
        if os.flags.intersects(excl) {
            self.report_conflict(other, o, decl, flags);
        }
    }

    fn report_conflict(
        &mut self,
        table: TableId,
        existing: SymbolId,
        decl: NodeId,
        flags: SymbolFlags,
    ) {
        let ex = self.symbols[existing.index()];
        let enum_conflict = (flags.intersects(SymbolFlags::ENUM)
            || ex.flags.intersects(SymbolFlags::ENUM))
            && !flags.intersects(SymbolFlags::ENUM_MEMBER);
        let mut decls = Vec::new();
        let mut l = self.sym_extra[existing.index()].head;
        while l != NONE {
            decls.push(self.decl_links[l as usize].0);
            l = self.decl_links[l as usize].1;
        }
        // A local that merges with a default-exported declaration is reported by the checker
        // (TS2652), not here.
        if !self.module_export_tables.contains(&table)
            && decls
                .iter()
                .chain(std::iter::once(&decl))
                .any(|&d| self.ast.node(d).modifiers().contains(Modifiers::DEFAULT))
        {
            return;
        }
        let first_text = self.decl_name_text(decls[0], ex.name);
        let jsdoc_alias = ex.name == self.default_name
            && decls
                .iter()
                .chain(std::iter::once(&decl))
                .any(|&d| self.ast.is_jsdoc(d));
        let report = |b: &mut Self, d: NodeId| {
            let span = b.decl_error_span(d);
            let diag = if enum_conflict {
                Diagnostic::new(m::ENUM_MERGE_CONFLICT, span, &[])
            } else if ex.name == b.default_name
                && b.module_export_tables.contains(&table)
                && !jsdoc_alias
            {
                Diagnostic::new(m::MULTIPLE_DEFAULT_EXPORTS, span, &[])
            } else if jsdoc_alias {
                // A JSDoc `@typedef default` against a default export: each declaration
                // under its own name (tsc 7).
                Diagnostic::new(
                    m::DUPLICATE_IDENTIFIER,
                    span,
                    &[&b.decl_name_text(d, ex.name)],
                )
            } else {
                let text = if ex.flags.intersects(SymbolFlags::ENUM_MEMBER) {
                    b.decl_name_text(d, ex.name)
                } else {
                    first_text.clone()
                };
                let msg = if ex.flags.intersects(SymbolFlags::BLOCK_SCOPED_VARIABLE) {
                    m::CANNOT_REDECLARE_BLOCK_SCOPED
                } else {
                    m::DUPLICATE_IDENTIFIER
                };
                Diagnostic::new(msg, span, &[&text])
            };
            b.diags.push(diag);
        };
        for &d in &decls {
            report(self, d);
        }
        report(self, decl);
    }

    /// Text used in duplicate-identifier messages: the source text of the declaration name.
    fn decl_name_text(&self, decl: NodeId, name: Name) -> String {
        if name.is_reserved() {
            return self.names.text(self.ast, name).to_owned();
        }
        match crate::decl_name_node(self.ast, decl) {
            Some(n) => {
                let span = self.ast.span(n);
                self.src
                    .get(span.start as usize..span.end as usize)
                    .unwrap_or("")
                    .to_owned()
            }
            None => self.names.text(self.ast, name).to_owned(),
        }
    }

    fn decl_error_span(&self, decl: NodeId) -> Span {
        if let Node::ExportDefaultExpr { expr } = *self.ast.node(decl)
            && matches!(self.ast.node(expr), Node::Missing {})
        {
            // `export default` without an expression: at the end of `default`.
            let sp = self.ast.span(decl);
            let text = self
                .src
                .get(sp.start as usize..sp.end as usize)
                .unwrap_or("");
            if let Some(i) = text.find("default") {
                return Span::at(sp.start + i as u32 + 7);
            }
        }
        match crate::decl_name_node(self.ast, decl) {
            Some(n) => self.ast.span(n),
            None => self.ast.span(decl),
        }
    }

    // ---------------------------------------------------------------------------------------
    // Scopes and context
    // ---------------------------------------------------------------------------------------

    fn new_scope(&mut self, kind: ScopeKind, node: NodeId, symbol: Option<SymbolId>) -> ScopeId {
        let id = ScopeId(self.scopes.len() as u32);
        let parent = if self.scopes.is_empty() {
            None
        } else {
            Some(self.ctx.scope)
        };
        let locals = self.new_table();
        self.scopes.push(Scope {
            kind,
            node,
            parent,
            locals,
            symbol,
        });
        self.scope_in_params
            .push(parent.is_some() && self.ctx.params_of == parent);
        self.scope_in_type_params
            .push(parent.is_some() && self.ctx.type_params_of == parent);
        self.scope_of_node.push((node, id));
        id
    }

    #[inline]
    fn locals(&self, scope: ScopeId) -> TableId {
        self.scopes[scope.0 as usize].locals
    }

    /// Where a declaration with `modifiers` goes: the container's exports or a locals table.
    fn decl_table(
        &mut self,
        modifiers: Modifiers,
        block_scoped: bool,
    ) -> (TableId, Option<SymbolId>, SymbolFacts) {
        if let Some(c) = self.ctx.export_container
            && self.ctx.scope == self.ctx.container_scope
            && (modifiers.contains(Modifiers::EXPORT) || self.ctx.implicit_export)
        {
            let t = self.ensure_exports(c);
            return (t, Some(c), SymbolFacts::EXPORTED);
        }
        let scope = if block_scoped {
            self.ctx.scope
        } else {
            self.ctx.var_scope
        };
        (self.locals(scope), None, SymbolFacts::NONE)
    }

    fn ambient_facts(&self, modifiers: Modifiers) -> SymbolFacts {
        if self.ctx.ambient || modifiers.contains(Modifiers::DECLARE) {
            SymbolFacts::AMBIENT
        } else {
            SymbolFacts::NONE
        }
    }

    // ---------------------------------------------------------------------------------------
    // Diagnostics helpers
    // ---------------------------------------------------------------------------------------

    fn error(&mut self, msg: DiagnosticMessage, span: Span, args: &[&str]) {
        self.diags.push(Diagnostic::new(msg, span, args));
    }

    fn ident_text(&self, id: NodeId) -> Option<&'a str> {
        match *self.ast.node(id) {
            Node::Ident { name } => Some(self.ast.atom(name)),
            _ => None,
        }
    }

    /// Strict-mode reserved words used as identifiers (TS1212/1213/1214).
    /// Reserved-word check for a declaration name (skipped for `declare` declarations).
    fn check_decl_name(&mut self, name: NodeId, modifiers: Modifiers) {
        if !modifiers.contains(Modifiers::DECLARE) {
            self.check_reserved(name);
        }
    }

    /// A directive prologue containing exactly `"use strict"` / `'use strict'`.
    fn has_use_strict(&self, stmts: &[NodeId]) -> bool {
        for &s in stmts {
            let Node::Directive { .. } = self.ast.node(s) else {
                return false;
            };
            let sp = self.ast.span(s);
            let text = self
                .src
                .get(sp.start as usize..sp.end as usize)
                .unwrap_or("");
            let text = text.trim_end_matches(';').trim_end();
            if text == "\"use strict\"" || text == "'use strict'" {
                return true;
            }
        }
        false
    }

    fn check_reserved(&mut self, ident: NodeId) {
        if !self.ctx.strict || self.ctx.ambient {
            return;
        }
        let Some(text) = self.ident_text(ident) else {
            return;
        };
        if !RESERVED_IN_STRICT.contains(&text) {
            return;
        }
        // Escaped identifiers (`let`) are reported by the parser, not here.
        let span = self.ast.span(ident);
        if self.src.get(span.start as usize..span.end as usize) != Some(text) {
            return;
        }
        let msg = if self.ctx.class_depth > 0 {
            m::RESERVED_WORD_IN_CLASS
        } else if self.is_module {
            m::RESERVED_WORD_IN_MODULE
        } else {
            m::RESERVED_WORD_IN_STRICT_MODE
        };
        self.error(msg, span, &[text]);
    }

    /// `eval`/`arguments` declared or assigned (TS1100/1210/1215).
    fn check_eval_arguments(&mut self, ident: NodeId) {
        if !self.ctx.strict || self.ctx.ambient {
            return;
        }
        let Some(text) = self.ident_text(ident) else {
            return;
        };
        if text != "eval" && text != "arguments" {
            return;
        }
        let msg = if self.ctx.class_depth > 0 {
            m::INVALID_USE_IN_CLASS
        } else if self.is_module {
            m::INVALID_USE_IN_MODULE
        } else {
            m::INVALID_USE_IN_STRICT_MODE
        };
        let span = self.ast.span(ident);
        self.error(msg, span, &[text]);
    }

    // ---------------------------------------------------------------------------------------
    // Depth guard
    // ---------------------------------------------------------------------------------------

    /// Enters one level of recursion. Returns false (after reporting once) when too deep.
    #[inline]
    fn enter(&mut self, id: NodeId) -> bool {
        self.depth += 1;
        if self.too_deep {
            return false;
        }
        if self.depth > MAX_DEPTH || self.stack_base.abs_diff(stack_addr()) > STACK_BUDGET {
            self.too_deep = true;
            let span = self.ast.span(id);
            self.error(m::MAXIMUM_NESTING_DEPTH, span, &[]);
            return false;
        }
        true
    }

    // ---------------------------------------------------------------------------------------
    // References
    // ---------------------------------------------------------------------------------------

    fn add_ref(&mut self, ident: NodeId, meaning: u8, write: bool) {
        let Node::Ident { name } = *self.ast.node(ident) else {
            return;
        };
        self.check_reserved(ident);
        if meaning & meaning::VALUE != 0
            && self.ctx.in_arguments_fn
            && self.ast.atom(name) == "arguments"
        {
            return;
        }
        self.refs.push(PendingRef {
            node: ident,
            name: Name::from_atom(name),
            scope: self.ctx.scope,
            meaning,
            write,
            in_params: self.ctx.params_of == Some(self.ctx.scope),
            in_type_params: self.ctx.type_params_of == Some(self.ctx.scope),
        });
    }

    /// An entity name (`A`, `A.B`, `A.B.C`, as `QualifiedName` or `Member` chain): references
    /// its leftmost identifier.
    fn add_entity_ref(&mut self, node: NodeId, last_meaning: u8) {
        let mut cur = node;
        let mut meaning = last_meaning;
        loop {
            match *self.ast.node(cur) {
                Node::Ident { .. } => {
                    self.add_ref(cur, meaning, false);
                    if !self.ctx.in_type {
                        self.record_flow(cur);
                    }
                    return;
                }
                Node::QualifiedName { left, .. } => {
                    cur = left;
                    meaning = meaning::NAMESPACE | (last_meaning & meaning::VALUE);
                }
                Node::Member { obj, .. } => {
                    cur = obj;
                    meaning = meaning::NAMESPACE | meaning::VALUE;
                }
                Node::Paren { expr } => cur = expr,
                _ => {
                    self.bind_expr(cur);
                    return;
                }
            }
        }
    }

    // ---------------------------------------------------------------------------------------
    // Flow helpers
    // ---------------------------------------------------------------------------------------

    fn add_flow(&mut self, kind: FlowKind, node: Option<NodeId>, a: u32, b: u32) -> FlowId {
        let id = FlowId(self.flow_nodes.len() as u32);
        self.flow_nodes.push(FlowNode {
            kind,
            flags: 0,
            node,
            a,
            b,
        });
        id
    }

    fn new_label(&mut self, kind: FlowKind) -> FlowId {
        let slot = self.label_ante.len() as u32;
        self.label_ante.push(Vec::new());
        self.add_flow(kind, None, slot, 0)
    }

    fn add_ante(&mut self, label: FlowId, ante: FlowId) {
        if ante.is_unreachable() {
            return;
        }
        let slot = self.flow_nodes[label.index()].a as usize;
        let list = &mut self.label_ante[slot];
        if !list.contains(&ante) {
            list.push(ante);
        }
    }

    fn label_count(&self, label: FlowId) -> usize {
        self.label_ante[self.flow_nodes[label.index()].a as usize].len()
    }

    /// Finishes a branch label: unreachable if nothing reaches it, its single antecedent if
    /// only one does.
    fn finish_label(&mut self, label: FlowId) -> FlowId {
        let slot = self.flow_nodes[label.index()].a as usize;
        match self.label_ante[slot].len() {
            0 => FlowId::UNREACHABLE,
            1 if self.flow_nodes[label.index()].flags & flow_flags::FINALLY == 0 => {
                self.label_ante[slot][0]
            }
            _ => label,
        }
    }

    fn condition(&mut self, kind: FlowKind, ante: FlowId, expr: NodeId, flags: u8) -> FlowId {
        if ante.is_unreachable() {
            return ante;
        }
        let e = skip_parens(self.ast, expr);
        if self.no_fold == 0
            && let Node::BoolLit { value } = *self.ast.node(e)
        {
            let dead = (kind == FlowKind::TrueCondition && !value)
                || (kind == FlowKind::FalseCondition && value);
            return if dead { FlowId::UNREACHABLE } else { ante };
        }
        let id = self.add_flow(kind, Some(expr), ante.0, 0);
        self.flow_nodes[id.index()].flags = flags;
        id
    }

    fn assignment_flow(&mut self, node: NodeId) {
        if self.current.is_unreachable() {
            return;
        }
        let f = self.add_flow(FlowKind::Assignment, Some(node), self.current.0, 0);
        self.current = f;
        if let Some(t) = self.jumps.exception {
            self.add_ante(t, f);
        }
    }

    fn record_flow(&mut self, node: NodeId) {
        self.node_flow.push((node, self.current));
    }

    fn next_seq(&mut self) -> u32 {
        self.seq += 1;
        self.seq
    }

    /// Jumps from the current flow to `target` (whose scope started at `target_seq`), going
    /// through any `finally` blocks in between.
    fn jump(&mut self, target: FlowId, target_seq: u32) {
        let flow = self.current;
        self.jump_flow(target, target_seq, flow);
        self.current = FlowId::UNREACHABLE;
    }

    fn jump_flow(&mut self, target: FlowId, target_seq: u32, flow: FlowId) {
        if flow.is_unreachable() {
            return;
        }
        if let Some(frame) = self.jumps.finally.last_mut()
            && frame.seq > target_seq
        {
            frame.pending.push((target, target_seq, flow));
            let entry = frame.entry;
            self.add_ante(entry, flow);
            return;
        }
        self.add_ante(target, flow);
    }

    // ---------------------------------------------------------------------------------------
    // Statements
    // ---------------------------------------------------------------------------------------

    fn bind_stmts(&mut self, stmts: &[NodeId]) {
        // Function declarations bind before the other statements of their list.
        for &s in stmts {
            if let Node::FunctionDecl { .. } = self.ast.node(s) {
                self.declare_function(s);
            }
        }
        let mut run: Option<Span> = None;
        for &s in stmts {
            let dead = self.current.is_unreachable() && self.unreachable_suppress == 0;
            let reportable = dead && self.is_reportable(s);
            if reportable {
                let sp = self.ast.span(s);
                run = Some(match run {
                    Some(r) => r.cover(sp),
                    None => sp,
                });
                self.unreachable_suppress += 1;
                self.bind_stmt(s);
                self.unreachable_suppress -= 1;
            } else {
                if let Some(r) = run.take() {
                    self.unreachable.push(r);
                }
                // The checker may still find it unreachable (after a call returning
                // `never`, an exhaustive switch).
                if !dead
                    && !self.current.is_unreachable()
                    && self.opts.allow_unreachable_code == Some(false)
                    && self.is_reportable(s)
                {
                    self.stmt_flow.push((s, self.current));
                }
                self.bind_stmt(s);
            }
        }
        if let Some(r) = run {
            self.unreachable.push(r);
        }
    }

    /// Statements reported as unreachable code (TS7027) when the flow cannot reach them.
    fn is_reportable(&self, s: NodeId) -> bool {
        let n = self.ast.node(s);
        if n.modifiers().contains(Modifiers::DECLARE) {
            return false;
        }
        match *n {
            Node::FunctionDecl { .. }
            | Node::Interface { .. }
            | Node::TypeAlias { .. }
            | Node::Block { .. }
            | Node::Empty {}
            | Node::Missing {}
            | Node::ImportDecl { .. }
            | Node::ImportEquals { .. }
            | Node::ExportNamed { .. }
            | Node::ExportAll { .. }
            | Node::NamespaceExport { .. }
            | Node::Directive { .. } => false,
            Node::VarDecl { kind, decls, .. } => {
                kind != VarKind::Var
                    || self.ast.list(decls).iter().any(|&d| {
                        matches!(self.ast.node(d), Node::VarDeclarator { init: Some(_), .. })
                    })
            }
            Node::Enum { modifiers, .. } => {
                !modifiers.contains(Modifiers::CONST) || self.opts.preserve_const_enums
            }
            Node::Module { body, .. } => body.is_some_and(|b| {
                let st = self.module_state(b);
                st == 2 || (st == 1 && self.opts.preserve_const_enums)
            }),
            _ => true,
        }
    }

    fn bind_stmt(&mut self, id: NodeId) {
        if self.enter(id) {
            self.bind_stmt_inner(id);
        }
        self.depth -= 1;
    }

    fn bind_stmt_inner(&mut self, id: NodeId) {
        let ast = self.ast;
        match *ast.node(id) {
            Node::Block { stmts } => {
                let saved = self.ctx;
                self.ctx.scope = self.new_scope(ScopeKind::Block, id, None);
                self.bind_stmts(ast.list(stmts));
                self.ctx = saved;
            }
            Node::ExprStmt { expr } => {
                self.bind_expr(expr);
                let e = skip_parens(ast, expr);
                if let Node::Call { callee, .. } = *ast.node(e)
                    && !self.current.is_unreachable()
                {
                    // `xs.push(..)` got its ArrayMutation node in `bind_expr`.
                    if !self.is_array_mutation_call(callee) {
                        let f = self.add_flow(FlowKind::Call, Some(e), self.current.0, 0);
                        self.current = f;
                        if let Some(t) = self.jumps.exception {
                            self.add_ante(t, f);
                        }
                    }
                }
            }
            Node::If { test, cons, alt } => {
                let then_l = self.new_label(FlowKind::BranchLabel);
                let else_l = self.new_label(FlowKind::BranchLabel);
                let post = self.new_label(FlowKind::BranchLabel);
                self.bind_condition(test, then_l, else_l);
                self.current = self.finish_label(then_l);
                self.bind_stmt(cons);
                self.add_ante(post, self.current);
                self.current = self.finish_label(else_l);
                if let Some(alt) = alt {
                    self.bind_stmt(alt);
                }
                self.add_ante(post, self.current);
                self.current = self.finish_label(post);
            }
            Node::While { test, body } => {
                let pre = self.loop_head();
                let body_l = self.new_label(FlowKind::BranchLabel);
                let post = self.new_label(FlowKind::BranchLabel);
                let seq = self.next_seq();
                self.bind_condition(test, body_l, post);
                self.current = self.finish_label(body_l);
                self.bind_loop_body(id, body, post, pre, seq);
                self.add_ante(pre, self.current);
                self.current = self.finish_label(post);
            }
            Node::DoWhile { body, test } => {
                let pre = self.loop_head();
                let cont = self.new_label(FlowKind::BranchLabel);
                let post = self.new_label(FlowKind::BranchLabel);
                let seq = self.next_seq();
                self.bind_loop_body(id, body, post, cont, seq);
                self.add_ante(cont, self.current);
                self.current = self.finish_label(cont);
                self.bind_condition(test, pre, post);
                self.current = self.finish_label(post);
            }
            Node::For {
                init,
                test,
                update,
                body,
            } => {
                let saved = self.ctx;
                if let Some(init) = init
                    && let Node::VarDecl { kind, .. } = *ast.node(init)
                    && kind != VarKind::Var
                {
                    self.ctx.scope = self.new_scope(ScopeKind::For, id, None);
                }
                if let Some(init) = init {
                    self.bind_for_init(init);
                }
                let pre = self.loop_head();
                let body_l = self.new_label(FlowKind::BranchLabel);
                let cont = self.new_label(FlowKind::BranchLabel);
                let post = self.new_label(FlowKind::BranchLabel);
                let seq = self.next_seq();
                match test {
                    Some(t) => self.bind_condition(t, body_l, post),
                    None => self.add_ante(body_l, self.current),
                }
                self.current = self.finish_label(body_l);
                self.bind_loop_body(id, body, post, cont, seq);
                self.add_ante(cont, self.current);
                self.current = self.finish_label(cont);
                if let Some(u) = update {
                    self.bind_expr(u);
                }
                self.add_ante(pre, self.current);
                self.current = self.finish_label(post);
                self.ctx = saved;
            }
            Node::ForIn { left, right, body }
            | Node::ForOf {
                left, right, body, ..
            } => {
                let saved = self.ctx;
                if let Node::VarDecl { kind, .. } = *ast.node(left)
                    && kind != VarKind::Var
                {
                    self.ctx.scope = self.new_scope(ScopeKind::For, id, None);
                }
                self.bind_expr(right);
                let pre = self.loop_head();
                let post = self.new_label(FlowKind::BranchLabel);
                let seq = self.next_seq();
                self.add_ante(post, self.current);
                if let Node::VarDecl { .. } = ast.node(left) {
                    self.bind_var_decl(left, true);
                } else {
                    self.bind_assign_target(left, false);
                }
                self.bind_loop_body(id, body, post, pre, seq);
                self.add_ante(pre, self.current);
                self.current = self.finish_label(post);
                self.ctx = saved;
            }
            Node::Return { arg } => {
                if let Some(a) = arg {
                    self.bind_expr(a);
                }
                self.record_flow(id);
                if let Some((label, seq)) = self.jumps.return_label {
                    self.jump(label, seq);
                } else {
                    self.current = FlowId::UNREACHABLE;
                }
            }
            Node::Throw { arg } => {
                self.bind_expr(arg);
                self.record_flow(id);
                if let Some(t) = self.jumps.exception {
                    self.add_ante(t, self.current);
                }
                self.current = FlowId::UNREACHABLE;
            }
            Node::Break { label } => self.bind_break_continue(label, true),
            Node::Continue { label } => self.bind_break_continue(label, false),
            Node::Labeled { label, body } => self.bind_labeled(label, body),
            Node::Switch { disc, cases } => self.bind_switch(id, disc, cases),
            Node::Try {
                block,
                handler,
                finalizer,
            } => self.bind_try(block, handler, finalizer),
            Node::With { obj, body } => {
                let sp = ast.span(id);
                if self.ctx.strict {
                    self.error(
                        m::WITH_NOT_ALLOWED_IN_STRICT_MODE,
                        Span::new(sp.start, sp.start + 4),
                        &[],
                    );
                }
                self.bind_expr(obj);
                self.bind_stmt(body);
            }
            Node::VarDecl { .. } => self.bind_var_decl(id, false),
            Node::FunctionDecl { .. } => self.bind_function_like(id),
            Node::ClassDecl { .. } => self.bind_class(id),
            Node::Interface { .. } => self.bind_interface(id),
            Node::TypeAlias {
                modifiers,
                name,
                type_params,
                ty,
            } => {
                self.check_decl_name(name, modifiers);
                // A JSDoc `@typedef`/`@callback` at the top level of a JavaScript ES module is
                // exported (tsc 7). CommonJS modules keep them local.
                let exported = if self.ast.is_jsdoc(id) && self.is_module && !self.commonjs {
                    modifiers | Modifiers::EXPORT
                } else {
                    modifiers
                };
                let (table, parent, facts) = self.decl_table(exported, true);
                let n = self.ident_name(name);
                let facts = facts | self.ambient_facts(modifiers);
                self.declare(
                    table,
                    n,
                    id,
                    SymbolFlags::TYPE_ALIAS,
                    excludes::TYPE_ALIAS,
                    facts,
                    parent,
                );
                let saved = self.ctx;
                self.bind_type_params(type_params, id);
                self.bind_type(ty);
                self.ctx = saved;
            }
            Node::Enum { .. } => self.bind_enum(id),
            Node::Module { .. } => self.bind_module(id),
            Node::ImportDecl { .. } => self.bind_import(id),
            Node::ImportEquals {
                modifiers,
                type_only,
                name,
                module_ref,
            } => {
                self.check_reserved(name);
                // `import x = ...` is never implicitly exported from an ambient body (observed:
                // TS2459 "declares 'x' locally").
                let implicit = std::mem::replace(&mut self.ctx.implicit_export, false);
                let (table, parent, facts) = self.decl_table(modifiers, true);
                self.ctx.implicit_export = implicit;
                let n = self.ident_name(name);
                let mut facts = facts;
                if type_only {
                    facts |= SymbolFacts::TYPE_ONLY;
                }
                let sym = self.declare(
                    table,
                    n,
                    id,
                    SymbolFlags::ALIAS,
                    excludes::ALIAS,
                    facts,
                    parent,
                );
                let target = match *ast.node(module_ref) {
                    Node::ExternalModuleRef { expr } => {
                        let req = self.add_request(expr, RequestKind::ImportEquals, type_only);
                        match req {
                            Some(r) => AliasTarget::Module {
                                request: r,
                                import: ImportName::ExportEquals,
                            },
                            None => AliasTarget::Entity(module_ref),
                        }
                    }
                    _ => {
                        self.add_entity_ref(module_ref, meaning::NAMESPACE);
                        AliasTarget::Entity(module_ref)
                    }
                };
                self.aliases.push(AliasInfo {
                    symbol: sym,
                    decl: id,
                    target,
                });
            }
            Node::ExportNamed { .. } => self.bind_export_named(id),
            Node::ExportAll {
                type_only,
                exported,
                source,
                attributes,
            } => {
                let req = self.add_request(source, RequestKind::ExportFrom, type_only);
                if type_only {
                    self.set_resolution_mode(req, attributes);
                }
                let Some(c) = self.ctx.export_container else {
                    return;
                };
                match (exported, req) {
                    (Some(e), Some(r)) => {
                        let table = self.ensure_exports(c);
                        let n = self.key_name(e);
                        let mut facts = SymbolFacts::EXPORTED | SymbolFacts::EXPORT_SPECIFIER;
                        if type_only {
                            facts |= SymbolFacts::TYPE_ONLY;
                        }
                        let sym = self.declare(
                            table,
                            n,
                            id,
                            SymbolFlags::ALIAS,
                            excludes::ALIAS,
                            facts,
                            Some(c),
                        );
                        self.aliases.push(AliasInfo {
                            symbol: sym,
                            decl: id,
                            target: AliasTarget::Module {
                                request: r,
                                import: ImportName::Namespace,
                            },
                        });
                    }
                    (None, Some(r)) => self.export_stars.push(ExportStar {
                        request: r,
                        node: id,
                        container: c,
                    }),
                    _ => {}
                }
            }
            Node::ExportDefaultExpr { expr } | Node::ExportAssignment { expr } => {
                let is_default = matches!(ast.node(id), Node::ExportDefaultExpr { .. });
                if is_default && !ast.jsdoc_tags.is_empty() {
                    self.bind_jsdoc_side_types(id);
                }
                let e = skip_parens(ast, expr);
                let entity = self.is_entity_name(e);
                if matches!(ast.node(e), Node::Ident { .. }) {
                    self.add_ref(e, meaning::ALL, false);
                    self.record_flow(e);
                } else {
                    self.bind_expr(expr);
                }
                let Some(c) = self.ctx.export_container else {
                    return;
                };
                let table = self.ensure_exports(c);
                let name = if is_default {
                    self.default_name
                } else {
                    Name::EXPORT_EQUALS
                };
                let mut facts = SymbolFacts::EXPORTED;
                if is_default {
                    facts |= SymbolFacts::DEFAULT_EXPORT;
                }
                let flags = if entity {
                    SymbolFlags::ALIAS
                } else {
                    SymbolFlags::EXPORT_EXPRESSION
                };
                let sym = self.declare(
                    table,
                    name,
                    id,
                    flags,
                    excludes::EXPORT_EXPRESSION,
                    facts,
                    Some(c),
                );
                if entity {
                    self.aliases.push(AliasInfo {
                        symbol: sym,
                        decl: id,
                        target: AliasTarget::Entity(e),
                    });
                }
            }
            Node::NamespaceExport { name } => {
                // `export as namespace N` declares a UMD global only at the top of a module
                // declaration file; modifiers on it are an error of their own.
                let span = self.ast.span(id);
                if !self.ast.modifier_tokens_of(id).is_empty() {
                    self.error(m::MODIFIERS_CANNOT_APPEAR_HERE, span, &[]);
                }
                if self.ctx.module_depth != 0 || self.ctx.scope != ScopeId(0) {
                    self.error(m::GLOBAL_EXPORTS_AT_TOP_LEVEL, span, &[]);
                } else if !self.is_module {
                    self.error(m::GLOBAL_EXPORTS_IN_MODULE_FILES, span, &[]);
                } else if !self.ast.source_kind.is_declaration() {
                    self.error(m::GLOBAL_EXPORTS_IN_DECLARATION_FILES, span, &[]);
                } else {
                    let n = self.ident_name(name);
                    self.umd_name = Some((n, id));
                }
            }
            Node::Directive { .. } | Node::Empty {} | Node::Debugger {} | Node::Missing {} => {}
            _ => self.bind_any(id),
        }
    }

    /// Iterative: member chains come from the parser unbounded in length.
    fn is_entity_name(&self, mut e: NodeId) -> bool {
        loop {
            match *self.ast.node(e) {
                Node::Ident { .. } | Node::QualifiedName { .. } => return true,
                Node::Member {
                    obj,
                    prop,
                    optional,
                } if !optional && matches!(self.ast.node(prop), Node::Ident { .. }) => {
                    e = obj;
                }
                _ => return false,
            }
        }
    }

    fn is_array_mutation_call(&self, callee: NodeId) -> bool {
        let c = skip_parens(self.ast, callee);
        if let Node::Member { obj, prop, .. } = *self.ast.node(c)
            && let Some(p) = self.ident_text(prop)
            && (p == "push" || p == "unshift")
        {
            // The reference the object stands for: `(xs)`, `(a, xs)`, `(xs = v)`.
            let mut o = obj;
            loop {
                match *self.ast.node(o) {
                    Node::Paren { expr } => o = expr,
                    Node::Seq { exprs } => match self.ast.list(exprs).last() {
                        Some(&last) => o = last,
                        None => return false,
                    },
                    Node::Assign {
                        left,
                        op: AssignOp::Assign,
                        ..
                    } => o = left,
                    Node::Ident { .. } => return true,
                    _ => return false,
                }
            }
        }
        false
    }

    /// Creates a loop head label for the current flow and makes it current.
    fn loop_head(&mut self) -> FlowId {
        let pre = self.new_label(FlowKind::LoopLabel);
        let reachable = !self.current.is_unreachable();
        self.add_ante(pre, self.current);
        self.current = if reachable { pre } else { FlowId::UNREACHABLE };
        pre
    }

    fn bind_loop_body(
        &mut self,
        loop_node: NodeId,
        body: NodeId,
        brk: FlowId,
        cont: FlowId,
        seq: u32,
    ) {
        for l in self.jumps.labels.iter_mut().rev() {
            if l.body == loop_node {
                l.continue_label = Some(cont);
                l.seq = l.seq.min(seq);
            }
        }
        self.jumps.breaks.push((brk, seq));
        self.jumps.continues.push((cont, seq));
        self.bind_stmt(body);
        self.jumps.breaks.pop();
        self.jumps.continues.pop();
    }

    fn bind_for_init(&mut self, init: NodeId) {
        if let Node::VarDecl { .. } = self.ast.node(init) {
            self.bind_var_decl(init, false);
        } else {
            self.bind_expr(init);
        }
    }

    fn bind_break_continue(&mut self, label: Option<NodeId>, is_break: bool) {
        let target = match label {
            Some(l) => {
                let name = match *self.ast.node(l) {
                    Node::Ident { name } => name,
                    _ => return,
                };
                let found = self.jumps.labels.iter_mut().rev().find(|e| e.name == name);
                match found {
                    Some(e) => {
                        e.used = true;
                        if is_break {
                            Some((e.break_label, e.seq))
                        } else {
                            e.continue_label.map(|c| (c, e.seq))
                        }
                    }
                    None => None,
                }
            }
            None => {
                if is_break {
                    self.jumps.breaks.last().copied()
                } else {
                    self.jumps.continues.last().copied()
                }
            }
        };
        match target {
            Some((t, seq)) => self.jump(t, seq),
            None => self.current = FlowId::UNREACHABLE,
        }
    }

    fn bind_labeled(&mut self, label: NodeId, body: NodeId) {
        let Node::Ident { name } = *self.ast.node(label) else {
            self.bind_stmt(body);
            return;
        };
        if self.jumps.labels.iter().any(|l| l.name == name) {
            let text = self.ast.atom(name);
            let span = self.ast.span(label);
            self.error(m::DUPLICATE_LABEL, span, &[text]);
        }
        if matches!(
            self.ast.node(body),
            Node::FunctionDecl { .. }
                | Node::VarDecl { .. }
                | Node::ClassDecl { .. }
                | Node::Interface { .. }
                | Node::TypeAlias { .. }
                | Node::Enum { .. }
                | Node::Module { .. }
                | Node::ImportDecl { .. }
                | Node::ImportEquals { .. }
                | Node::ExportNamed { .. }
                | Node::ExportAll { .. }
                | Node::ExportDefaultExpr { .. }
                | Node::ExportAssignment { .. }
        ) {
            let span = self.ast.span(label);
            self.error(m::LABEL_NOT_ALLOWED_HERE, span, &[]);
        }
        let post = self.new_label(FlowKind::BranchLabel);
        let seq = self.next_seq();
        self.jumps.labels.push(LabelEntry {
            name,
            node: label,
            body,
            break_label: post,
            continue_label: None,
            seq,
            used: false,
        });
        self.bind_stmt(body);
        let entry = self.jumps.labels.pop();
        if let Some(e) = entry
            && !e.used
        {
            self.unused_labels.push(e.node);
        }
        self.add_ante(post, self.current);
        self.current = self.finish_label(post);
    }

    fn bind_switch(&mut self, id: NodeId, disc: NodeId, cases: NodeList) {
        self.bind_expr(disc);
        let saved = self.ctx;
        self.ctx.scope = self.new_scope(ScopeKind::Switch, id, None);
        let post = self.new_label(FlowKind::BranchLabel);
        let seq = self.next_seq();
        self.jumps.breaks.push((post, seq));
        let pre = self.current;
        let clauses = self.ast.list(cases);
        // Hoist function declarations of every clause (they share the case block scope).
        for &c in clauses {
            if let Node::SwitchCase { body, .. } = *self.ast.node(c) {
                for &s in self.ast.list(body) {
                    if let Node::FunctionDecl { .. } = self.ast.node(s) {
                        self.declare_function(s);
                    }
                }
            }
        }
        let mut fallthrough = FlowId::UNREACHABLE;
        let mut has_default = false;
        let n = clauses.len().min(u16::MAX as usize) as u32;
        for (i, &c) in clauses.iter().enumerate() {
            let Node::SwitchCase { test, body } = *self.ast.node(c) else {
                continue;
            };
            self.current = pre;
            match test {
                Some(t) => self.bind_expr(t),
                None => has_default = true,
            }
            let i = (i as u32).min(n);
            let clause_flow = if pre.is_unreachable() {
                FlowId::UNREACHABLE
            } else {
                self.add_flow(
                    FlowKind::SwitchClause,
                    Some(id),
                    pre.0,
                    i | ((i + 1).min(0xFFFF) << 16),
                )
            };
            let entry = self.new_label(FlowKind::BranchLabel);
            self.add_ante(entry, clause_flow);
            self.add_ante(entry, fallthrough);
            self.current = self.finish_label(entry);
            self.bind_case_stmts(self.ast.list(body));
            fallthrough = self.current;
            // A clause that may run into the next one (empty clauses group with it).
            if (i as usize) + 1 < clauses.len()
                && !self.ast.list(body).is_empty()
                && !self.current.is_unreachable()
            {
                self.fallthroughs.push((c, self.current));
            }
        }
        if !has_default && !pre.is_unreachable() {
            let f = self.add_flow(FlowKind::SwitchClause, Some(id), pre.0, n | (n << 16));
            self.add_ante(post, f);
        }
        self.add_ante(post, fallthrough);
        self.jumps.breaks.pop();
        self.current = self.finish_label(post);
        self.ctx = saved;
    }

    /// Statements of one case clause (functions were hoisted for the whole case block).
    fn bind_case_stmts(&mut self, stmts: &[NodeId]) {
        let mut run: Option<Span> = None;
        for &s in stmts {
            let dead = self.current.is_unreachable() && self.unreachable_suppress == 0;
            if dead && self.is_reportable(s) {
                let sp = self.ast.span(s);
                run = Some(run.map_or(sp, |r| r.cover(sp)));
                self.unreachable_suppress += 1;
                self.bind_stmt(s);
                self.unreachable_suppress -= 1;
            } else {
                if let Some(r) = run.take() {
                    self.unreachable.push(r);
                }
                // The checker may still find it unreachable (after a call returning
                // `never`, an exhaustive switch).
                if !dead
                    && !self.current.is_unreachable()
                    && self.opts.allow_unreachable_code == Some(false)
                    && self.is_reportable(s)
                {
                    self.stmt_flow.push((s, self.current));
                }
                self.bind_stmt(s);
            }
        }
        if let Some(r) = run {
            self.unreachable.push(r);
        }
    }

    fn bind_try(&mut self, block: NodeId, handler: Option<NodeId>, finalizer: Option<NodeId>) {
        let saved_exception = self.jumps.exception;
        let exc = self.new_label(FlowKind::BranchLabel);
        self.add_ante(exc, self.current);
        self.jumps.exception = Some(exc);
        if finalizer.is_some() {
            let entry = self.new_label(FlowKind::BranchLabel);
            self.flow_nodes[entry.index()].flags |= flow_flags::FINALLY;
            let seq = self.next_seq();
            self.jumps.finally.push(FinallyFrame {
                entry,
                seq,
                pending: Vec::new(),
            });
        }
        self.bind_stmt(block);
        self.add_ante(exc, self.current);
        let mut normal = vec![self.current];
        // Exceptions thrown in the catch clause go to the finally block (or outwards).
        let catch_exc = if finalizer.is_some() {
            let l = self.new_label(FlowKind::BranchLabel);
            Some(l)
        } else {
            saved_exception
        };
        let mut uncaught = FlowId::UNREACHABLE;
        if let Some(h) = handler {
            self.jumps.exception = catch_exc;
            self.current = self.finish_label(exc);
            if let Some(l) = catch_exc {
                self.add_ante(l, self.current);
            }
            self.bind_catch(h);
            if let Some(l) = catch_exc
                && finalizer.is_some()
            {
                self.add_ante(l, self.current);
                uncaught = self.finish_label(l);
            }
            normal.push(self.current);
        } else {
            uncaught = self.finish_label(exc);
        }
        self.jumps.exception = saved_exception;
        if let Some(fin) = finalizer {
            let frame = match self.jumps.finally.pop() {
                Some(f) => f,
                None => return,
            };
            let entry = frame.entry;
            for &f in &normal {
                self.add_ante(entry, f);
            }
            self.add_ante(entry, uncaught);
            self.current = if self.label_count(entry) == 0 {
                FlowId::UNREACHABLE
            } else {
                entry
            };
            self.bind_stmt(fin);
            let end = self.current;
            // Normal completion continues with only the normal antecedents of the entry.
            let normal: Vec<FlowId> = normal.into_iter().filter(|f| !f.is_unreachable()).collect();
            self.current = if end.is_unreachable() || normal.is_empty() {
                FlowId::UNREACHABLE
            } else {
                self.reduce(entry, &normal, end)
            };
            if !end.is_unreachable() {
                for (target, target_seq, flow) in frame.pending {
                    let r = self.reduce(entry, &[flow], end);
                    self.jump_flow(target, target_seq, r);
                }
                if !uncaught.is_unreachable()
                    && let Some(outer) = self.jumps.exception
                {
                    let r = self.reduce(entry, &[uncaught], end);
                    self.add_ante(outer, r);
                }
            }
        } else {
            let post = self.new_label(FlowKind::BranchLabel);
            for f in normal {
                self.add_ante(post, f);
            }
            self.current = self.finish_label(post);
        }
    }

    fn reduce(&mut self, target: FlowId, allowed: &[FlowId], ante: FlowId) -> FlowId {
        let b = self.flow_lists.len() as u32;
        self.flow_lists.push(target);
        self.flow_lists.push(FlowId(allowed.len() as u32));
        self.flow_lists.extend_from_slice(allowed);
        self.add_flow(FlowKind::ReduceLabel, None, ante.0, b)
    }

    fn bind_catch(&mut self, h: NodeId) {
        let Node::CatchClause {
            param,
            type_ann,
            body,
        } = *self.ast.node(h)
        else {
            return;
        };
        let saved = self.ctx;
        self.ctx.scope = self.new_scope(ScopeKind::Catch, h, None);
        if let Some(p) = param {
            let table = self.locals(self.ctx.scope);
            self.bind_binding(
                p,
                // Catch bindings are block-scoped in the catch clause (a `var` of the same
                // name hoists past them).
                BindingKind::Var {
                    table,
                    flags: SymbolFlags::BLOCK_SCOPED_VARIABLE,
                    excl: excludes::BLOCK_SCOPED_VARIABLE,
                    facts: SymbolFacts::NONE,
                    parent: None,
                },
                true,
            );
        }
        if let Some(t) = type_ann {
            self.bind_type(t);
        }
        self.bind_stmt(body);
        self.ctx = saved;
    }

    // ---------------------------------------------------------------------------------------
    // Variable declarations and binding patterns
    // ---------------------------------------------------------------------------------------

    fn bind_var_decl(&mut self, id: NodeId, for_head: bool) {
        let Node::VarDecl {
            modifiers,
            kind,
            decls,
        } = *self.ast.node(id)
        else {
            return;
        };
        let block = kind != VarKind::Var;
        let (table, parent, mut facts) = self.decl_table(modifiers, block);
        facts |= self.ambient_facts(modifiers);
        let (flags, excl) = if block {
            facts |= SymbolFacts::BLOCK_SCOPED;
            (
                SymbolFlags::BLOCK_SCOPED_VARIABLE,
                excludes::BLOCK_SCOPED_VARIABLE,
            )
        } else {
            (
                SymbolFlags::FUNCTION_SCOPED_VARIABLE,
                excludes::FUNCTION_SCOPED_VARIABLE,
            )
        };
        let bk = BindingKind::Var {
            table,
            flags,
            excl,
            facts,
            parent,
        };
        let saved_ambient = self.ctx.ambient;
        if modifiers.contains(Modifiers::DECLARE) {
            self.ctx.ambient = true;
        }
        for &d in self.ast.list(decls) {
            let Node::VarDeclarator {
                name,
                type_ann,
                init,
                ..
            } = *self.ast.node(d)
            else {
                continue;
            };
            if let Some(t) = type_ann {
                self.bind_type(t);
            }
            if let Some(i) = init {
                self.bind_expr(i);
            }
            self.bind_binding(name, bk, init.is_some() || for_head);
            // An annotated variable gets expando properties too: they type its function
            // initializer (`const f: Fn = () => 1; f.only = ...`), not the variable.
            if let Some(i) = init
                && !self.ctx.ambient
                && (kind == VarKind::Const || !self.ast.source_kind.is_typescript())
                && matches!(self.ast.node(name), Node::Ident { .. })
                && matches!(
                    self.ast.node(skip_parens(self.ast, i)),
                    Node::FunctionExpr { .. } | Node::Arrow { .. }
                )
            {
                self.expando_targets.push((name, self.ctx.var_scope));
            }
            // `var C = class {}` in JavaScript: `C.x = v` declares a static of the class.
            if let Some(i) = init
                && !self.ctx.ambient
                && !self.ast.source_kind.is_typescript()
                && matches!(self.ast.node(name), Node::Ident { .. })
                && let i = skip_parens(self.ast, i)
                && matches!(self.ast.node(i), Node::ClassExpr { .. })
            {
                self.expando_targets.push((name, self.ctx.var_scope));
                self.expando_classes.push((name, i));
            }
            // A JavaScript variable initialized to `{}` is a container: `o.x = v` in the
            // same function declares `x` (tsc 7).
            if let Some(i) = init
                && !self.ctx.ambient
                && !self.ast.source_kind.is_typescript()
                && matches!(self.ast.node(name), Node::Ident { .. })
                && matches!(
                    self.ast.node(skip_parens(self.ast, i)),
                    Node::ObjectLit { props } if self.ast.list(*props).is_empty()
                )
            {
                self.expando_targets.push((name, self.ctx.var_scope));
            }
        }
        self.ctx.ambient = saved_ambient;
    }

    /// A default value (binding element, parameter, destructuring assignment): evaluated only
    /// when the value is `undefined`, so the flow after it joins the flow before it.
    fn bind_default(&mut self, init: NodeId) {
        let pre = self.current;
        self.bind_expr(init);
        let post = self.new_label(FlowKind::BranchLabel);
        self.add_ante(post, pre);
        self.add_ante(post, self.current);
        self.current = self.finish_label(post);
    }

    /// Declares the names of a binding pattern. `assigned` creates assignment flow nodes.
    fn bind_binding(&mut self, pat: NodeId, kind: BindingKind, assigned: bool) {
        if !self.enter(pat) {
            self.depth -= 1;
            return;
        }
        match *self.ast.node(pat) {
            Node::Ident { name } => {
                self.check_eval_arguments(pat);
                self.check_reserved(pat);
                let n = Name::from_atom(name);
                match kind {
                    BindingKind::Var {
                        table,
                        flags,
                        excl,
                        facts,
                        parent,
                    } => {
                        self.declare(table, n, pat, flags, excl, facts, parent);
                    }
                }
                if assigned {
                    self.assignment_flow(pat);
                }
            }
            Node::ObjectPattern { props } => {
                for &p in self.ast.list(props) {
                    match *self.ast.node(p) {
                        Node::BindingProperty { key, value, init } => {
                            if let Node::ComputedKey { expr } = *self.ast.node(key) {
                                self.bind_expr(expr);
                            }
                            if let Some(i) = init {
                                self.bind_default(i);
                            }
                            match value {
                                Some(v) => self.bind_binding(v, kind, assigned),
                                None => self.bind_binding(key, kind, assigned),
                            }
                        }
                        Node::Rest { arg } => self.bind_binding(arg, kind, assigned),
                        _ => self.bind_any(p),
                    }
                }
            }
            Node::ArrayPattern { elements } => {
                for &e in self.ast.list(elements) {
                    self.bind_binding(e, kind, assigned);
                }
            }
            Node::AssignPattern { left, right } => {
                self.bind_default(right);
                self.bind_binding(left, kind, assigned);
            }
            Node::Rest { arg } => self.bind_binding(arg, kind, assigned),
            Node::Missing {} => {
                let BindingKind::Var {
                    table,
                    flags,
                    excl,
                    facts,
                    parent,
                } = kind;
                self.declare(table, Name::MISSING, pat, flags, excl, facts, parent);
            }
            Node::Hole {} => {}
            _ => self.bind_any(pat),
        }
        self.depth -= 1;
    }

    /// The target of an assignment, `for-in/of` head or destructuring assignment.
    /// `a.b = v` and `a[k] = v` (also compound): the target's object and key are
    /// evaluated before the value (`(x = "", o).p = x.length` reads `x` as `""`), the
    /// assignment happens after it. `false` (nothing bound) for other targets.
    fn bind_member_assignment(&mut self, left: NodeId, right: NodeId) -> bool {
        let ast = self.ast;
        let target = skip_parens(ast, left);
        // `/** @type {T} */ x.y = v`: `T` is recorded on the target.
        if !ast.jsdoc_tags.is_empty()
            && matches!(ast.node(target), Node::Member { .. } | Node::Index { .. })
        {
            self.bind_jsdoc_side_types(left);
        }
        match *ast.node(target) {
            Node::Member { obj, .. } => {
                self.bind_expr(obj);
                self.record_flow(target);
                self.bind_expr(right);
                self.assignment_flow(target);
                true
            }
            Node::Index { obj, index, .. } => {
                self.bind_expr(obj);
                self.bind_expr(index);
                self.record_flow(target);
                self.bind_expr(right);
                if matches!(ast.node(skip_parens(ast, obj)), Node::Ident { .. })
                    && !self.current.is_unreachable()
                {
                    let f = self.add_flow(FlowKind::ArrayMutation, Some(target), self.current.0, 0);
                    self.current = f;
                }
                self.assignment_flow(target);
                true
            }
            _ => false,
        }
    }

    /// A destructuring assignment target. Its defaults (`[x = d] = v`, `{ x = d } = v`) are
    /// bound in order, without the branch a binding pattern's defaults get: tsc's flow for
    /// an assignment pattern evaluates each default, then its target
    /// (controlFlowAssignmentPatternOrder; tsc: binder.go, the destructuring assignment
    /// flow).
    fn bind_assign_target(&mut self, target: NodeId, simple: bool) {
        if !self.enter(target) {
            self.depth -= 1;
            return;
        }
        let ast = self.ast;
        // `/** @type {T} */ x.y = v`: `T` is recorded on the target.
        if !ast.jsdoc_tags.is_empty()
            && matches!(
                ast.node(target),
                Node::Member { .. } | Node::Index { .. } | Node::Ident { .. }
            )
        {
            self.bind_jsdoc_side_types(target);
        }
        match *ast.node(target) {
            Node::Ident { .. } => {
                if simple {
                    self.check_eval_arguments(target);
                }
                self.add_ref(target, meaning::VALUE, true);
                self.record_flow(target);
                self.assignment_flow(target);
            }
            Node::Paren { expr } => self.bind_assign_target(expr, simple),
            Node::Member { obj, .. } => {
                self.bind_expr(obj);
                self.record_flow(target);
                self.assignment_flow(target);
            }
            Node::Index { obj, index, .. } => {
                self.bind_expr(obj);
                self.bind_expr(index);
                self.record_flow(target);
                if matches!(ast.node(skip_parens(ast, obj)), Node::Ident { .. })
                    && !self.current.is_unreachable()
                {
                    let f = self.add_flow(FlowKind::ArrayMutation, Some(target), self.current.0, 0);
                    self.current = f;
                }
                self.assignment_flow(target);
            }
            Node::ArrayLit { elements } => {
                for &e in ast.list(elements) {
                    match *ast.node(e) {
                        Node::Spread { arg } => self.bind_assign_target(arg, false),
                        Node::Assign {
                            left,
                            op: AssignOp::Assign,
                            right,
                        } => {
                            self.bind_expr(right);
                            self.bind_assign_target(left, false);
                        }
                        Node::Hole {} => {}
                        _ => self.bind_assign_target(e, false),
                    }
                }
            }
            Node::ObjectLit { props } => {
                for &p in ast.list(props) {
                    match *ast.node(p) {
                        Node::Property { key, value, .. } => {
                            if let Node::ComputedKey { expr } = *ast.node(key) {
                                self.bind_expr(expr);
                            }
                            match *ast.node(value) {
                                Node::Assign {
                                    left,
                                    op: AssignOp::Assign,
                                    right,
                                } => {
                                    self.bind_expr(right);
                                    self.bind_assign_target(left, false);
                                }
                                _ => self.bind_assign_target(value, false),
                            }
                        }
                        Node::ShorthandProp { name, init, .. } => {
                            if let Some(i) = init {
                                self.bind_expr(i);
                            }
                            self.bind_assign_target(name, false);
                        }
                        Node::Spread { arg } => self.bind_assign_target(arg, false),
                        _ => self.bind_any(p),
                    }
                }
            }
            Node::NonNull { expr } => self.bind_assign_target(expr, simple),
            Node::As { expr, ty }
            | Node::Satisfies { expr, ty }
            | Node::TypeAssertion { ty, expr } => {
                self.bind_type(ty);
                self.bind_assign_target(expr, simple);
            }
            _ => self.bind_expr(target),
        }
        self.depth -= 1;
    }

    // ---------------------------------------------------------------------------------------
    // Expressions
    // ---------------------------------------------------------------------------------------

    fn bind_expr(&mut self, id: NodeId) {
        if self.enter(id) {
            if !self.ast.jsdoc_tags.is_empty()
                && matches!(
                    self.ast.node(id),
                    Node::Member { .. } | Node::Index { .. } | Node::Ident { .. }
                )
            {
                self.bind_jsdoc_side_types(id);
            }
            self.bind_expr_inner(id);
        }
        self.depth -= 1;
    }

    /// JSDoc `@type` of a function, an assignment target, an object literal property or an
    /// `export default` (JavaScript): its type is not a field of the tree, so it is bound
    /// here, in the scope of its host.
    fn bind_jsdoc_side_types(&mut self, host: NodeId) {
        let ast = self.ast;
        for t in ast.jsdoc_tags_of(host) {
            if t.kind == ts_r_ast::JsDocTagKind::Type
                && let Some(ty) = t.ty
            {
                self.bind_type(ty);
            }
        }
    }

    /// Whether `id` is a property access, element access or call whose spine (objects and
    /// callees, not through parentheses) has a `?.`.
    fn in_optional_chain(&self, id: NodeId) -> bool {
        let ast = self.ast;
        let mut n = id;
        for _ in 0..4096 {
            match *ast.node(n) {
                Node::Member { obj, optional, .. } | Node::Index { obj, optional, .. } => {
                    if optional {
                        return true;
                    }
                    n = obj;
                }
                Node::Call {
                    callee, optional, ..
                } => {
                    if optional {
                        return true;
                    }
                    n = callee;
                }
                Node::NonNull { expr } => n = expr,
                _ => return false,
            }
        }
        false
    }

    /// An optional chain (`a?.b.c(x)`): each `?.` continues with its object not nullish
    /// (a nullish-flagged `TrueCondition` on the object), and the chain's end joins those
    /// flows with the ones where it short-circuited.
    fn bind_optional_chain(&mut self, id: NodeId, spine: bool) {
        let ast = self.ast;
        let outer = if spine {
            None
        } else {
            Some(self.chain_skips.replace(Vec::new()))
        };
        let (object, optional) = match *ast.node(id) {
            Node::Member { obj, optional, .. } | Node::Index { obj, optional, .. } => {
                (obj, optional)
            }
            Node::Call {
                callee, optional, ..
            } => (callee, optional),
            Node::NonNull { expr } => (expr, false),
            _ => return,
        };
        self.in_spine = true;
        self.bind_expr(object);
        self.in_spine = false;
        if optional && !self.current.is_unreachable() {
            let pre = self.current;
            let skip = self.condition(FlowKind::FalseCondition, pre, object, flow_flags::NULLISH);
            if let Some(list) = self.chain_skips.as_mut() {
                list.push(skip);
            }
            self.current =
                self.condition(FlowKind::TrueCondition, pre, object, flow_flags::NULLISH);
        }
        match *ast.node(id) {
            Node::Member { .. } => self.record_flow(id),
            Node::Index { index, .. } => {
                self.bind_expr(index);
                self.record_flow(id);
            }
            Node::Call {
                type_args, args, ..
            } => {
                if let Some(t) = type_args {
                    self.bind_type(t);
                }
                for &a in ast.list(args) {
                    self.bind_expr(a);
                }
            }
            _ => {}
        }
        if let Some(outer) = outer {
            let skips = std::mem::replace(&mut self.chain_skips, outer).unwrap_or_default();
            if !skips.is_empty() {
                let post = self.new_label(FlowKind::BranchLabel);
                self.add_ante(post, self.current);
                for f in skips {
                    self.add_ante(post, f);
                }
                self.current = self.finish_label(post);
            }
        }
    }

    fn bind_expr_inner(&mut self, id: NodeId) {
        let ast = self.ast;
        let spine = std::mem::replace(&mut self.in_spine, false);
        if matches!(
            ast.node(id),
            Node::Member { .. } | Node::Index { .. } | Node::Call { .. } | Node::NonNull { .. }
        ) && self.in_optional_chain(id)
        {
            self.bind_optional_chain(id, spine);
            return;
        }
        match *ast.node(id) {
            Node::Ident { .. } => {
                self.add_ref(id, meaning::VALUE, false);
                self.record_flow(id);
            }
            Node::This {} | Node::Super {} => self.record_flow(id),
            Node::Member { obj, .. } => {
                self.bind_expr(obj);
                self.record_flow(id);
            }
            Node::Index { obj, index, .. } => {
                self.bind_expr(obj);
                self.bind_expr(index);
                self.record_flow(id);
            }
            Node::Assign { left, op, right } => match op {
                AssignOp::Assign => {
                    if !self.bind_member_assignment(left, right) {
                        self.bind_expr(right);
                        self.bind_assign_target(left, true);
                    }
                    self.record_expando_assign(left);
                    if self.commonjs {
                        self.record_commonjs_export(id, left);
                    }
                }
                AssignOp::AndAssign | AssignOp::OrAssign | AssignOp::CoalesceAssign => {
                    self.bind_expr(left);
                    let (t, f) = (FlowKind::TrueCondition, FlowKind::FalseCondition);
                    let flags = if op == AssignOp::CoalesceAssign {
                        flow_flags::NULLISH
                    } else {
                        0
                    };
                    let pre = self.current;
                    let (go, skip) = if op == AssignOp::AndAssign {
                        (t, f)
                    } else {
                        (f, t)
                    };
                    let skip_flow = self.condition(skip, pre, left, flags);
                    self.current = self.condition(go, pre, left, flags);
                    self.bind_expr(right);
                    self.bind_assign_target(left, true);
                    let post = self.new_label(FlowKind::BranchLabel);
                    self.add_ante(post, self.current);
                    self.add_ante(post, skip_flow);
                    self.current = self.finish_label(post);
                }
                _ => {
                    if !self.bind_member_assignment(left, right) {
                        self.bind_expr(right);
                        self.bind_assign_target(left, true);
                    }
                }
            },
            Node::Binary { left, op, right } => match op {
                BinaryOp::LogicalAnd | BinaryOp::LogicalOr | BinaryOp::Coalesce => {
                    self.bind_expr(left);
                    let flags = if op == BinaryOp::Coalesce {
                        flow_flags::NULLISH
                    } else {
                        0
                    };
                    let pre = self.current;
                    let (go, skip) = if op == BinaryOp::LogicalAnd {
                        (FlowKind::TrueCondition, FlowKind::FalseCondition)
                    } else {
                        (FlowKind::FalseCondition, FlowKind::TrueCondition)
                    };
                    let skip_flow = self.condition(skip, pre, left, flags);
                    self.current = self.condition(go, pre, left, flags);
                    self.bind_expr(right);
                    let post = self.new_label(FlowKind::BranchLabel);
                    self.add_ante(post, self.current);
                    self.add_ante(post, skip_flow);
                    self.current = self.finish_label(post);
                }
                _ => {
                    self.bind_expr(left);
                    self.bind_expr(right);
                }
            },
            Node::Conditional { test, cons, alt } => {
                let t = self.new_label(FlowKind::BranchLabel);
                let f = self.new_label(FlowKind::BranchLabel);
                let post = self.new_label(FlowKind::BranchLabel);
                self.bind_condition(test, t, f);
                self.current = self.finish_label(t);
                self.bind_expr(cons);
                self.add_ante(post, self.current);
                self.current = self.finish_label(f);
                self.bind_expr(alt);
                self.add_ante(post, self.current);
                self.current = self.finish_label(post);
            }
            Node::Unary { op, arg } => {
                if op == UnaryOp::Delete
                    && self.ctx.strict
                    && matches!(ast.node(arg), Node::Ident { .. } | Node::Missing {})
                {
                    // (A missing operand is an identifier to tsc.)
                    let span = ast.span(arg);
                    self.error(m::DELETE_IDENTIFIER_IN_STRICT_MODE, span, &[]);
                }
                self.bind_expr(arg);
            }
            Node::Update { arg, .. } => {
                let a = skip_parens(ast, arg);
                if matches!(ast.node(a), Node::Ident { .. }) {
                    self.check_eval_arguments(a);
                    self.add_ref(a, meaning::VALUE, true);
                    self.record_flow(a);
                    self.assignment_flow(a);
                } else {
                    self.bind_expr(arg);
                    self.assignment_flow(a);
                }
            }
            Node::Call {
                callee,
                type_args,
                args,
                ..
            } => {
                let f = skip_parens(ast, callee);
                let iife = match *ast.node(f) {
                    Node::FunctionExpr { flags, .. } | Node::Arrow { flags, .. } => {
                        !flags.intersects(FnFlags::ASYNC | FnFlags::GENERATOR)
                    }
                    _ => false,
                };
                if iife {
                    // An immediately invoked function expression runs inline: its body
                    // continues the current flow and the call completes where it does.
                    if let Some(t) = type_args {
                        self.bind_type(t);
                    }
                    for &a in ast.list(args) {
                        self.bind_expr(a);
                    }
                    self.iife = Some(f);
                    self.bind_expr(callee);
                    self.iife = None;
                } else {
                    self.bind_expr(callee);
                    if let Some(t) = type_args {
                        self.bind_type(t);
                    }
                    for &a in ast.list(args) {
                        self.bind_expr(a);
                    }
                    // JavaScript `require("m")` imports a module, like `import x =
                    // require("m")`.
                    if !ast.source_kind.is_typescript()
                        && let [a] = ast.list(args)
                        && matches!(ast.node(*a), Node::StringLit { .. })
                        && matches!(*ast.node(f), Node::Ident { name } if ast.atom(name) == "require")
                    {
                        self.add_request(*a, RequestKind::ImportEquals, false);
                    }
                    // Any `xs.push(..)` / `xs.unshift(..)` call mutates an evolving array,
                    // also inside an expression (`(xs.push(1), xs).push(2)`).
                    if self.is_array_mutation_call(callee) && !self.current.is_unreachable() {
                        let m = self.add_flow(FlowKind::ArrayMutation, Some(id), self.current.0, 0);
                        self.current = m;
                        if let Some(t) = self.jumps.exception {
                            self.add_ante(t, m);
                        }
                    }
                    if self.commonjs {
                        self.record_commonjs_define_property(id, f, args);
                    }
                    if !ast.source_kind.is_typescript() {
                        self.record_define_property_expando(id, f, args);
                    }
                }
            }
            Node::New {
                callee,
                type_args,
                args,
                ..
            } => {
                self.bind_expr(callee);
                if let Some(t) = type_args {
                    self.bind_type(t);
                }
                for &a in ast.list(args) {
                    self.bind_expr(a);
                }
            }
            Node::ImportCall { arg, options, .. } => {
                if let Node::StringLit { .. } = ast.node(arg) {
                    self.add_request(arg, RequestKind::DynamicImport, false);
                }
                self.bind_expr(arg);
                if let Some(o) = options {
                    self.bind_expr(o);
                }
            }
            Node::FunctionExpr { .. } | Node::Arrow { .. } => self.bind_function_like(id),
            Node::ClassExpr { .. } => self.bind_class(id),
            Node::ObjectLit { .. } => self.bind_object_literal(id),
            Node::As { expr, ty }
            | Node::Satisfies { expr, ty }
            | Node::TypeAssertion { ty, expr } => {
                self.bind_expr(expr);
                self.bind_type(ty);
            }
            Node::Instantiation { expr, type_args } => {
                self.bind_expr(expr);
                self.bind_type(type_args);
            }
            Node::TaggedTemplate {
                tag,
                type_args,
                quasi,
            } => {
                self.bind_expr(tag);
                if let Some(t) = type_args {
                    self.bind_type(t);
                }
                self.bind_expr(quasi);
            }
            Node::JsxElement { .. }
            | Node::JsxOpening { .. }
            | Node::JsxClosing { .. }
            | Node::JsxFragment { .. }
            | Node::JsxAttr { .. }
            | Node::JsxSpreadAttr { .. }
            | Node::JsxExprContainer { .. }
            | Node::JsxSpreadChild { .. } => self.bind_jsx(id),
            Node::Missing {} => {}
            _ => self.bind_any(id),
        }
    }

    fn bind_jsx(&mut self, id: NodeId) {
        let ast = self.ast;
        match *ast.node(id) {
            Node::JsxOpening {
                name,
                type_args,
                attrs,
                ..
            } => {
                self.bind_jsx_tag(name);
                if let Some(t) = type_args {
                    self.bind_type(t);
                }
                for &a in ast.list(attrs) {
                    self.bind_jsx(a);
                }
            }
            Node::JsxClosing { .. } => {}
            Node::JsxAttr { value, .. } => {
                if let Some(v) = value {
                    self.bind_expr(v);
                }
            }
            Node::JsxSpreadAttr { arg } => self.bind_expr(arg),
            Node::JsxExprContainer { expr } => {
                if let Some(e) = expr {
                    self.bind_expr(e);
                }
            }
            Node::JsxSpreadChild { expr } => self.bind_expr(expr),
            _ => {
                let mut kids = Vec::new();
                ast.for_each_child(id, |c| kids.push(c));
                for c in kids {
                    self.bind_expr(c);
                }
            }
        }
    }

    fn bind_jsx_tag(&mut self, name: NodeId) {
        match *self.ast.node(name) {
            Node::Ident { name: a } => {
                let t = self.ast.atom(a);
                // Intrinsic elements start with a lowercase letter and are not references.
                // A value tag is a narrowable reference (`if (C) <C />`).
                if t.chars().next().is_some_and(|c| !c.is_ascii_lowercase()) && !t.contains('-') {
                    self.add_ref(name, meaning::VALUE, false);
                    self.record_flow(name);
                }
            }
            Node::JsxMemberExpr { obj, .. } => {
                self.record_flow(name);
                let mut o = obj;
                while let Node::JsxMemberExpr { obj, .. } = *self.ast.node(o) {
                    self.record_flow(o);
                    o = obj;
                }
                if let Node::Ident { .. } = self.ast.node(o) {
                    self.add_ref(o, meaning::VALUE, false);
                    self.record_flow(o);
                } else {
                    self.bind_expr(o);
                }
            }
            Node::This {} => {}
            _ => {}
        }
    }

    /// Binds a condition, sending its true branch to `t` and its false branch to `f`.
    fn bind_condition(&mut self, expr: NodeId, t: FlowId, f: FlowId) {
        if !self.enter(expr) {
            self.depth -= 1;
            return;
        }
        let ast = self.ast;
        match *ast.node(expr) {
            Node::Paren { expr: inner } => self.bind_condition(inner, t, f),
            Node::Unary {
                op: UnaryOp::Not,
                arg,
            } => {
                // Only a literal `true`/`false` written as the condition is constant.
                self.no_fold += 1;
                self.bind_condition(arg, f, t);
                self.no_fold -= 1;
            }
            Node::Binary {
                left,
                op: op @ (BinaryOp::LogicalAnd | BinaryOp::LogicalOr),
                right,
            } => {
                let mid = self.new_label(FlowKind::BranchLabel);
                if op == BinaryOp::LogicalAnd {
                    self.bind_condition(left, mid, f);
                } else {
                    self.bind_condition(left, t, mid);
                }
                self.current = self.finish_label(mid);
                self.bind_condition(right, t, f);
            }
            _ => {
                self.bind_expr(expr);
                let cur = self.current;
                let tf = self.condition(FlowKind::TrueCondition, cur, expr, 0);
                let ff = self.condition(FlowKind::FalseCondition, cur, expr, 0);
                self.add_ante(t, tf);
                self.add_ante(f, ff);
            }
        }
        self.depth -= 1;
    }

    /// Children of nodes without special handling, dispatched by node category.
    fn bind_any(&mut self, id: NodeId) {
        let mut kids: smallvec_like::Kids = smallvec_like::Kids::new();
        self.ast.for_each_child(id, |c| kids.push(c));
        for c in kids.iter() {
            let k = self.ast.kind(c);
            if is_type_kind(k) {
                self.bind_type(c);
            } else if self.ast.node(c).is_statement() {
                self.bind_stmt(c);
            } else {
                self.bind_expr(c);
            }
        }
    }

    // ---------------------------------------------------------------------------------------
    // Object literals
    // ---------------------------------------------------------------------------------------

    fn bind_object_literal(&mut self, id: NodeId) {
        let Node::ObjectLit { props } = *self.ast.node(id) else {
            return;
        };
        let sym = self.new_symbol(
            Name::OBJECT,
            SymbolFlags::OBJECT_LITERAL,
            id,
            SymbolFacts::NONE,
            SymbolFlags::NONE,
        );
        let members = self.ensure_members(sym);
        const P: u8 = 1;
        const ME: u8 = 2;
        const G: u8 = 4;
        const S: u8 = 8;
        let mut seen: FxHashMap<Name, u8> = FxHashMap::default();
        for &p in self.ast.list(props) {
            if !self.ast.jsdoc_tags.is_empty()
                && matches!(
                    self.ast.node(p),
                    Node::Property { .. } | Node::ShorthandProp { .. }
                )
            {
                self.bind_jsdoc_side_types(p);
            }
            let (key, kind, flags, excl) = match *self.ast.node(p) {
                Node::Property { key, value, .. } => {
                    self.bind_key(key);
                    self.bind_expr(value);
                    (key, P, SymbolFlags::PROPERTY, excludes::OBJ_PROPERTY)
                }
                Node::ShorthandProp { name, init, .. } => {
                    self.add_ref(name, meaning::VALUE, false);
                    self.record_flow(name);
                    if let Some(i) = init {
                        self.bind_default(i);
                    }
                    (name, P, SymbolFlags::PROPERTY, excludes::OBJ_PROPERTY)
                }
                Node::MethodDef {
                    kind, key, value, ..
                } => {
                    self.bind_key(key);
                    self.bind_function_like(value);
                    match kind {
                        MethodKind::Get => (
                            key,
                            G,
                            SymbolFlags::GET_ACCESSOR,
                            excludes::OBJ_GET_ACCESSOR,
                        ),
                        MethodKind::Set => (
                            key,
                            S,
                            SymbolFlags::SET_ACCESSOR,
                            excludes::OBJ_SET_ACCESSOR,
                        ),
                        _ => (key, ME, SymbolFlags::METHOD, excludes::OBJ_METHOD),
                    }
                }
                Node::Spread { arg } => {
                    self.bind_expr(arg);
                    seen.clear();
                    continue;
                }
                _ => {
                    self.bind_expr(p);
                    continue;
                }
            };
            let name = self.key_name(key);
            if name == Name::COMPUTED || name == Name::MISSING {
                self.new_symbol(name, flags, p, SymbolFacts::NONE, excl);
                continue;
            }
            let prev = seen.get(&name).copied().unwrap_or(0);
            let err = match kind {
                P if prev & P != 0 => Some(m::OBJECT_LITERAL_DUPLICATE_PROPERTY),
                P if prev & (ME | G | S) != 0 => Some(m::OBJECT_LITERAL_PROPERTY_AND_ACCESSOR),
                ME if prev & (P | G | S) != 0 => Some(m::OBJECT_LITERAL_PROPERTY_AND_ACCESSOR),
                G | S if prev & (P | ME) != 0 => Some(m::OBJECT_LITERAL_PROPERTY_AND_ACCESSOR),
                G if prev & G != 0 => Some(m::OBJECT_LITERAL_DUPLICATE_ACCESSOR),
                S if prev & S != 0 => Some(m::OBJECT_LITERAL_DUPLICATE_ACCESSOR),
                _ => None,
            };
            if let Some(msg) = err {
                let span = self.ast.span(key);
                self.error(msg, span, &[]);
            }
            seen.insert(name, prev | kind);
            self.declare(members, name, p, flags, excl, SymbolFacts::NONE, Some(sym));
        }
    }

    /// Binds a computed property name's expression.
    fn bind_key(&mut self, key: NodeId) {
        if let Node::ComputedKey { expr } = *self.ast.node(key) {
            self.bind_expr(expr);
        }
    }

    /// The symbol name of a property key (`Name::COMPUTED` for non-literal computed keys).
    fn key_name(&mut self, key: NodeId) -> Name {
        let ast = self.ast;
        match *ast.node(key) {
            Node::Ident { name } | Node::StringLit { value: name } => Name::from_atom(name),
            Node::PrivateName { name } => {
                let s = format!("#{}", ast.atom(name));
                self.names.name_for(ast, &s)
            }
            Node::NumberLit { value } => {
                let s = js_number_to_string(value.get());
                self.names.name_for(ast, &s)
            }
            // A bigint key names no member (it is TS1539; tsc's binder leaves it nameless).
            Node::BigIntLit { .. } => Name::MISSING,
            Node::ComputedKey { expr } => {
                let e = skip_parens(ast, expr);
                match *ast.node(e) {
                    Node::StringLit { .. } | Node::NumberLit { .. } => self.key_name(e),
                    Node::TemplateLit { parts } => {
                        let parts = ast.list(parts);
                        if let [p] = parts
                            && let Node::TemplateElement { cooked, .. } = *ast.node(*p)
                        {
                            Name::from_atom(cooked)
                        } else {
                            Name::COMPUTED
                        }
                    }
                    _ => Name::COMPUTED,
                }
            }
            Node::Missing {} => Name::MISSING,
            _ => Name::COMPUTED,
        }
    }

    fn ident_name(&mut self, id: NodeId) -> Name {
        match *self.ast.node(id) {
            Node::Ident { name } | Node::StringLit { value: name } => Name::from_atom(name),
            _ => Name::MISSING,
        }
    }

    // ---------------------------------------------------------------------------------------
    // Functions
    // ---------------------------------------------------------------------------------------

    /// Declares a function declaration's symbol (hoisted before the other statements).
    fn declare_function(&mut self, id: NodeId) {
        let Node::FunctionDecl {
            modifiers, name, ..
        } = *self.ast.node(id)
        else {
            return;
        };
        if let Some(n) = name
            && !modifiers.contains(Modifiers::DECLARE)
        {
            self.check_eval_arguments(n);
            self.check_reserved(n);
        }
        let facts = self.ambient_facts(modifiers);
        let flags = SymbolFlags::FUNCTION;
        if modifiers.contains(Modifiers::DEFAULT)
            && modifiers.contains(Modifiers::EXPORT)
            && let Some(c) = self.ctx.export_container
            && self.ctx.scope == self.ctx.container_scope
        {
            let table = self.ensure_exports(c);
            let f = facts | SymbolFacts::EXPORTED | SymbolFacts::DEFAULT_EXPORT;
            self.declare(
                table,
                self.default_name,
                id,
                flags,
                excludes::FUNCTION,
                f,
                Some(c),
            );
            if let Some(n) = name {
                let nm = self.ident_name(n);
                let locals = self.locals(self.ctx.scope);
                self.declare(locals, nm, id, flags, excludes::FUNCTION, facts, None);
                // `export default function f() {}; f.x = 1`: expandos of the local `f`.
                if !self.ctx.ambient && !modifiers.contains(Modifiers::DECLARE) {
                    self.expando_targets.push((id, self.ctx.var_scope));
                }
            }
            return;
        }
        let (table, parent, f) = self.decl_table(modifiers, true);
        let nm = match name {
            Some(n) => self.ident_name(n),
            None => Name::FUNCTION,
        };
        self.declare(table, nm, id, flags, excludes::FUNCTION, f | facts, parent);
        if name.is_some() && !self.ctx.ambient && !modifiers.contains(Modifiers::DECLARE) {
            self.expando_targets.push((id, self.ctx.var_scope));
        }
    }

    /// Binds any function-like node: declarations, expressions, arrows, methods, signatures and
    /// function/constructor types.
    fn bind_function_like(&mut self, id: NodeId) {
        let ast = self.ast;
        if !ast.jsdoc_tags.is_empty() {
            self.bind_jsdoc_side_types(id);
        }
        let (name, type_params, params, ret, body, is_arrow, is_signature) = match *ast.node(id) {
            Node::FunctionDecl {
                type_params,
                params,
                return_type,
                body,
                ..
            } => (None, type_params, params, return_type, body, false, false),
            Node::FunctionExpr {
                name,
                type_params,
                params,
                return_type,
                body,
                ..
            } => (name, type_params, params, return_type, body, false, false),
            Node::Arrow {
                type_params,
                params,
                return_type,
                body,
                ..
            } => (
                None,
                type_params,
                params,
                return_type,
                Some(body),
                true,
                false,
            ),
            Node::FunctionType {
                type_params,
                params,
                return_type,
            }
            | Node::ConstructorType {
                type_params,
                params,
                return_type,
                ..
            } => (
                None,
                type_params,
                params,
                Some(return_type),
                None,
                false,
                true,
            ),
            Node::MethodSignature {
                type_params,
                params,
                return_type,
                ..
            }
            | Node::CallSignature {
                type_params,
                params,
                return_type,
            }
            | Node::ConstructSignature {
                type_params,
                params,
                return_type,
            } => (None, type_params, params, return_type, None, false, true),
            _ => return,
        };
        let saved = self.ctx;
        if ast.node(id).modifiers().contains(Modifiers::DECLARE) {
            self.ctx.ambient = true;
        }
        let is_expr = matches!(ast.node(id), Node::FunctionExpr { .. } | Node::Arrow { .. });
        if let Some(n) = name {
            // Named function expression: its name is visible inside.
            self.check_eval_arguments(n);
            self.check_reserved(n);
            self.ctx.scope = self.new_scope(ScopeKind::ExprName, id, None);
            let nm = self.ident_name(n);
            let locals = self.locals(self.ctx.scope);
            self.declare(
                locals,
                nm,
                id,
                SymbolFlags::FUNCTION,
                SymbolFlags::NONE,
                SymbolFacts::NONE,
                None,
            );
        } else if is_expr {
            self.new_symbol(
                Name::FUNCTION,
                SymbolFlags::FUNCTION,
                id,
                SymbolFacts::NONE,
                SymbolFlags::NONE,
            );
        }
        // Type parameters share the function's declaration space with its top-level locals.
        let fscope = self.new_scope(ScopeKind::Function, id, None);
        self.ctx.scope = fscope;
        self.ctx.var_scope = fscope;
        if let Some(tp) = type_params {
            // The type parameters do not see the parameters (`<T extends typeof a>(a: T)`).
            self.ctx.type_params_of = Some(fscope);
            self.declare_type_params(tp);
            self.ctx.type_params_of = None;
        }
        self.ctx.export_container = None;
        self.ctx.implicit_export = false;
        if !is_arrow {
            self.ctx.in_arguments_fn = !is_signature;
        }
        if is_signature {
            self.ctx.in_type = true;
        }
        if let Some(b) = body {
            self.scope_of_node.push((b, fscope));
        }
        // Flow: a function body starts fresh.
        let outer_current = self.current;
        let iife = self.iife.take() == Some(id);
        let saved_jumps = std::mem::take(&mut self.jumps);
        let saved_suppress = if iife {
            self.unreachable_suppress
        } else {
            std::mem::replace(&mut self.unreachable_suppress, 0)
        };
        if iife {
            self.jumps.exception = saved_jumps.exception;
        }
        let detached = self.detached_function.take() == Some(id);
        let start = self.add_flow(
            FlowKind::Start,
            Some(id),
            if is_expr && !detached {
                outer_current.0
            } else {
                0
            },
            0,
        );
        self.current = if iife && outer_current.is_unreachable() {
            FlowId::UNREACHABLE
        } else {
            start
        };
        let ret_label = self.new_label(FlowKind::BranchLabel);
        let seq = self.next_seq();
        self.jumps.return_label = Some((ret_label, seq));
        let table = self.locals(fscope);
        self.ctx.params_of = Some(fscope);
        for &p in ast.list(params) {
            self.bind_param(p, table);
        }
        // The return type sees what the parameters see (`function f(): typeof b { var b }`
        // reads an outer `b`).
        if let Some(r) = ret {
            self.bind_type(r);
        }
        self.ctx.params_of = None;
        if let Some(b) = body {
            match *ast.node(b) {
                Node::Block { stmts } if !is_signature => {
                    if !self.ctx.strict && self.has_use_strict(ast.list(stmts)) {
                        self.ctx.strict = true;
                    }
                    self.bind_stmts(ast.list(stmts))
                }
                _ => self.bind_expr(b),
            }
        }
        let mut after = outer_current;
        if !is_signature {
            let end = self.current;
            let returns = self.finish_label(ret_label);
            self.functions.push(FunctionFlow {
                function: id,
                start,
                end,
                returns,
            });
            if iife {
                let post = self.new_label(FlowKind::BranchLabel);
                self.add_ante(post, end);
                self.add_ante(post, returns);
                after = self.finish_label(post);
            }
        }
        self.jumps = saved_jumps;
        self.unreachable_suppress = saved_suppress;
        self.current = after;
        self.ctx = saved;
    }

    fn bind_param(&mut self, p: NodeId, table: TableId) {
        let ast = self.ast;
        let Node::Param {
            decorators,
            pattern,
            type_ann,
            init,
            ..
        } = *ast.node(p)
        else {
            self.bind_any(p);
            return;
        };
        for &d in ast.list(decorators) {
            self.bind_any(d);
        }
        if let Some(t) = type_ann {
            self.bind_type(t);
        }
        if let Some(i) = init {
            self.bind_default(i);
        }
        if let Node::Ident { name } = *ast.node(pattern) {
            if ast.atom(name) == "this" {
                return;
            }
            self.check_eval_arguments(pattern);
            self.check_reserved(pattern);
            let flags = SymbolFlags::FUNCTION_SCOPED_VARIABLE | SymbolFlags::PARAMETER;
            self.declare(
                table,
                Name::from_atom(name),
                p,
                flags,
                excludes::PARAMETER,
                SymbolFacts::NONE,
                None,
            );
            if init.is_some() {
                self.assignment_flow(pattern);
            }
        } else {
            let kind = BindingKind::Var {
                table,
                flags: SymbolFlags::FUNCTION_SCOPED_VARIABLE | SymbolFlags::PARAMETER,
                excl: excludes::PARAMETER,
                facts: SymbolFacts::NONE,
                parent: None,
            };
            self.bind_binding(pattern, kind, true);
        }
    }

    fn declare_type_params(&mut self, tp: NodeId) {
        let ast = self.ast;
        let Node::TypeParams { params } = *ast.node(tp) else {
            return;
        };
        let table = self.locals(self.ctx.scope);
        for &p in ast.list(params) {
            if let Node::TypeParam { name, .. } = *ast.node(p) {
                self.check_reserved(name);
                let n = self.ident_name(name);
                // A repeated type parameter is reported on the repeat only.
                if let Some(e) = self.lookup_table(table, n)
                    && self.symbols[e.index()]
                        .flags
                        .intersects(SymbolFlags::TYPE_PARAMETER)
                {
                    let span = ast.span(name);
                    let text = self
                        .src
                        .get(span.start as usize..span.end as usize)
                        .unwrap_or("")
                        .to_owned();
                    self.error(m::DUPLICATE_IDENTIFIER, span, &[&text]);
                    self.new_symbol(
                        n,
                        SymbolFlags::TYPE_PARAMETER,
                        p,
                        SymbolFacts::DETACHED,
                        excludes::TYPE_PARAMETER,
                    );
                    continue;
                }
                self.declare(
                    table,
                    n,
                    p,
                    SymbolFlags::TYPE_PARAMETER,
                    excludes::TYPE_PARAMETER,
                    SymbolFacts::NONE,
                    None,
                );
            }
        }
        for &p in ast.list(params) {
            if let Node::TypeParam {
                constraint,
                default,
                ..
            } = *ast.node(p)
            {
                if let Some(c) = constraint {
                    self.bind_type(c);
                }
                if let Some(d) = default {
                    self.bind_type(d);
                }
            }
        }
    }

    /// Opens a type parameter scope (if any) and declares the parameters. The caller restores
    /// the context.
    fn bind_type_params(&mut self, type_params: Option<NodeId>, _owner: NodeId) {
        if let Some(tp) = type_params {
            self.ctx.scope = self.new_scope(ScopeKind::TypeParams, tp, None);
            self.declare_type_params(tp);
        }
    }

    // ---------------------------------------------------------------------------------------
    // Classes, interfaces, enums
    // ---------------------------------------------------------------------------------------

    fn bind_class(&mut self, id: NodeId) {
        let ast = self.ast;
        let (decorators, modifiers, name, type_params, heritage, members, is_expr) =
            match *ast.node(id) {
                Node::ClassDecl {
                    decorators,
                    modifiers,
                    name,
                    type_params,
                    heritage,
                    members,
                } => (
                    decorators,
                    modifiers,
                    name,
                    type_params,
                    heritage,
                    members,
                    false,
                ),
                Node::ClassExpr {
                    decorators,
                    modifiers,
                    name,
                    type_params,
                    heritage,
                    members,
                } => (
                    decorators,
                    modifiers,
                    name,
                    type_params,
                    heritage,
                    members,
                    true,
                ),
                _ => return,
            };
        for &d in ast.list(decorators) {
            self.bind_any(d);
        }
        let mut facts = self.ambient_facts(modifiers);
        if modifiers.contains(Modifiers::ABSTRACT) {
            facts |= SymbolFacts::ABSTRACT;
        }
        facts |= SymbolFacts::BLOCK_SCOPED;
        let saved = self.ctx;
        // The class name and heritage clauses are class (strict) code.
        self.ctx.class_depth += 1;
        self.ctx.strict = true;
        if modifiers.contains(Modifiers::DECLARE) {
            self.ctx.ambient = true;
        }
        if let Some(n) = name {
            self.check_reserved(n);
        }
        self.ctx.class_depth = saved.class_depth;
        let mut default_local = None;
        // A JavaScript class declaration gets expando statics (`C.x = v`, tsc 7).
        if !is_expr
            && name.is_some()
            && !saved.ambient
            && !modifiers.contains(Modifiers::DECLARE)
            && !self.ast.source_kind.is_typescript()
        {
            self.expando_targets.push((id, saved.var_scope));
        }
        let sym = if is_expr {
            let nm = match name {
                Some(n) => self.ident_name(n),
                None => Name::CLASS,
            };
            let s = self.new_symbol(nm, SymbolFlags::CLASS, id, facts, excludes::CLASS);
            if name.is_some() {
                self.ctx.scope = self.new_scope(ScopeKind::ExprName, id, None);
                let locals = self.locals(self.ctx.scope);
                self.table_map.insert((locals.0, nm), s);
            }
            s
        } else if modifiers.contains(Modifiers::DEFAULT)
            && modifiers.contains(Modifiers::EXPORT)
            && let Some(c) = self.ctx.export_container
            && self.ctx.scope == self.ctx.container_scope
        {
            let table = self.ensure_exports(c);
            let f = facts | SymbolFacts::EXPORTED | SymbolFacts::DEFAULT_EXPORT;
            let s = self.declare(
                table,
                self.default_name,
                id,
                SymbolFlags::CLASS,
                excludes::CLASS,
                f,
                Some(c),
            );
            if let Some(n) = name {
                let nm = self.ident_name(n);
                let locals = self.locals(self.ctx.scope);
                let local = self.declare(
                    locals,
                    nm,
                    id,
                    SymbolFlags::CLASS,
                    excludes::CLASS,
                    facts,
                    None,
                );
                if local != s {
                    default_local = Some(local);
                }
            }
            s
        } else {
            let (table, parent, f) = self.decl_table(modifiers, true);
            let nm = match name {
                Some(n) => self.ident_name(n),
                None => Name::CLASS,
            };
            self.declare(
                table,
                nm,
                id,
                SymbolFlags::CLASS,
                excludes::CLASS,
                f | facts,
                parent,
            )
        };
        self.ctx.scope = self.new_scope(ScopeKind::Class, id, Some(sym));
        self.ctx.class_depth += 1;
        self.ctx.strict = true;
        if let Some(tp) = type_params {
            self.ctx.scope = self.new_scope(ScopeKind::TypeParams, tp, None);
            self.declare_type_params(tp);
        }
        for &h in ast.list(heritage) {
            if let Node::HeritageClause { kind, types } = *ast.node(h) {
                for &t in ast.list(types) {
                    if let Node::ExprWithTypeArgs { expr, type_args } = *ast.node(t) {
                        match kind {
                            HeritageKind::Extends => self.bind_expr(expr),
                            HeritageKind::Implements => {
                                let saved_in_type = self.ctx.in_type;
                                self.ctx.in_type = true;
                                self.add_entity_ref(expr, meaning::TYPE);
                                self.ctx.in_type = saved_in_type;
                            }
                        }
                        if let Some(ta) = type_args {
                            self.bind_type(ta);
                        }
                    }
                }
            }
        }
        let inst = self.decl_members_table(sym, id);
        let stat = self.ensure_exports(sym);
        // `export default class C`: the local `C` has the class's members and statics.
        if let Some(local) = default_local
            && self.symbols[local.index()].members.is_none()
            && self.symbols[local.index()].exports.is_none()
        {
            self.symbols[local.index()].members = self.symbols[sym.index()].members;
            self.symbols[local.index()].exports = stat;
        }
        // Flow through a class body, as observed from tsc 7: static blocks run first, in
        // order, and the class continues from the last one; the other members (their
        // computed names) follow on a side path whose assignments do not reach the code
        // after the class (`let a; class C { [(a = 1, "m")]() {} } a;` is TS2454, while
        // a name sees a later static block's assignment).
        let is_block = |m: &NodeId| matches!(ast.node(*m), Node::StaticBlock { .. });
        let saved_class_expr = std::mem::replace(&mut self.in_class_expr, is_expr);
        for &mem in ast.list(members).iter().filter(|m| is_block(m)) {
            self.bind_class_member(mem, sym, inst, stat);
        }
        let after_blocks = self.current;
        for &mem in ast.list(members).iter().filter(|m| !is_block(m)) {
            self.bind_class_member(mem, sym, inst, stat);
        }
        self.in_class_expr = saved_class_expr;
        self.current = after_blocks;
        if !self.ast.source_kind.is_typescript() && !self.ctx.ambient {
            self.declare_this_properties(members, sym, inst, stat);
        }
        self.ctx = saved;
    }

    /// JavaScript classes: `this.x = v` in a constructor, method, accessor or static block
    /// (arrow functions included, nested functions and classes not) declares the property
    /// `x` unless the class declares it. The assignment targets are its declarations
    /// (typed by their values, like expandos).
    fn declare_this_properties(
        &mut self,
        members: ts_r_ast::NodeList,
        class: SymbolId,
        inst: TableId,
        stat: TableId,
    ) {
        let ast = self.ast;
        let mut found: Vec<(bool, Name, NodeId)> = Vec::new();
        let mut stack: Vec<NodeId> = Vec::new();
        for &mem in ast.list(members) {
            let (is_static, body) = match *ast.node(mem) {
                Node::MethodDef {
                    modifiers, value, ..
                } => match *ast.node(value) {
                    Node::FunctionExpr { body: Some(b), .. } => {
                        (modifiers.contains(Modifiers::STATIC), b)
                    }
                    _ => continue,
                },
                Node::StaticBlock { .. } => (true, mem),
                _ => continue,
            };
            stack.clear();
            stack.push(body);
            while let Some(n) = stack.pop() {
                match *ast.node(n) {
                    Node::FunctionDecl { .. }
                    | Node::FunctionExpr { .. }
                    | Node::ClassDecl { .. }
                    | Node::ClassExpr { .. } => continue,
                    Node::Assign {
                        op: ts_r_ast::AssignOp::Assign,
                        left,
                        ..
                    } => {
                        if let Node::Member { obj, prop, .. } = *ast.node(left)
                            && matches!(ast.node(obj), Node::This {})
                            && let Node::Ident { name } = *ast.node(prop)
                        {
                            found.push((is_static, Name::from_atom(name), left));
                        }
                    }
                    _ => {}
                }
                let at = stack.len();
                ast.for_each_child(n, |c| stack.push(c));
                stack[at..].reverse();
            }
        }
        for (is_static, name, left) in found {
            let table = if is_static { stat } else { inst };
            match self.table_map.get(&(table.0, name)).copied() {
                Some(existing) => {
                    if self.symbols[existing.index()]
                        .facts
                        .intersects(SymbolFacts::EXPANDO)
                    {
                        self.add_decl(existing, left);
                    }
                }
                None => {
                    let mut facts = SymbolFacts::EXPANDO;
                    if is_static {
                        facts |= SymbolFacts::STATIC;
                    }
                    let s = self.new_symbol(
                        name,
                        SymbolFlags::PROPERTY,
                        left,
                        facts,
                        SymbolFlags::NONE,
                    );
                    self.symbols[s.index()].parent = Some(class);
                    self.table_map.insert((table.0, name), s);
                }
            }
        }
    }

    fn bind_class_member(&mut self, mem: NodeId, class: SymbolId, inst: TableId, stat: TableId) {
        let ast = self.ast;
        let node = *ast.node(mem);
        let modifiers = node.modifiers();
        let is_static = modifiers.contains(Modifiers::STATIC);
        let table = if is_static { stat } else { inst };
        let mut facts = SymbolFacts::NONE;
        if is_static {
            facts |= SymbolFacts::STATIC;
        }
        if modifiers.contains(Modifiers::ABSTRACT) {
            facts |= SymbolFacts::ABSTRACT;
        }
        if modifiers.contains(Modifiers::READONLY) {
            facts |= SymbolFacts::READONLY;
        }
        match node {
            Node::PropertyDef {
                decorators,
                key,
                optional,
                type_ann,
                value,
                ..
            } => {
                for &d in ast.list(decorators) {
                    self.bind_any(d);
                }
                self.bind_key(key);
                let name = self.key_name(key);
                if matches!(ast.node(key), Node::PrivateName { .. }) {
                    facts |= SymbolFacts::PRIVATE_NAME;
                }
                let mut flags = SymbolFlags::PROPERTY;
                if optional {
                    flags |= SymbolFlags::OPTIONAL;
                }
                self.declare(
                    table,
                    name,
                    mem,
                    flags,
                    excludes::PROPERTY,
                    facts,
                    Some(class),
                );
                if let Some(t) = type_ann {
                    self.bind_type(t);
                }
                if let Some(v) = value {
                    self.bind_initializer(mem, v);
                }
            }
            Node::MethodDef {
                decorators,
                kind,
                key,
                optional,
                value,
                ..
            } => {
                for &d in ast.list(decorators) {
                    self.bind_any(d);
                }
                self.bind_key(key);
                if matches!(ast.node(key), Node::PrivateName { .. }) {
                    facts |= SymbolFacts::PRIVATE_NAME;
                }
                let (name, mut flags, excl) = match kind {
                    MethodKind::Constructor => (
                        Name::CONSTRUCTOR,
                        SymbolFlags::CONSTRUCTOR,
                        SymbolFlags::NONE,
                    ),
                    MethodKind::Get => (
                        self.key_name(key),
                        SymbolFlags::GET_ACCESSOR,
                        excludes::GET_ACCESSOR,
                    ),
                    MethodKind::Set => (
                        self.key_name(key),
                        SymbolFlags::SET_ACCESSOR,
                        excludes::SET_ACCESSOR,
                    ),
                    MethodKind::Method => {
                        (self.key_name(key), SymbolFlags::METHOD, excludes::METHOD)
                    }
                };
                if optional {
                    flags |= SymbolFlags::OPTIONAL;
                }
                if is_static
                    && kind == MethodKind::Method
                    && self.names.text(ast, name) == "prototype"
                {
                    // Conflicts with the constructor's built-in `prototype` property.
                    let span = ast.span(key);
                    let text = self
                        .src
                        .get(span.start as usize..span.end as usize)
                        .unwrap_or("prototype");
                    self.error(m::DUPLICATE_IDENTIFIER, span, &[text]);
                }
                self.declare(table, name, mem, flags, excl, facts, Some(class));
                if kind == MethodKind::Constructor
                    && let Node::FunctionExpr { params, .. } = *ast.node(value)
                {
                    // Parameter properties are also instance members.
                    for &p in ast.list(params) {
                        if let Node::Param {
                            modifiers,
                            pattern,
                            optional,
                            ..
                        } = *ast.node(p)
                            && modifiers.intersects(
                                Modifiers::PUBLIC
                                    | Modifiers::PRIVATE
                                    | Modifiers::PROTECTED
                                    | Modifiers::READONLY
                                    | Modifiers::OVERRIDE,
                            )
                            && let Node::Ident { name } = *ast.node(pattern)
                        {
                            // `public x?: T` declares an optional property.
                            let flags = if optional {
                                SymbolFlags::PROPERTY | SymbolFlags::OPTIONAL
                            } else {
                                SymbolFlags::PROPERTY
                            };
                            let facts = if modifiers.contains(Modifiers::READONLY) {
                                SymbolFacts::READONLY
                            } else {
                                SymbolFacts::NONE
                            };
                            self.declare(
                                inst,
                                Name::from_atom(name),
                                p,
                                flags,
                                excludes::PROPERTY,
                                facts,
                                Some(class),
                            );
                        }
                    }
                }
                // Only methods and accessors of class expressions continue the flow
                // around them (observed: narrowing reaches them, not constructors or
                // members of class declarations).
                if kind == MethodKind::Constructor || !self.in_class_expr {
                    self.detached_function = Some(value);
                }
                self.bind_function_like(value);
            }
            Node::StaticBlock { body } => {
                let saved = self.ctx;
                self.ctx.scope = self.new_scope(ScopeKind::StaticBlock, mem, None);
                self.ctx.var_scope = self.ctx.scope;
                self.ctx.in_arguments_fn = false;
                // A static block runs inline while the class is evaluated: its body
                // continues the surrounding flow and the class continues from its end.
                let saved_jumps = std::mem::take(&mut self.jumps);
                let start = self.add_flow(FlowKind::Start, Some(mem), 0, 0);
                self.bind_stmts(ast.list(body));
                self.functions.push(FunctionFlow {
                    function: mem,
                    start,
                    end: self.current,
                    returns: FlowId::UNREACHABLE,
                });
                self.jumps = saved_jumps;
                self.ctx = saved;
            }
            Node::IndexSignature { .. } => {
                self.declare(
                    table,
                    Name::INDEX,
                    mem,
                    SymbolFlags::SIGNATURE,
                    SymbolFlags::NONE,
                    facts,
                    Some(class),
                );
                self.bind_signature_params(mem);
            }
            _ => self.bind_any(mem),
        }
    }

    /// A class field initializer: its own flow start, `this` is the instance.
    fn bind_initializer(&mut self, owner: NodeId, value: NodeId) {
        let outer = self.current;
        let saved_jumps = std::mem::take(&mut self.jumps);
        let saved_args = self.ctx.in_arguments_fn;
        self.ctx.in_arguments_fn = false;
        let start = self.add_flow(FlowKind::Start, Some(owner), 0, 0);
        self.current = start;
        self.bind_expr(value);
        self.jumps = saved_jumps;
        self.current = outer;
        self.ctx.in_arguments_fn = saved_args;
    }

    /// Index signatures: parameters in their own scope, then the type.
    fn bind_signature_params(&mut self, id: NodeId) {
        let ast = self.ast;
        let Node::IndexSignature {
            params, type_ann, ..
        } = *ast.node(id)
        else {
            return;
        };
        let saved = self.ctx;
        self.ctx.in_type = true;
        self.ctx.scope = self.new_scope(ScopeKind::Function, id, None);
        let table = self.locals(self.ctx.scope);
        for &p in ast.list(params) {
            if let Node::Param {
                pattern, type_ann, ..
            } = *ast.node(p)
            {
                if let Node::Ident { name } = *ast.node(pattern) {
                    let flags = SymbolFlags::FUNCTION_SCOPED_VARIABLE | SymbolFlags::PARAMETER;
                    self.declare(
                        table,
                        Name::from_atom(name),
                        p,
                        flags,
                        excludes::PARAMETER,
                        SymbolFacts::NONE,
                        None,
                    );
                }
                if let Some(t) = type_ann {
                    self.bind_type(t);
                }
            }
        }
        if let Some(t) = type_ann {
            self.bind_type(t);
        }
        self.ctx = saved;
    }

    fn bind_interface(&mut self, id: NodeId) {
        let ast = self.ast;
        let Node::Interface {
            modifiers,
            name,
            type_params,
            extends,
            body,
        } = *ast.node(id)
        else {
            return;
        };
        self.check_decl_name(name, modifiers);
        let nm = self.ident_name(name);
        let facts = self.ambient_facts(modifiers);
        let sym = if modifiers.contains(Modifiers::DEFAULT)
            && modifiers.contains(Modifiers::EXPORT)
            && let Some(c) = self.ctx.export_container
            && self.ctx.scope == self.ctx.container_scope
        {
            let table = self.ensure_exports(c);
            let f = facts | SymbolFacts::EXPORTED | SymbolFacts::DEFAULT_EXPORT;
            let s = self.declare(
                table,
                self.default_name,
                id,
                SymbolFlags::INTERFACE,
                excludes::INTERFACE,
                f,
                Some(c),
            );
            let locals = self.locals(self.ctx.scope);
            self.declare(
                locals,
                nm,
                id,
                SymbolFlags::INTERFACE,
                excludes::INTERFACE,
                facts,
                None,
            );
            s
        } else {
            let (table, parent, f) = self.decl_table(modifiers, true);
            self.declare(
                table,
                nm,
                id,
                SymbolFlags::INTERFACE,
                excludes::INTERFACE,
                f | facts,
                parent,
            )
        };
        let saved = self.ctx;
        self.ctx.in_type = true;
        self.bind_type_params(type_params, id);
        for &e in ast.list(extends) {
            if let Node::ExprWithTypeArgs { expr, type_args } = *ast.node(e) {
                self.add_entity_ref(expr, meaning::TYPE);
                if let Some(ta) = type_args {
                    self.bind_type(ta);
                }
            } else {
                self.bind_type(e);
            }
        }
        // All interface declarations of a symbol share one member table.
        let members = match self.iface_tables.get(&sym) {
            Some(&t) => t,
            None => {
                let t = self.new_table();
                if self.symbols[sym.index()].members.is_none() {
                    self.symbols[sym.index()].members = t;
                }
                self.iface_tables.insert(sym, t);
                t
            }
        };
        self.decl_members.push((id, members));
        for &mem in ast.list(body) {
            self.bind_type_member(mem, sym, members, id);
        }
        self.ctx = saved;
    }

    /// A member of an interface or type literal.
    fn bind_type_member(&mut self, mem: NodeId, owner: SymbolId, table: TableId, body: NodeId) {
        let ast = self.ast;
        match *ast.node(mem) {
            Node::PropertySignature {
                modifiers,
                key,
                optional,
                type_ann,
            } => {
                self.bind_key(key);
                let name = self.key_name(key);
                let mut flags = SymbolFlags::PROPERTY;
                if optional {
                    flags |= SymbolFlags::OPTIONAL;
                }
                let facts = if modifiers.contains(Modifiers::READONLY) {
                    SymbolFacts::READONLY
                } else {
                    SymbolFacts::NONE
                };
                self.check_property_repeat(table, name, mem, body);
                self.declare(
                    table,
                    name,
                    mem,
                    flags,
                    excludes::PROPERTY_SIGNATURE,
                    facts,
                    Some(owner),
                );
                if let Some(t) = type_ann {
                    self.bind_type(t);
                }
            }
            Node::MethodSignature {
                kind,
                key,
                optional,
                ..
            } => {
                self.bind_key(key);
                let name = self.key_name(key);
                let (mut flags, excl) = match kind {
                    MethodKind::Get => (SymbolFlags::GET_ACCESSOR, excludes::GET_ACCESSOR),
                    MethodKind::Set => (SymbolFlags::SET_ACCESSOR, excludes::SET_ACCESSOR),
                    _ => (SymbolFlags::METHOD, excludes::METHOD),
                };
                if optional {
                    flags |= SymbolFlags::OPTIONAL;
                }
                self.declare(
                    table,
                    name,
                    mem,
                    flags,
                    excl,
                    SymbolFacts::NONE,
                    Some(owner),
                );
                self.bind_function_like(mem);
            }
            Node::CallSignature { .. } => {
                self.declare(
                    table,
                    Name::CALL,
                    mem,
                    SymbolFlags::SIGNATURE,
                    SymbolFlags::NONE,
                    SymbolFacts::NONE,
                    Some(owner),
                );
                self.bind_function_like(mem);
            }
            Node::ConstructSignature { .. } => {
                self.declare(
                    table,
                    Name::NEW,
                    mem,
                    SymbolFlags::SIGNATURE,
                    SymbolFlags::NONE,
                    SymbolFacts::NONE,
                    Some(owner),
                );
                self.bind_function_like(mem);
            }
            Node::IndexSignature { .. } => {
                self.declare(
                    table,
                    Name::INDEX,
                    mem,
                    SymbolFlags::SIGNATURE,
                    SymbolFlags::NONE,
                    SymbolFacts::NONE,
                    Some(owner),
                );
                self.bind_signature_params(mem);
            }
            _ => self.bind_any(mem),
        }
    }

    /// A property signature repeated within one interface body or type literal (TS2300 on
    /// each).
    fn check_property_repeat(&mut self, table: TableId, name: Name, mem: NodeId, body: NodeId) {
        if name == Name::COMPUTED || name == Name::MISSING {
            return;
        }
        let Some(existing) = self.lookup_table(table, name) else {
            return;
        };
        if !self.symbols[existing.index()]
            .flags
            .intersects(SymbolFlags::PROPERTY)
        {
            return;
        }
        let body_span = self.ast.span(body);
        let mut same = Vec::new();
        let mut l = self.sym_extra[existing.index()].head;
        while l != NONE {
            let d = self.decl_links[l as usize].0;
            if matches!(self.ast.node(d), Node::PropertySignature { .. })
                && body_span.contains_span(self.ast.span(d))
            {
                same.push(d);
            }
            l = self.decl_links[l as usize].1;
        }
        let Some(&first) = same.first() else { return };
        let text = self.decl_name_text(first, name);
        same.push(mem);
        for d in same {
            let span = self.decl_error_span(d);
            self.error(m::DUPLICATE_IDENTIFIER, span, &[&text]);
        }
    }

    fn bind_enum(&mut self, id: NodeId) {
        let ast = self.ast;
        let Node::Enum {
            modifiers,
            name,
            members,
        } = *ast.node(id)
        else {
            return;
        };
        self.check_decl_name(name, modifiers);
        let nm = self.ident_name(name);
        let (table, parent, f) = self.decl_table(modifiers, true);
        let (flags, excl) = if modifiers.contains(Modifiers::CONST) {
            (SymbolFlags::CONST_ENUM, excludes::CONST_ENUM)
        } else {
            (SymbolFlags::REGULAR_ENUM, excludes::REGULAR_ENUM)
        };
        let facts = f | self.ambient_facts(modifiers) | SymbolFacts::BLOCK_SCOPED;
        let sym = self.declare(table, nm, id, flags, excl, facts, parent);
        let exports = self.ensure_exports(sym);
        let saved = self.ctx;
        self.ctx.scope = self.new_scope(ScopeKind::Enum, id, Some(sym));
        for &mem in ast.list(members) {
            if let Node::EnumMember { name, init } = *ast.node(mem) {
                let n = self.key_name(name);
                self.declare(
                    exports,
                    n,
                    mem,
                    SymbolFlags::ENUM_MEMBER,
                    excludes::ENUM_MEMBER,
                    SymbolFacts::NONE,
                    Some(sym),
                );
                if let Node::ComputedKey { expr } = *ast.node(name) {
                    self.bind_expr(expr);
                }
                if let Some(i) = init {
                    self.bind_expr(i);
                }
            }
        }
        self.ctx = saved;
    }

    // ---------------------------------------------------------------------------------------
    // Namespaces and modules
    // ---------------------------------------------------------------------------------------

    /// 0 = not instantiated, 1 = const enums only, 2 = instantiated.
    fn module_state(&self, body: NodeId) -> u8 {
        self.module_state_at(body, 0, &mut FxHashMap::default())
    }

    /// `module_state` with the depth of `export { .. }` lookups (bounded: a namespace can
    /// re-export a namespace that re-exports it). `memo` holds each (body, depth) already
    /// classified, so namespaces that re-export each other under many names stay polynomial.
    fn module_state_at(
        &self,
        body: NodeId,
        depth: u32,
        memo: &mut FxHashMap<(NodeId, u32), u8>,
    ) -> u8 {
        if let Some(&st) = memo.get(&(body, depth)) {
            return st;
        }
        let st = self.module_state_uncached(body, depth, memo);
        memo.insert((body, depth), st);
        st
    }

    fn module_state_uncached(
        &self,
        body: NodeId,
        depth: u32,
        memo: &mut FxHashMap<(NodeId, u32), u8>,
    ) -> u8 {
        let ast = self.ast;
        let stmts = match *ast.node(body) {
            Node::ModuleBlock { stmts } => ast.list(stmts),
            Node::Module { body: Some(b), .. } => return self.module_state_at(b, depth, memo),
            _ => return 0,
        };
        let mut state = 0u8;
        for &s in stmts {
            let st = match *ast.node(s) {
                Node::Interface { .. } | Node::TypeAlias { .. } => 0,
                Node::Enum { modifiers, .. } if modifiers.contains(Modifiers::CONST) => 1,
                Node::Module { body: Some(b), .. } => self.module_state_at(b, depth, memo),
                Node::Module { body: None, .. } => 0,
                Node::ImportEquals { type_only, .. } => {
                    if type_only {
                        0
                    } else {
                        2
                    }
                }
                Node::ImportDecl { .. } => 0,
                // `export { a, type T }` instantiates the namespace when a local name is a
                // value declared in the block or at the top level of the file (observed: also
                // through `export type { a }`, but not for a type alias without `type`).
                Node::ExportNamed {
                    source: None,
                    specifiers,
                    ..
                } if depth < 8 => {
                    let top = match *ast.node(ast.root) {
                        Node::Program { stmts } => ast.list(stmts),
                        _ => &[],
                    };
                    let value = ast.list(specifiers).iter().any(|&sp| {
                        let Node::ExportSpecifier { local, .. } = *ast.node(sp) else {
                            return false;
                        };
                        let Node::Ident { name } = *ast.node(local) else {
                            return false;
                        };
                        let name = ast.atom(name);
                        self.declares_value(stmts, name, depth, memo)
                            || self.declares_value(top, name, depth, memo)
                    });
                    if value { 2 } else { 0 }
                }
                Node::ExportNamed { source: None, .. } => 0,
                _ => 2,
            };
            state = state.max(st);
            if state == 2 {
                break;
            }
        }
        state
    }

    /// Whether `stmts` declare `name` as a value (variables, functions, classes, non-const
    /// enums, instantiated namespaces, imports).
    fn declares_value(
        &self,
        stmts: &[NodeId],
        name: &str,
        depth: u32,
        memo: &mut FxHashMap<(NodeId, u32), u8>,
    ) -> bool {
        let ast = self.ast;
        let is = |n: Option<NodeId>| {
            n.is_some_and(
                |n| matches!(*ast.node(n), Node::Ident { name: a } if ast.atom(a) == name),
            )
        };
        stmts.iter().any(|&st| match *ast.node(st) {
            Node::VarDecl { decls, .. } => ast.list(decls).iter().any(
                |&d| matches!(*ast.node(d), Node::VarDeclarator { name: n, .. } if is(Some(n))),
            ),
            Node::FunctionDecl { name: n, .. } | Node::ClassDecl { name: n, .. } => is(n),
            Node::Enum {
                modifiers, name: n, ..
            } => !modifiers.contains(Modifiers::CONST) && is(Some(n)),
            Node::Module { name: n, body, .. } => {
                is(Some(n)) && body.is_some_and(|b| self.module_state_at(b, depth + 1, memo) == 2)
            }
            Node::ImportEquals {
                name: n, type_only, ..
            } => !type_only && is(Some(n)),
            _ => false,
        })
    }

    fn bind_module(&mut self, id: NodeId) {
        let ast = self.ast;
        let Node::Module {
            modifiers,
            kind,
            name,
            body,
        } = *ast.node(id)
        else {
            return;
        };
        let at_top = self.ctx.module_depth == 0 && self.ctx.scope == ScopeId(0);
        let quoted = match *ast.node(name) {
            Node::StringLit { value } => Some(ast.atom(value)),
            _ => None,
        };
        // `export` on an ambient module or a global augmentation: TS2668 at the first
        // modifier (tsc: binder.go, bindModuleDeclaration).
        if (quoted.is_some() || kind == ModuleKind::Global)
            && modifiers.contains(Modifiers::EXPORT)
            && let Some(t) = ast.modifier_tokens_of(id).first()
        {
            self.error(m::EXPORT_ON_AMBIENT_MODULE, t.span, &[]);
        }
        // A script's top-level `module "./x"`: TS2436 at the name (tsc: checker.go,
        // checkModuleDeclaration).
        if let Some(text) = quoted
            && kind == ModuleKind::Module
            && !self.is_module
            && at_top
            && is_relative_module_name(text)
        {
            self.error(m::AMBIENT_MODULE_RELATIVE_NAME, ast.span(name), &[]);
        }
        match kind {
            ModuleKind::Global => {
                let ok = (self.is_module && at_top)
                    || (self.ctx.in_ambient_module
                        && self.ctx.module_depth == 1
                        && self.ctx.scope == self.ctx.container_scope);
                // Inside a block or function body the checker reports TS1234 instead.
                let in_body = at_top || self.ctx.scope == self.ctx.container_scope;
                if !ok && in_body {
                    let span = ast.span(name);
                    self.error(m::GLOBAL_AUGMENTATION_PLACEMENT, span, &[]);
                }
                let gsym = match self.global_symbol {
                    Some(g) => {
                        self.add_decl(g, id);
                        g
                    }
                    None => {
                        let g = self.new_symbol(
                            Name::GLOBAL,
                            SymbolFlags::VALUE_MODULE | SymbolFlags::NAMESPACE_MODULE,
                            id,
                            SymbolFacts::AMBIENT,
                            SymbolFlags::NONE,
                        );
                        self.symbols[g.index()].exports = self.global_augmentations;
                        self.global_symbol = Some(g);
                        g
                    }
                };
                let outer = self.ctx.in_global_augmentation;
                self.ctx.in_global_augmentation = true;
                self.bind_module_body(id, body, gsym, true, false);
                self.ctx.in_global_augmentation = outer;
            }
            ModuleKind::Module if matches!(ast.node(name), Node::StringLit { .. }) => {
                // Nested directly in a namespace or a module augmentation: TS2435. Inside
                // blocks/functions the checker reports TS1234 instead; inside a script's
                // ambient module it is accepted. Only top-level augmentations are resolved.
                let nested_ok = self.ctx.in_ambient_module && !self.is_module;
                let nested = !at_top
                    && self.ctx.module_depth > 0
                    && self.ctx.scope == self.ctx.container_scope
                    && !nested_ok;
                if nested {
                    let span = ast.span(name);
                    self.error(m::AMBIENT_MODULE_NESTED, span, &[]);
                }
                // An ambient module (not an augmentation) is a pattern when its name has a
                // `*`; more than one is an error at the name (`tsc: tsc/internal/binder/binder.go`).
                if !(self.is_module && at_top)
                    && let Node::StringLit { value } = *ast.node(name)
                {
                    let text = ast.atom(value);
                    if text.bytes().filter(|&c| c == b'*').count() > 1 {
                        self.error(m::PATTERN_TOO_MANY_ASTERISKS, ast.span(name), &[text]);
                    }
                }
                // A script's `declare module "x"` nested in a namespace (TS2435) is a member
                // of that namespace, not a global ambient module: imports of "x" do not find
                // it (tsc 7.0.2, privacyGloImportParseErrors).
                let table = if self.is_module {
                    self.module_augmentations
                } else if nested {
                    self.new_table()
                } else {
                    self.ambient_modules
                };
                if self.is_module && at_top {
                    self.add_request(name, RequestKind::Augmentation, false);
                }
                let nm = self.ident_name(name);
                let flags = SymbolFlags::VALUE_MODULE
                    | SymbolFlags::NAMESPACE_MODULE
                    | SymbolFlags::MODULE_FILE;
                let sym = self.declare(
                    table,
                    nm,
                    id,
                    flags,
                    SymbolFlags::NONE,
                    SymbolFacts::AMBIENT,
                    None,
                );
                let exports = self.ensure_exports(sym);
                if !self.module_export_tables.contains(&exports) {
                    self.module_export_tables.push(exports);
                }
                self.bind_module_body(id, body, sym, true, true);
            }
            _ => {
                // Namespace `A` or `A.B.C`.
                let mut parts: Vec<NodeId> = Vec::new();
                let mut cur = name;
                loop {
                    match *ast.node(cur) {
                        Node::QualifiedName { left, right } => {
                            parts.push(right);
                            cur = left;
                        }
                        _ => {
                            parts.push(cur);
                            break;
                        }
                    }
                }
                parts.reverse();
                let state = body.map_or(0, |b| self.module_state(b));
                let (flags, excl) = if state > 0 {
                    (
                        SymbolFlags::VALUE_MODULE | SymbolFlags::NAMESPACE_MODULE,
                        excludes::VALUE_MODULE,
                    )
                } else {
                    (SymbolFlags::NAMESPACE_MODULE, excludes::NAMESPACE_MODULE)
                };
                let mut facts = self.ambient_facts(modifiers);
                if state == 1 {
                    facts |= SymbolFacts::CONST_ENUM_ONLY;
                }
                let last = parts.len() - 1;
                let mut sym = None;
                for (i, &part) in parts.iter().enumerate() {
                    self.check_decl_name(part, modifiers);
                    let nm = self.ident_name(part);
                    let decl = if i == last { id } else { part };
                    let s = match sym {
                        None => {
                            let (table, parent, f) = self.decl_table(modifiers, true);
                            self.declare(table, nm, decl, flags, excl, f | facts, parent)
                        }
                        Some(p) => {
                            let t = self.ensure_exports(p);
                            self.declare(
                                t,
                                nm,
                                decl,
                                flags,
                                excl,
                                facts | SymbolFacts::EXPORTED,
                                Some(p),
                            )
                        }
                    };
                    sym = Some(s);
                }
                if let Some(s) = sym {
                    let ambient = self.ctx.ambient || modifiers.contains(Modifiers::DECLARE);
                    self.bind_module_body(id, body, s, ambient, false);
                }
            }
        }
    }

    fn bind_module_body(
        &mut self,
        id: NodeId,
        body: Option<NodeId>,
        sym: SymbolId,
        ambient: bool,
        ambient_module: bool,
    ) {
        let Some(b) = body else { return };
        let ast = self.ast;
        let stmts = match *ast.node(b) {
            Node::ModuleBlock { stmts } => ast.list(stmts),
            _ => return,
        };
        let saved = self.ctx;
        let scope = self.new_scope(ScopeKind::Module, id, Some(sym));
        self.scope_of_node.push((b, scope));
        let exports = self.ensure_exports(sym);
        let locals = self.locals(scope);
        let saved_partner = self.partners.insert(exports.0, locals);
        self.ctx.scope = scope;
        self.ctx.var_scope = scope;
        self.ctx.container_scope = scope;
        self.ctx.export_container = Some(sym);
        self.ctx.ambient = ambient;
        self.ctx.module_depth += 1;
        self.ctx.in_ambient_module = ambient_module;
        self.ctx.implicit_export = ambient && !has_explicit_exports(ast, stmts);
        let outer = self.current;
        let saved_jumps = std::mem::take(&mut self.jumps);
        if !self.current.is_unreachable() {
            // Namespace bodies run when declared; keep the flow but not jump targets.
        }
        self.bind_stmts(stmts);
        match saved_partner {
            Some(p) => self.partners.insert(exports.0, p),
            None => self.partners.remove(&exports.0),
        };
        self.jumps = saved_jumps;
        // A namespace body does not change the flow of the code around it.
        self.current = outer;
        self.ctx = saved;
    }

    // ---------------------------------------------------------------------------------------
    // Imports and exports
    // ---------------------------------------------------------------------------------------

    fn add_request(&mut self, node: NodeId, kind: RequestKind, type_only: bool) -> Option<u32> {
        let Node::StringLit { value } = *self.ast.node(node) else {
            return None;
        };
        let i = self.module_requests.len() as u32;
        self.module_requests.push(ModuleRequest {
            specifier: value,
            node,
            kind,
            type_only,
            in_ambient_module: self.ctx.in_ambient_module && !self.is_module,
            in_augmentation: (self.ctx.in_ambient_module || self.ctx.in_global_augmentation)
                && self.is_module
                && matches!(
                    kind,
                    RequestKind::Import | RequestKind::ExportFrom | RequestKind::ImportEquals
                ),
            resolution_mode: None,
        });
        Some(i)
    }

    /// Records the `resolution-mode` attribute of request `req` (of a type-only import or
    /// export, or an `import()` type).
    fn set_resolution_mode(&mut self, req: Option<u32>, attributes: Option<NodeId>) {
        if let (Some(r), Some(a)) = (req, attributes)
            && let Some(m) = resolution_mode_attribute(self.ast, a)
            && let Some(slot) = self.module_requests.get_mut(r as usize)
        {
            slot.resolution_mode = Some(m);
        }
    }

    fn bind_import(&mut self, id: NodeId) {
        let ast = self.ast;
        let Node::ImportDecl {
            type_only,
            specifiers,
            source,
            attributes,
            ..
        } = *ast.node(id)
        else {
            return;
        };
        let req = self.add_request(source, RequestKind::Import, type_only);
        if type_only {
            self.set_resolution_mode(req, attributes);
        }
        if let Some(a) = attributes {
            self.bind_any(a);
        }
        // Without a module specifier (after a syntax error) the names are still declared,
        // as in tsc, but have no target.
        let table = self.locals(self.ctx.scope);
        for &s in ast.list(specifiers) {
            let (local, import, spec_type_only) = match *ast.node(s) {
                Node::ImportDefaultSpecifier { local } => (local, ImportName::Default, false),
                Node::ImportNamespaceSpecifier { local } => (local, ImportName::Namespace, false),
                Node::ImportSpecifier {
                    type_only,
                    imported,
                    local,
                } => {
                    let n = self.key_name(imported.unwrap_or(local));
                    (local, ImportName::Named(n), type_only)
                }
                _ => continue,
            };
            self.check_reserved(local);
            self.check_eval_arguments_import(local);
            let nm = self.ident_name(local);
            let mut facts = SymbolFacts::NONE;
            if type_only || spec_type_only {
                facts |= SymbolFacts::TYPE_ONLY;
            }
            let sym = self.declare(
                table,
                nm,
                s,
                SymbolFlags::ALIAS,
                excludes::ALIAS,
                facts,
                None,
            );
            if let Some(req) = req {
                self.aliases.push(AliasInfo {
                    symbol: sym,
                    decl: s,
                    target: AliasTarget::Module {
                        request: req,
                        import,
                    },
                });
            }
        }
    }

    fn check_eval_arguments_import(&mut self, _local: NodeId) {}

    fn bind_export_named(&mut self, id: NodeId) {
        let ast = self.ast;
        let Node::ExportNamed {
            type_only,
            specifiers,
            source,
            attributes,
        } = *ast.node(id)
        else {
            return;
        };
        let req = source.and_then(|s| self.add_request(s, RequestKind::ExportFrom, type_only));
        if type_only {
            self.set_resolution_mode(req, attributes);
        }
        if let Some(a) = attributes {
            self.bind_any(a);
        }
        let Some(c) = self.ctx.export_container else {
            // `export {}` outside a module container: still record local references.
            if source.is_none() {
                for &s in ast.list(specifiers) {
                    if let Node::ExportSpecifier { local, .. } = *ast.node(s) {
                        self.add_ref(local, meaning::ALL, false);
                    }
                }
            }
            return;
        };
        let table = self.ensure_exports(c);
        for &s in ast.list(specifiers) {
            let Node::ExportSpecifier {
                type_only: spec_type_only,
                local,
                exported,
            } = *ast.node(s)
            else {
                continue;
            };
            let name = self.key_name(exported.unwrap_or(local));
            let mut facts = SymbolFacts::EXPORTED | SymbolFacts::EXPORT_SPECIFIER;
            if type_only || spec_type_only {
                facts |= SymbolFacts::TYPE_ONLY;
            }
            if name == self.default_name {
                facts |= SymbolFacts::DEFAULT_EXPORT;
            }
            let sym = self.declare(
                table,
                name,
                s,
                SymbolFlags::ALIAS,
                excludes::ALIAS,
                facts,
                Some(c),
            );
            let target = match (source, req) {
                (Some(_), Some(r)) => {
                    let n = self.key_name(local);
                    let import = if n == self.default_name {
                        ImportName::Default
                    } else {
                        ImportName::Named(n)
                    };
                    AliasTarget::Module { request: r, import }
                }
                _ => {
                    if matches!(ast.node(local), Node::Ident { .. }) {
                        self.add_ref(local, meaning::ALL, false);
                    }
                    AliasTarget::Entity(local)
                }
            };
            self.aliases.push(AliasInfo {
                symbol: sym,
                decl: s,
                target,
            });
        }
    }

    // ---------------------------------------------------------------------------------------
    // Types
    // ---------------------------------------------------------------------------------------

    fn bind_type(&mut self, id: NodeId) {
        if self.enter(id) {
            let saved = self.ctx.in_type;
            self.ctx.in_type = true;
            self.bind_type_inner(id);
            self.ctx.in_type = saved;
        }
        self.depth -= 1;
    }

    fn bind_type_inner(&mut self, id: NodeId) {
        let ast = self.ast;
        match *ast.node(id) {
            Node::TypeRef { name, type_args } => {
                self.add_entity_ref(name, meaning::TYPE);
                if let Some(t) = type_args {
                    self.bind_type(t);
                }
            }
            Node::TypeQuery {
                expr_name,
                type_args,
            } => {
                match *ast.node(expr_name) {
                    // `typeof x` reads `x` where it stands: the checker narrows it there.
                    Node::Ident { .. } => {
                        self.add_entity_ref(expr_name, meaning::VALUE);
                        self.record_flow(expr_name);
                    }
                    // `typeof x.p` and `typeof this.a.b` narrow the reference: the name and
                    // its leftmost identifier record where they stand.
                    Node::QualifiedName { .. } => {
                        self.add_entity_ref(expr_name, meaning::VALUE);
                        self.record_flow(expr_name);
                        let mut left = expr_name;
                        while let Node::QualifiedName { left: l, .. } = *ast.node(left) {
                            left = l;
                        }
                        if matches!(ast.node(left), Node::Ident { .. }) {
                            self.record_flow(left);
                        }
                    }
                    _ => self.bind_type(expr_name),
                }
                if let Some(t) = type_args {
                    self.bind_type(t);
                }
            }
            Node::ImportType {
                arg,
                attributes,
                type_args,
                ..
            } => {
                let lit = match *ast.node(arg) {
                    Node::StringLit { .. } => Some(arg),
                    _ => {
                        let mut found = None;
                        ast.for_each_child(arg, |c| {
                            if found.is_none() && matches!(ast.node(c), Node::StringLit { .. }) {
                                found = Some(c);
                            }
                        });
                        found
                    }
                };
                if let Some(l) = lit {
                    let req = self.add_request(l, RequestKind::ImportType, true);
                    self.set_resolution_mode(req, attributes);
                }
                if let Some(a) = attributes {
                    self.bind_any(a);
                }
                if let Some(t) = type_args {
                    self.bind_type(t);
                }
            }
            Node::TypeLiteral { members } => {
                let sym = self.new_symbol(
                    Name::TYPE,
                    SymbolFlags::TYPE_LITERAL,
                    id,
                    SymbolFacts::NONE,
                    SymbolFlags::NONE,
                );
                let table = self.ensure_members(sym);
                for &mem in ast.list(members) {
                    self.bind_type_member(mem, sym, table, id);
                }
            }
            Node::FunctionType { .. } | Node::ConstructorType { .. } => self.bind_function_like(id),
            Node::MappedType {
                type_param,
                name_type,
                ty,
                ..
            } => {
                if let Node::TypeParam {
                    name, constraint, ..
                } = *ast.node(type_param)
                {
                    if let Some(c) = constraint {
                        self.bind_type(c);
                    }
                    let saved = self.ctx;
                    self.ctx.scope = self.new_scope(ScopeKind::TypeParams, id, None);
                    let table = self.locals(self.ctx.scope);
                    let n = self.ident_name(name);
                    self.declare(
                        table,
                        n,
                        type_param,
                        SymbolFlags::TYPE_PARAMETER,
                        excludes::TYPE_PARAMETER,
                        SymbolFacts::NONE,
                        None,
                    );
                    if let Some(n) = name_type {
                        self.bind_type(n);
                    }
                    if let Some(t) = ty {
                        self.bind_type(t);
                    }
                    self.ctx = saved;
                }
            }
            Node::ConditionalType {
                check,
                extends,
                true_ty,
                false_ty,
            } => {
                self.bind_type(check);
                let saved = self.ctx;
                let scope = self.new_scope(ScopeKind::TypeParams, id, None);
                self.ctx.infer_scope = Some(scope);
                self.bind_type(extends);
                self.ctx.infer_scope = saved.infer_scope;
                self.ctx.scope = scope;
                self.bind_type(true_ty);
                self.ctx = saved;
                self.bind_type(false_ty);
            }
            Node::InferType { type_param } => {
                if let Node::TypeParam {
                    name, constraint, ..
                } = *ast.node(type_param)
                {
                    if let Some(scope) = self.ctx.infer_scope {
                        let table = self.locals(scope);
                        let n = self.ident_name(name);
                        self.declare(
                            table,
                            n,
                            type_param,
                            SymbolFlags::TYPE_PARAMETER,
                            excludes::INFER_TYPE_PARAMETER,
                            SymbolFacts::NONE,
                            None,
                        );
                    }
                    if let Some(c) = constraint {
                        self.bind_type(c);
                    }
                }
            }
            Node::TypePredicate { ty, .. } => {
                if let Some(t) = ty {
                    self.bind_type(t);
                }
            }
            Node::NamedTupleMember { ty, .. } => self.bind_type(ty),
            Node::MethodSignature { .. }
            | Node::CallSignature { .. }
            | Node::ConstructSignature { .. } => self.bind_function_like(id),
            Node::Missing {} => {}
            _ => {
                let mut kids = smallvec_like::Kids::new();
                ast.for_each_child(id, |c| kids.push(c));
                for c in kids.iter() {
                    if is_type_kind(ast.kind(c)) {
                        self.bind_type(c);
                    } else {
                        // Literal types (`"a"`, `-1`) and template parts: expressions.
                        self.bind_expr(c);
                    }
                }
            }
        }
    }

    // ---------------------------------------------------------------------------------------
    // Finish
    // ---------------------------------------------------------------------------------------

    fn finish(mut self, file_sym: SymbolId) -> BoundFile {
        let resolved = crate::resolve::resolve_refs(&mut self);
        self.bind_expandos(&resolved);
        self.bind_commonjs_exports(&resolved, file_sym);
        // Declarations, contiguous per symbol.
        let mut decl_nodes = Vec::with_capacity(self.decl_links.len());
        for (i, sym) in self.symbols.iter_mut().enumerate() {
            let start = decl_nodes.len() as u32;
            let mut l = self.sym_extra[i].head;
            while l != NONE {
                decl_nodes.push(self.decl_links[l as usize].0);
                l = self.decl_links[l as usize].1;
            }
            sym.decl_start = start;
            sym.decl_len = decl_nodes.len() as u32 - start;
        }
        // Tables, sorted by name.
        let mut entries: Vec<(u32, Name, SymbolId)> = self
            .table_map
            .iter()
            .map(|(&(t, n), &s)| (t, n, s))
            .collect();
        entries.sort_unstable_by_key(|&(t, n, _)| (t, n));
        let mut tables = vec![TableRange::default(); self.table_count as usize];
        let mut table_entries = Vec::with_capacity(entries.len());
        let mut i = 0;
        while i < entries.len() {
            let t = entries[i].0;
            let start = table_entries.len() as u32;
            while i < entries.len() && entries[i].0 == t {
                table_entries.push((entries[i].1, entries[i].2));
                i += 1;
            }
            tables[t as usize] = TableRange {
                start,
                len: table_entries.len() as u32 - start,
            };
        }
        // Flow labels.
        let mut lists = std::mem::take(&mut self.flow_lists);
        for node in self.flow_nodes.iter_mut() {
            if matches!(node.kind, FlowKind::BranchLabel | FlowKind::LoopLabel) {
                let ante = &self.label_ante[node.a as usize];
                let start = lists.len() as u32;
                lists.extend_from_slice(ante);
                node.a = start;
                node.b = ante.len() as u32;
            }
        }
        // A node recorded twice (the target of `x ||= y` is read, then assigned) keeps its
        // first flow, the read: the sort must be stable for that (and for determinism).
        self.node_flow.sort_by_key(|&(n, _)| n);
        self.node_flow.dedup_by_key(|&mut (n, _)| n);
        self.functions.sort_unstable_by_key(|f| f.function);
        self.stmt_flow.sort_by_key(|&(n, _)| n);
        self.stmt_flow.dedup_by_key(|&mut (n, _)| n);
        self.scope_of_node.sort_by_key(|&(n, _)| n);
        self.symbol_of_decl.sort_by_key(|&(n, s)| (n, s));
        self.decl_members.sort_by_key(|&(n, _)| n);
        self.aliases.sort_by_key(|a| a.symbol);
        // Diagnostics.
        if self.opts.allow_unreachable_code == Some(false) {
            for &sp in &self.unreachable {
                self.diags
                    .push(Diagnostic::new(m::UNREACHABLE_CODE, sp, &[]));
            }
        }
        if self.opts.allow_unused_labels == Some(false) {
            for &l in &self.unused_labels {
                let sp = self.ast.span(l);
                self.diags.push(Diagnostic::new(m::UNUSED_LABEL, sp, &[]));
            }
        }
        self.diags.sort_by(|a, b| {
            (a.span.start, a.span.end, a.code, &a.message).cmp(&(
                b.span.start,
                b.span.end,
                b.code,
                &b.message,
            ))
        });
        self.diags
            .dedup_by(|a, b| a.span == b.span && a.code == b.code && a.message == b.message);
        BoundFile {
            is_module: self.is_module,
            commonjs: self.commonjs,
            is_declaration: self.ast.source_kind.is_declaration(),
            file_symbol: file_sym,
            symbols: self.symbols,
            decl_nodes,
            tables,
            table_entries,
            scopes: self.scopes,
            scope_of_node: self.scope_of_node,
            symbol_of_decl: self.symbol_of_decl,
            decl_members: self.decl_members,
            flow: FlowGraph {
                nodes: self.flow_nodes,
                lists,
                node_flow: self.node_flow,
                functions: self.functions,
                fallthroughs: self.fallthroughs,
                stmt_flow: self.stmt_flow,
            },
            refs: resolved,
            aliases: self.aliases,
            module_requests: self.module_requests,
            export_stars: self.export_stars,
            ambient_modules: self.ambient_modules,
            module_augmentations: self.module_augmentations,
            global_augmentations: self.global_augmentations,
            umd_name: self.umd_name,
            names: self.names,
            diagnostics: self.diags,
            unreachable: self.unreachable,
            unused_labels: self.unused_labels,
            late_expandos: self.late_expandos,
        }
    }

    /// Records `Object.defineProperty(o, "x", d)` in JavaScript: a candidate expando
    /// property `x` of `o` declared by the call (read-only unless `d` has a setter or
    /// `writable: true`).
    fn record_define_property_expando(&mut self, call: NodeId, callee: NodeId, args: NodeList) {
        let ast = self.ast;
        let is_ident = |n: NodeId, t: &str| matches!(*ast.node(n), Node::Ident { name } if ast.atom(name) == t);
        let Node::Member { obj, prop, .. } = *ast.node(callee) else {
            return;
        };
        if !is_ident(obj, "Object") || !is_ident(prop, "defineProperty") {
            return;
        }
        let [o, k, _, ..] = ast.list(args) else {
            return;
        };
        let Node::StringLit { value } = *ast.node(skip_parens(ast, *k)) else {
            return;
        };
        let o = skip_parens(ast, *o);
        if matches!(*ast.node(o), Node::Ident { name } if ast.atom(name) != "exports") {
            self.expando_assigns
                .push((call, o, Name::from_atom(value), self.ctx.var_scope));
        }
    }

    /// Records `f.x = v` / `f["x"] = v` (a candidate expando property of `f`).
    fn record_expando_assign(&mut self, left: NodeId) {
        let ast = self.ast;
        let (obj, name) = match *ast.node(left) {
            Node::Member { obj, prop, .. } => match *ast.node(prop) {
                Node::Ident { name } => (obj, Name::from_atom(name)),
                _ => return,
            },
            Node::Index { obj, index, .. } => match *ast.node(skip_parens(ast, index)) {
                Node::StringLit { value } => (obj, Name::from_atom(value)),
                Node::NumberLit { value } => {
                    let s = js_number_to_string(value.get());
                    (obj, self.names.name_for(ast, &s))
                }
                // `` f[`x`] = v ``: a template without substitutions names `x`.
                Node::TemplateLit { parts } if ast.list(parts).len() == 1 => {
                    match *ast.node(ast.list(parts)[0]) {
                        Node::TemplateElement { cooked, .. } => (obj, Name::from_atom(cooked)),
                        _ => (obj, Name::COMPUTED),
                    }
                }
                // A late-bound name (`f[sym] = v`): the checker names it by the key's type
                // (a unique symbol or a literal).
                Node::Ident { .. } | Node::Member { .. }
                    if is_entity_name_expr(ast, skip_parens(ast, index))
                        && matches!(ast.node(obj), Node::Ident { .. }) =>
                {
                    self.expando_late.push((left, obj, self.ctx.var_scope));
                    return;
                }
                // Any other key: the function gets no expando properties at all.
                _ => (obj, Name::COMPUTED),
            },
            _ => return,
        };
        // (`F.prototype = {...}` too: tsc 7 has no constructor functions, so it is an
        // ordinary expando property.)
        if matches!(ast.node(obj), Node::Ident { .. }) {
            self.expando_assigns
                .push((left, obj, name, self.ctx.var_scope));
        }
    }

    /// `exports.x = v`, `module.exports.x = v` (also `["x"]`) and `module.exports = v` in a
    /// CommonJS module: candidates for [`Binder::bind_commonjs_exports`].
    fn record_commonjs_export(&mut self, assign: NodeId, left: NodeId) {
        let ast = self.ast;
        let is_ident = |n: NodeId, t: &str| matches!(*ast.node(n), Node::Ident { name } if ast.atom(name) == t);
        // `module.exports` / `module["exports"]`: the `module` identifier.
        let module_exports = |n: NodeId| match *ast.node(n) {
            Node::Member { obj, prop, .. }
                if is_ident(obj, "module") && is_ident(prop, "exports") =>
            {
                Some(obj)
            }
            Node::Index { obj, index, .. }
                if is_ident(obj, "module")
                    && matches!(*ast.node(skip_parens(ast, index)), Node::StringLit { value } if ast.atom(value) == "exports") =>
            {
                Some(obj)
            }
            _ => None,
        };
        if let Some(root) = module_exports(left) {
            self.commonjs_exports
                .push((assign, left, root, Name::EXPORT_EQUALS));
            return;
        }
        let (obj, name) = match *ast.node(left) {
            Node::Member { obj, prop, .. } => match *ast.node(prop) {
                Node::Ident { name } => (obj, Name::from_atom(name)),
                _ => return,
            },
            Node::Index { obj, index, .. } => match *ast.node(skip_parens(ast, index)) {
                Node::StringLit { value } => (obj, Name::from_atom(value)),
                _ => return,
            },
            _ => return,
        };
        let root = if is_ident(obj, "exports") {
            obj
        } else if let Some(m) = module_exports(obj) {
            m
        } else {
            return;
        };
        self.commonjs_exports.push((assign, left, root, name));
    }

    /// `Object.defineProperty(exports, "x", d)` (or `module.exports`) in a CommonJS module:
    /// an export `x` declared by the call.
    fn record_commonjs_define_property(&mut self, call: NodeId, callee: NodeId, args: NodeList) {
        let ast = self.ast;
        let is_ident = |n: NodeId, t: &str| matches!(*ast.node(n), Node::Ident { name } if ast.atom(name) == t);
        let Node::Member { obj, prop, .. } = *ast.node(callee) else {
            return;
        };
        if !is_ident(obj, "Object") || !is_ident(prop, "defineProperty") {
            return;
        }
        let [o, k, _, ..] = ast.list(args) else {
            return;
        };
        let Node::StringLit { value } = *ast.node(skip_parens(ast, *k)) else {
            return;
        };
        let root = match *ast.node(skip_parens(ast, *o)) {
            Node::Ident { name } if ast.atom(name) == "exports" => skip_parens(ast, *o),
            Node::Member { obj, prop, .. }
                if is_ident(obj, "module") && is_ident(prop, "exports") =>
            {
                obj
            }
            _ => return,
        };
        self.commonjs_exports
            .push((call, call, root, Name::from_atom(value)));
    }

    /// Declares the CommonJS exports of a JavaScript module in the file's exports (where
    /// `exports`/`module` is not a declared name): `exports.x = v` a property `x` (declared
    /// by each assignment target, typed like an expando), `module.exports = v` the module's
    /// `export=` (an alias when `v` is a name, else the assigned expression).
    fn bind_commonjs_exports(&mut self, refs: &Refs, file_sym: SymbolId) {
        if self.commonjs_exports.is_empty() {
            return;
        }
        // (`Object.defineProperty(o, "name", ...)` on the name assigned to `module.exports`
        // declares an expando property of `o`: `record_define_property_expando`.)
        let ast = self.ast;
        let resolved = |n: NodeId| refs.resolved.binary_search_by_key(&n, |&(r, _)| r).is_ok();
        let table = self.ensure_exports(file_sym);
        for (assign, left, root, name) in std::mem::take(&mut self.commonjs_exports) {
            if resolved(root) {
                continue;
            }
            if name == Name::EXPORT_EQUALS {
                let Node::Assign { right, .. } = *ast.node(assign) else {
                    continue;
                };
                let value = skip_parens(ast, right);
                // `module.exports = exports` changes nothing.
                if matches!(*ast.node(value), Node::Ident { name } if ast.atom(name) == "exports") {
                    continue;
                }
                if let Some(existing) = self.table_map.get(&(table.0, name)).copied() {
                    self.add_decl(existing, assign);
                    continue;
                }
                let entity = self.is_entity_name(value);
                let flags = if entity {
                    SymbolFlags::ALIAS
                } else {
                    SymbolFlags::EXPORT_EXPRESSION
                };
                let s = self.new_symbol(
                    name,
                    flags,
                    assign,
                    SymbolFacts::EXPORTED,
                    SymbolFlags::NONE,
                );
                self.symbols[s.index()].parent = Some(file_sym);
                self.table_map.insert((table.0, name), s);
                if entity {
                    self.aliases.push(AliasInfo {
                        symbol: s,
                        decl: assign,
                        target: AliasTarget::Entity(value),
                    });
                }
                continue;
            }
            match self.table_map.get(&(table.0, name)).copied() {
                Some(existing) => {
                    if self.symbols[existing.index()]
                        .facts
                        .intersects(SymbolFacts::EXPANDO)
                    {
                        self.add_decl(existing, left);
                    }
                }
                None => {
                    // `Object.defineProperty(exports, "x", d)` without a setter or
                    // `writable: true` is read-only.
                    let readonly = match *ast.node(left) {
                        Node::Call { args, .. } => !define_property_writable(ast, args),
                        _ => false,
                    };
                    let mut facts = SymbolFacts::EXPANDO
                        | SymbolFacts::EXPORTED
                        | SymbolFacts::EXPORT_SPECIFIER;
                    if readonly {
                        facts |= SymbolFacts::READONLY;
                    }
                    let s = self.new_symbol(
                        name,
                        SymbolFlags::PROPERTY,
                        left,
                        facts,
                        SymbolFlags::NONE,
                    );
                    self.symbols[s.index()].parent = Some(file_sym);
                    self.table_map.insert((table.0, name), s);
                }
            }
        }
    }

    /// Declares expando properties: an assignment `f.x = v` in the same function (or file,
    /// or namespace body) as the declaration of the function `f` it resolves to adds `x` to
    /// `f`'s exports, declared by the assignment target (TypeScript 3.1).
    fn bind_expandos(&mut self, refs: &Refs) {
        if (self.expando_assigns.is_empty() && self.expando_late.is_empty())
            || self.expando_targets.is_empty()
        {
            return;
        }
        let mut targets: FxHashMap<SymbolId, ScopeId> = FxHashMap::default();
        let decl_scope: FxHashMap<NodeId, ScopeId> = self.expando_targets.iter().copied().collect();
        // `export default function f() {}` has a local and an export symbol: the expandos
        // go in both tables (one property symbol).
        let mut sibling: FxHashMap<SymbolId, SymbolId> = FxHashMap::default();
        let mut first_of: FxHashMap<NodeId, SymbolId> = FxHashMap::default();
        for &(decl, sym) in &self.symbol_of_decl {
            if let Some(&sc) = decl_scope.get(&decl) {
                targets.insert(sym, sc);
                match first_of.get(&decl) {
                    Some(&other) if other != sym => {
                        sibling.insert(other, sym);
                        sibling.insert(sym, other);
                    }
                    _ => {
                        first_of.insert(decl, sym);
                    }
                }
            }
        }
        // `var C = class {}`: the variable's expandos go to the class symbol.
        let mut owner: FxHashMap<SymbolId, SymbolId> = FxHashMap::default();
        if !self.expando_classes.is_empty() {
            let by_decl: FxHashMap<NodeId, SymbolId> =
                self.symbol_of_decl.iter().copied().collect();
            for &(name, class) in &self.expando_classes {
                if let (Some(&v), Some(&c)) = (by_decl.get(&name), by_decl.get(&class)) {
                    owner.insert(v, c);
                }
            }
        }
        let assigns = std::mem::take(&mut self.expando_assigns);
        let resolve = |obj: NodeId| {
            refs.resolved
                .binary_search_by_key(&obj, |&(n, _)| n)
                .ok()
                .map(|i| refs.resolved[i].1)
        };
        for &(_, obj, name, _) in &assigns {
            if name == Name::COMPUTED
                && let Some(t) = resolve(obj)
            {
                targets.remove(&t);
            }
        }
        for (left, obj, scope) in std::mem::take(&mut self.expando_late) {
            if let Some(t) = resolve(obj)
                && targets.get(&t) == Some(&scope)
            {
                self.late_expandos.push((t, left));
            }
        }
        self.late_expandos.sort_unstable();
        for (left, obj, name, scope) in assigns {
            let Some(target) = resolve(obj) else {
                continue;
            };
            if name == Name::COMPUTED || targets.get(&target) != Some(&scope) {
                continue;
            }
            let target = owner.get(&target).copied().unwrap_or(target);
            let table = self.ensure_exports(target);
            match self.table_map.get(&(table.0, name)).copied() {
                Some(existing) => {
                    if self.symbols[existing.index()]
                        .facts
                        .intersects(SymbolFacts::EXPANDO)
                    {
                        self.add_decl(existing, left);
                    }
                }
                None => {
                    let readonly = match *self.ast.node(left) {
                        Node::Call { args, .. } => !define_property_writable(self.ast, args),
                        _ => false,
                    };
                    let facts = if readonly {
                        SymbolFacts::EXPANDO | SymbolFacts::READONLY
                    } else {
                        SymbolFacts::EXPANDO
                    };
                    let s = self.new_symbol(
                        name,
                        SymbolFlags::PROPERTY,
                        left,
                        facts,
                        SymbolFlags::NONE,
                    );
                    self.symbols[s.index()].parent = Some(target);
                    self.table_map.insert((table.0, name), s);
                    if let Some(&other) = sibling.get(&target) {
                        let t2 = self.ensure_exports(other);
                        self.table_map.entry((t2.0, name)).or_insert(s);
                    }
                }
            }
        }
    }

    // Accessors for the resolver pass.
    pub(crate) fn take_refs(&mut self) -> Vec<PendingRef> {
        std::mem::take(&mut self.refs)
    }
    pub(crate) fn lookup_table(&self, table: TableId, name: Name) -> Option<SymbolId> {
        self.table_map.get(&(table.0, name)).copied()
    }
    pub(crate) fn scopes(&self) -> &[Scope] {
        &self.scopes
    }
    pub(crate) fn global_augmentations_table(&self) -> TableId {
        self.global_augmentations
    }
    pub(crate) fn scope_in_params(&self, s: ScopeId) -> bool {
        self.scope_in_params[s.0 as usize]
    }
    pub(crate) fn scope_in_type_params(&self, s: ScopeId) -> bool {
        self.scope_in_type_params[s.0 as usize]
    }
    pub(crate) fn symbol_ref(&self, s: SymbolId) -> &Symbol {
        &self.symbols[s.index()]
    }
    pub(crate) fn mark_assigned(&mut self, s: SymbolId) {
        self.symbols[s.index()].facts |= SymbolFacts::ASSIGNED;
    }
}

#[derive(Clone, Copy)]
enum BindingKind {
    Var {
        table: TableId,
        flags: SymbolFlags,
        excl: SymbolFlags,
        facts: SymbolFacts,
        parent: Option<SymbolId>,
    },
}

fn is_type_kind(k: NodeKind) -> bool {
    use NodeKind::*;
    matches!(
        k,
        KeywordType
            | TypeRef
            | QualifiedName
            | ThisType
            | TemplateLitType
            | ArrayType
            | TupleType
            | NamedTupleMember
            | OptionalType
            | RestType
            | UnionType
            | IntersectionType
            | FunctionType
            | ConstructorType
            | TypeLiteral
            | PropertySignature
            | MethodSignature
            | CallSignature
            | ConstructSignature
            | TypeQuery
            | TypeOperator
            | IndexedAccessType
            | ConditionalType
            | InferType
            | MappedType
            | ParenType
            | TypePredicate
            | ImportType
            | TypeParams
            | TypeParam
            | TypeArgs
    )
}

/// An export clause (`export {}`, `export … from`, `export *`) or an export assignment
/// (`export =`, `export default <expr>`) turns off the implicit exports of an ambient body.
/// `export default class`/`function` and `export as namespace` do not (observed with tsc 7).
fn has_explicit_exports(ast: &Ast, stmts: &[NodeId]) -> bool {
    stmts.iter().any(|&s| {
        matches!(
            ast.node(s),
            Node::ExportNamed { .. }
                | Node::ExportAll { .. }
                | Node::ExportAssignment { .. }
                | Node::ExportDefaultExpr { .. }
        )
    })
}

/// Top-level `import`/`export` makes a file a module; in JavaScript files so do CommonJS
/// `exports.x =` / `module.exports =` assignments and top-level `require(...)` initializers.
fn file_is_module(ast: &Ast) -> bool {
    let Node::Program { stmts } = *ast.node(ast.root) else {
        return false;
    };
    if !ast.source_kind.is_typescript()
        && ast
            .list(stmts)
            .iter()
            .any(|&s| is_commonjs_indicator(ast, s))
    {
        return true;
    }
    // `import.meta` anywhere marks the file as a module.
    let uses_import_meta = (1..=ast.node_count() as u32)
        .filter_map(NodeId::from_raw)
        .any(|n| {
            matches!(
                ast.node(n),
                Node::MetaProperty {
                    kind: ts_r_ast::MetaPropKind::ImportMeta
                }
            )
        });
    uses_import_meta
        || ast.list(stmts).iter().any(|&s| match *ast.node(s) {
            // A JSDoc `@import` does not make a module (tsc 7: its names are global).
            Node::ImportDecl { .. } => !ast.is_jsdoc(s),
            Node::ExportNamed { .. }
            | Node::ExportAll { .. }
            | Node::ExportDefaultExpr { .. }
            | Node::ExportAssignment { .. } => true,
            Node::ImportEquals {
                modifiers,
                module_ref,
                ..
            } => {
                modifiers.contains(Modifiers::EXPORT)
                    || matches!(ast.node(module_ref), Node::ExternalModuleRef { .. })
            }
            ref n => n.modifiers().contains(Modifiers::EXPORT),
        })
}

/// Whether the descriptor of `Object.defineProperty(o, k, d)` makes the property writable:
/// a `set` member or `writable: true`.
fn define_property_writable(ast: &Ast, args: NodeList) -> bool {
    let Some(&d) = ast.list(args).get(2) else {
        return true;
    };
    let Node::ObjectLit { props } = *ast.node(skip_parens(ast, d)) else {
        return true;
    };
    let key = |k: NodeId| match *ast.node(k) {
        Node::Ident { name } => Some(ast.atom(name)),
        Node::StringLit { value } => Some(ast.atom(value)),
        _ => None,
    };
    ast.list(props).iter().any(|&p| match *ast.node(p) {
        Node::Property { key: k, value, .. } => match key(k) {
            Some("set") => true,
            Some("writable") => matches!(
                *ast.node(skip_parens(ast, value)),
                Node::BoolLit { value: true }
            ),
            _ => false,
        },
        Node::MethodDef { key: k, .. } => key(k) == Some("set"),
        Node::ShorthandProp { name, .. } => key(name) == Some("set"),
        Node::Spread { .. } => true,
        _ => false,
    })
}

/// Top-level ECMAScript module syntax (`import`/`export` statements or modifiers).
fn has_esm_syntax(ast: &Ast) -> bool {
    let Node::Program { stmts } = *ast.node(ast.root) else {
        return false;
    };
    ast.list(stmts).iter().any(|&s| match *ast.node(s) {
        Node::ImportDecl { .. }
        | Node::ExportNamed { .. }
        | Node::ExportAll { .. }
        | Node::ExportDefaultExpr { .. }
        | Node::ExportAssignment { .. } => true,
        ref n => n.modifiers().contains(Modifiers::EXPORT),
    })
}

fn is_commonjs_indicator(ast: &Ast, s: NodeId) -> bool {
    let is_ident =
        |n: NodeId, t: &str| matches!(*ast.node(n), Node::Ident { name } if ast.atom(name) == t);
    // `exports`, `module.exports`, possibly followed by `.x` / `["x"]`.
    let exports_root = |mut n: NodeId| loop {
        match *ast.node(n) {
            Node::Ident { .. } => return is_ident(n, "exports"),
            Node::Member { obj, prop, .. } => {
                if is_ident(obj, "module") && is_ident(prop, "exports") {
                    return true;
                }
                n = obj;
            }
            Node::Index { obj, .. } => n = obj,
            _ => return false,
        }
    };
    let is_require = |mut n: NodeId| loop {
        match *ast.node(n) {
            Node::Call { callee, args, .. } => {
                return is_ident(callee, "require")
                    && ast.list(args).len() == 1
                    && matches!(ast.node(ast.list(args)[0]), Node::StringLit { .. });
            }
            Node::Member { obj, .. } => n = obj,
            _ => return false,
        }
    };
    match *ast.node(s) {
        Node::ExprStmt { expr } => match *ast.node(expr) {
            Node::Assign { left, .. } => exports_root(left),
            // `Object.defineProperty(exports, ..)` / `(module.exports, ..)`.
            Node::Call { callee, args, .. } => {
                is_require(expr)
                    || matches!(*ast.node(callee), Node::Member { obj, prop, .. }
                        if is_ident(obj, "Object") && is_ident(prop, "defineProperty"))
                        && ast.list(args).first().is_some_and(|&o| {
                            is_ident(o, "exports")
                                || matches!(*ast.node(o), Node::Member { obj, prop, .. }
                                    if is_ident(obj, "module") && is_ident(prop, "exports"))
                        })
            }
            _ => false,
        },
        Node::VarDecl { decls, .. } => ast.list(decls).iter().any(
            |&d| matches!(*ast.node(d), Node::VarDeclarator { init: Some(i), .. } if is_require(i)),
        ),
        _ => false,
    }
}

/// A small inline list of child ids, so `bind_any` does not allocate for typical nodes.
mod smallvec_like {
    use ts_r_ast::NodeId;

    pub struct Kids {
        inline: [Option<NodeId>; 8],
        len: usize,
        spill: Vec<NodeId>,
    }

    impl Kids {
        pub fn new() -> Kids {
            Kids {
                inline: [None; 8],
                len: 0,
                spill: Vec::new(),
            }
        }

        pub fn push(&mut self, id: NodeId) {
            if self.len < 8 {
                self.inline[self.len] = Some(id);
            } else {
                self.spill.push(id);
            }
            self.len += 1;
        }

        pub fn iter(&self) -> impl Iterator<Item = NodeId> + '_ {
            self.inline
                .iter()
                .take(self.len.min(8))
                .flatten()
                .copied()
                .chain(self.spill.iter().copied())
        }
    }
}

/// The value of the `resolution-mode` attribute in import attributes `a`, when it is the only
/// attribute and is `"import"` or `"require"` (with other keys it is ignored: TS1464).
fn resolution_mode_attribute(ast: &Ast, a: NodeId) -> Option<ts_r_resolver::ResolutionMode> {
    let Node::ImportAttributes { entries, .. } = *ast.node(a) else {
        return None;
    };
    let [only] = ast.list(entries) else {
        return None;
    };
    let Node::ImportAttribute { key, value } = *ast.node(*only) else {
        return None;
    };
    let (Node::Ident { name } | Node::StringLit { value: name }) = *ast.node(key) else {
        return None;
    };
    if ast.atom(name) != "resolution-mode" {
        return None;
    }
    match *ast.node(value) {
        Node::StringLit { value: v } => match ast.atom(v) {
            "import" => Some(ts_r_resolver::ResolutionMode::Import),
            "require" => Some(ts_r_resolver::ResolutionMode::Require),
            _ => None,
        },
        _ => None,
    }
}

/// A module name tsc treats as relative: `.`, `..`, starting with `./`, `../` (or with
/// backslashes), or a rooted path: `/`, `\`, a drive (`c:`, `c:/`), `^/`, or a URL with a
/// path (`file:///x`).
fn is_relative_module_name(name: &str) -> bool {
    let b = name.as_bytes();
    let sep = |c: u8| c == b'/' || c == b'\\';
    match b {
        [b'.'] | [b'.', b'.'] => true,
        [b'.', c, ..] | [b'.', b'.', c, ..] if sep(*c) => true,
        [c, ..] if sep(*c) => true,
        [d, b':'] if d.is_ascii_alphabetic() => true,
        [d, b':', c, ..] if d.is_ascii_alphabetic() && sep(*c) => true,
        [b'^', b'/', ..] => true,
        _ => name
            .find("://")
            .is_some_and(|i| name[i + 3..].contains('/')),
    }
}
