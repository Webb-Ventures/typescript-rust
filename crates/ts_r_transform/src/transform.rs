//! The main visitor: statements, expressions, functions, imports and exports.
//!
//! Nodes are rewritten **in place**: a node is replaced by copying the replacement into its
//! slot (`builder::replace`), so no parent pointers are needed. Statement lists are rebuilt
//! (`add_list`) when statements are removed or expanded. Synthesized nodes have empty spans.

use rustc_hash::{FxHashMap, FxHashSet};
use ts_r_ast::{
    Ast, Atom, CommentKind, FnFlags, Modifiers, Node, NodeId, NodeKind, NodeList, Span, VarKind,
};
use ts_r_diagnostics::{Category, Diagnostic, FileId};

use crate::builder::{Build, replace};
use crate::enums::EnumInfo;
use crate::helpers::{Helper, Helpers};
use crate::options::{JsxMode, ModuleFormat, TransformOptions};
use crate::scope::{BindingId, BindingKind, Scopes, analyze};
use crate::{TransformResult, is_type_kind};

/// The namespace whose body is being transformed.
#[derive(Clone, Copy, Debug)]
pub(crate) struct NsFrame {
    /// The scope of the namespace body (from scope analysis).
    pub scope: u32,
    /// The IIFE parameter name used inside the body.
    pub param: Atom,
}

pub(crate) struct Tx<'a> {
    pub ast: &'a mut Ast,
    pub src: &'a str,
    pub opts: &'a TransformOptions,
    pub sc: Scopes,
    pub erased: Vec<Span>,
    pub helpers: Helpers,
    pub diags: Vec<Diagnostic>,
    /// Statements inserted at the top of the module, after directives.
    pub top: Vec<NodeId>,
    /// Statements appended at the end of the module.
    pub bottom: Vec<NodeId>,
    /// Names generated so far.
    pub taken: FxHashSet<String>,
    pub ns_stack: Vec<NsFrame>,
    /// Evaluated enums of this file, by binding.
    pub enums: FxHashMap<BindingId, EnumInfo>,
    /// Whether a member receiver chain starts at an import binding.
    pub enum_import_roots: FxHashMap<NodeId, bool>,
    /// Enums already given a `var` (merged declarations).
    pub enum_declared: FxHashSet<BindingId>,
    /// Namespaces already given a `var`, by binding.
    pub ns_declared: FxHashSet<BindingId>,
    pub is_module: bool,
    /// Depth of statement lists below the program (0 = top level).
    pub list_depth: u32,
    pub cjs: crate::cjs::State,
    pub jsx: crate::jsx::State,
    /// `createRequire` alias for `import x = require()` in ES modules.
    pub require_alias: Option<Atom>,
    /// Enum member scope → the enum IIFE parameter.
    pub enum_params: FxHashMap<u32, Atom>,
    /// Hoisted temporaries (`var _a, _b;`) per function body; index 0 is the module.
    pub temps: Vec<Vec<Atom>>,
    /// Every hoisted temporary made by [`Tx::temp`]: below ES2022 the lowering pass renames
    /// them in print order with its own (tsc names all temporaries when printing).
    pub temp_atoms: Vec<Atom>,
    /// Unique names the `using` lowering made (`env_1`, `e_1`, `result_1`): below ES2022
    /// they are renamed with the lowering's own unique names, in print order.
    pub using_names: Vec<Atom>,
    /// `accessor` storages the TC39 decorator transform made: an ordinary private field
    /// in member order when private names are lowered (observed).
    pub decorator_storages: Vec<Atom>,
    /// Generated `accessor` storage names (`_a`, `_b`, ...) when private names are lowered:
    /// counted across the file, apart from temporaries (observed).
    pub storage_count: u32,
    /// Expressions that were instantiation expressions (`f<T>`): the lowering captures them
    /// in a temporary even when they are now an identifier (observed).
    pub instantiated: rustc_hash::FxHashSet<NodeId>,
    /// Synthesized static blocks that stand for a statement of the class's static
    /// initialization (`static { this.x = 1; }`, `static { _a = k; }`): below ES2022 they
    /// become that statement (or expression) after the class instead of an IIFE.
    pub inline_static: Vec<NodeId>,
    /// The expressions of `static { o.k, _a = k; }` blocks holding field keys (evaluated
    /// where the class is defined, like key captures).
    pub key_exprs: Vec<NodeId>,
    /// Class expressions standing for a class declaration (legacy decorators' `let C = class
    /// C {}`): below ES2022 their statics follow the statement.
    pub decl_like: Vec<NodeId>,
    /// The binding name an anonymous class expression is assigned to (`x = class {}`), for
    /// the TC39 decorator transform's `__setFunctionName`. Taken by the next class expression.
    pub class_name_hint: Option<Atom>,
    /// The expression being visited as the object of a non-optional member access, or the
    /// callee/tag of a call, `new` or tagged template: a parenthesized optional chain there
    /// keeps its parentheses (`(a?.b as T).c` → `(a?.b).c`), elsewhere it loses them.
    pub chain_target: Option<NodeId>,
    /// Cross-file facts (the checker in a program emit) for `file`.
    pub resolver: &'a dyn crate::EmitResolver,
    pub file: FileId,
    /// Constructs the resolver could not answer.
    pub unknown: Vec<(Span, &'static str)>,
    /// Import bindings the resolver said stand for no value (their specifier was dropped):
    /// `export default x` of one is erased too.
    pub type_alias_imports: FxHashSet<BindingId>,
    /// Layout hints for codegen ([`crate::TransformResult::single_line`]).
    pub single_line: Vec<NodeId>,
    pub multi_line: Vec<NodeId>,
    /// Source-map ranges of synthesized nodes ([`crate::TransformResult::map_ranges`]).
    pub map_ranges: Vec<(NodeId, Span)>,
    /// Statements standing for source nodes moved elsewhere
    /// ([`crate::TransformResult::moved_comments`]).
    pub moved_comments: Vec<(NodeId, Span)>,
}

impl Build for Tx<'_> {
    fn ast_mut(&mut self) -> &mut Ast {
        self.ast
    }
}

pub(crate) fn run(
    ast: &mut Ast,
    src: &str,
    opts: &TransformOptions,
    resolver: &dyn crate::EmitResolver,
    file: FileId,
) -> TransformResult {
    let sc = analyze(
        ast,
        opts.experimental_decorators && opts.emit_decorator_metadata,
    );
    let is_module = !ast.source_kind.is_declaration()
        && (opts.force_module
            || sc.has_module_syntax
            || (!opts.legacy_module_detection
                && sc.has_jsx
                && matches!(opts.jsx, JsxMode::Automatic | JsxMode::AutomaticDev)));
    let mut tx = Tx {
        ast,
        src,
        opts,
        sc,
        erased: Vec::new(),
        helpers: Helpers::default(),
        diags: Vec::new(),
        top: Vec::new(),
        bottom: Vec::new(),
        taken: FxHashSet::default(),
        ns_stack: Vec::new(),
        enums: FxHashMap::default(),
        enum_import_roots: FxHashMap::default(),
        enum_declared: FxHashSet::default(),
        ns_declared: FxHashSet::default(),
        is_module,
        list_depth: 0,
        cjs: crate::cjs::State::default(),
        jsx: crate::jsx::State::default(),
        require_alias: None,
        temps: vec![Vec::new()],
        temp_atoms: Vec::new(),
        using_names: Vec::new(),
        decorator_storages: Vec::new(),
        storage_count: 0,
        instantiated: rustc_hash::FxHashSet::default(),
        inline_static: Vec::new(),
        key_exprs: Vec::new(),
        decl_like: Vec::new(),
        enum_params: FxHashMap::default(),
        class_name_hint: None,
        chain_target: None,
        resolver,
        file,
        unknown: Vec::new(),
        type_alias_imports: FxHashSet::default(),
        single_line: Vec::new(),
        multi_line: Vec::new(),
        map_ranges: Vec::new(),
        moved_comments: Vec::new(),
    };
    let using = crate::downlevel::using_needing_lowering(&*tx.ast, opts.target);
    crate::enums::prepass(&mut tx);
    tx.program();
    let using = using.and_then(|_| {
        // Every `using` is lowered below ESNext; at ESNext only a top level followed by
        // exports.
        crate::downlevel::using::run(&mut tx, opts.target < ts_r_config::ScriptTarget::ESNext);
        crate::downlevel::using::remaining(&*tx.ast)
            .filter(|_| opts.target < ts_r_config::ScriptTarget::ESNext)
            .map(|s| (s, "'using' declaration"))
    });
    if opts.target < ts_r_config::ScriptTarget::ES2022 {
        crate::downlevel::lower::run(&mut tx);
    }
    crate::cjs::finish_helper_import(&mut tx);
    let unlowered: Vec<(Span, &'static str)> = using
        .or_else(|| crate::downlevel::unlowered(&*tx.ast, src, opts))
        .into_iter()
        .collect();
    let mut erased = std::mem::take(&mut tx.erased);
    erased.sort_by_key(|s| (s.start, std::cmp::Reverse(s.end)));
    let mut merged: Vec<Span> = Vec::with_capacity(erased.len());
    for s in erased {
        if s.start >= s.end {
            continue;
        }
        match merged.last_mut() {
            Some(last) if s.start <= last.end => last.end = last.end.max(s.end),
            _ => merged.push(s),
        }
    }
    let prelude = if tx.tslib_module() {
        String::new()
    } else {
        tx.helpers.render(opts)
    };
    TransformResult {
        erased: merged,
        prelude,
        diagnostics: tx.diags,
        unknown: {
            let mut u = tx.unknown;
            u.sort_by_key(|(s, _)| (s.start, s.end));
            u.dedup();
            u
        },
        unlowered,
        single_line: sorted(tx.single_line),
        multi_line: sorted(tx.multi_line),
        map_ranges: {
            // The last range given to a node wins.
            let mut r = tx.map_ranges;
            r.reverse();
            r.sort_by_key(|&(n, _)| n);
            r.dedup_by_key(|&mut (n, _)| n);
            r
        },
        moved_comments: {
            let mut r = tx.moved_comments;
            r.sort_by_key(|&(n, _)| n);
            r.dedup_by_key(|&mut (n, _)| n);
            r
        },
    }
}

fn sorted(mut v: Vec<NodeId>) -> Vec<NodeId> {
    v.sort_unstable();
    v.dedup();
    v
}

impl<'a> Tx<'a> {
    // ---- layout hints ----

    /// `export =` in a CommonJS module: the other exports are dropped.
    pub fn cjs_export_assignment(&self) -> bool {
        self.cjs.export_assignment()
    }

    /// Prints the synthesized block or embedded statement `id` on one line (tsc layout).
    pub fn mark_single_line(&mut self, id: NodeId) {
        self.single_line.push(id);
    }

    /// Prints the body block of the synthesized function `f` on one line (tsc layout).
    pub fn mark_single_line_body(&mut self, f: NodeId) {
        let body = match self.n(f) {
            Node::MethodDef { value, .. } => match self.n(value) {
                Node::FunctionExpr { body, .. } => body,
                _ => None,
            },
            Node::FunctionExpr { body, .. } | Node::FunctionDecl { body, .. } => body,
            Node::Arrow { body, .. } => Some(body),
            _ => None,
        };
        if let Some(b) = body
            && self.kind(b) == NodeKind::Block
        {
            self.single_line.push(b);
        }
    }

    /// Source maps (tsc layout): the synthesized node `id` maps to `range` (its start and
    /// end), as tsc maps the nodes it builds for a construct to the construct's source.
    pub fn map_range(&mut self, id: NodeId, range: Span) {
        self.map_ranges.push((id, range));
    }

    /// Source maps (tsc's): an expression left from erasing `x as T`, `<T>x`,
    /// `x satisfies T` or `x!` (now in `id`) starts where `x` does and ends where the erased
    /// expression did (`end`).
    fn erased_range(&mut self, id: NodeId, end: u32) {
        let inner = self.span(id);
        // A synthesized inner node (an empty span) has no position.
        if inner.start < inner.end && inner.end < end {
            self.map_range(id, Span::new(inner.start, end));
        }
    }

    /// Prints the synthesized array literal `id` one element per line, or the block `id`
    /// over several lines (tsc layout).
    pub fn mark_multi_line(&mut self, id: NodeId) {
        self.multi_line.push(id);
    }

    // ---- small helpers ----

    #[inline]
    pub fn n(&self, id: NodeId) -> Node {
        *self.ast.node(id)
    }

    #[inline]
    pub fn kind(&self, id: NodeId) -> NodeKind {
        self.ast.kind(id)
    }

    #[inline]
    pub fn span(&self, id: NodeId) -> Span {
        self.ast.span(id)
    }

    /// Source position for synthesized nodes derived from `id`.
    #[inline]
    pub fn at(&self, id: NodeId) -> u32 {
        self.ast.span(id).start
    }

    pub fn items(&self, l: NodeList) -> Vec<NodeId> {
        self.ast.list(l).to_vec()
    }

    pub fn set(&mut self, id: NodeId, n: Node) {
        *self.ast.node_mut(id) = n;
    }

    pub fn erase(&mut self, id: NodeId) {
        let s = self.ast.span(id);
        self.erased.push(s);
    }

    pub fn erase_opt(&mut self, id: Option<NodeId>) {
        if let Some(id) = id {
            self.erase(id);
        }
    }

    pub fn name(&self, id: NodeId) -> &str {
        match *self.ast.node(id) {
            Node::Ident { name } | Node::PrivateName { name } => self.ast.atom(name),
            Node::StringLit { value } => self.ast.atom(value),
            _ => "",
        }
    }

    pub fn atom_of(&self, id: NodeId) -> Option<Atom> {
        match *self.ast.node(id) {
            Node::Ident { name } => Some(name),
            _ => None,
        }
    }

    pub fn error(&mut self, code: u32, message: impl Into<String>, span: Span) {
        self.diags.push(Diagnostic {
            code,
            category: Category::Error,
            message: message.into(),
            file: FileId(0),
            span,
            related: Vec::new(),
        });
    }

    /// A name not used anywhere in the file (nor generated before): `base`, `base_1`, ...
    pub fn fresh(&mut self, base: &str, always_suffix: bool) -> Atom {
        let mut i = if always_suffix { 1 } else { 0 };
        loop {
            let cand = if i == 0 {
                base.to_owned()
            } else {
                format!("{base}_{i}")
            };
            let atom = self.ast.intern(&cand);
            if !self.sc.names.contains(&atom) && !self.taken.contains(&cand) {
                self.taken.insert(cand);
                return atom;
            }
            i += 1;
        }
    }

    /// A hoisted temporary (`_a`, `_b`, ...) declared with `var` at the top of the enclosing
    /// function body (or module).
    pub fn temp(&mut self) -> Atom {
        let a = self.temp_name();
        if let Some(t) = self.temps.last_mut() {
            t.push(a);
        }
        self.temp_atoms.push(a);
        a
    }

    /// A unique temporary name (`_a`, ..., `_z`, `_0`, ...) that is not declared.
    pub fn temp_name(&mut self) -> Atom {
        let mut i = 0usize;
        loop {
            let cand = if i < 26 {
                format!("_{}", (b'a' + i as u8) as char)
            } else {
                format!("_{}", i - 26)
            };
            i += 1;
            let atom = self.ast.intern(&cand);
            if !self.sc.names.contains(&atom) && !self.taken.contains(&cand) {
                self.taken.insert(cand);
                return atom;
            }
        }
    }

    /// `var _a, _b;` for the temporaries of the innermost body, if any.
    pub(crate) fn temps_decl(&mut self, temps: Vec<Atom>) -> Option<NodeId> {
        if temps.is_empty() {
            return None;
        }
        let mut decls = Vec::with_capacity(temps.len());
        for t in temps {
            let name = self.ident_atom(t, 0);
            let d = self.mk(
                Node::VarDeclarator {
                    name,
                    definite: false,
                    type_ann: None,
                    init: None,
                },
                0,
            );
            decls.push(d);
        }
        let decls = self.new_list(&decls);
        Some(self.mk(
            Node::VarDecl {
                modifiers: Modifiers::NONE,
                kind: VarKind::Var,
                decls,
            },
            0,
        ))
    }

    pub fn need(&mut self, h: Helper) {
        self.helpers.add(h);
    }

    /// Helper reference: `__decorate` (or `tslib.__decorate` with importHelpers).
    pub fn helper_ref(&mut self, h: Helper, at: u32) -> NodeId {
        self.need(h);
        if self.tslib_module() {
            // CommonJS with importHelpers: the import is inserted after lowering.
            let v = match self.cjs.tslib {
                Some(v) => v,
                None => {
                    let v = self.fresh("tslib", true);
                    self.cjs.tslib = Some(v);
                    v
                }
            };
            let o = self.ident_atom(v, at);
            return self.member(o, h.name(), at);
        }
        self.ident(h.name(), at)
    }

    /// Helpers come from a `tslib` module variable (CommonJS modules with importHelpers).
    pub fn tslib_module(&self) -> bool {
        self.opts.import_helpers && self.opts.is_cjs() && self.is_module
    }

    // ---- program ----

    /// An `.mts` file under `module: commonjs`: tsc emits it as an ES module but still adds
    /// `"use strict"`. Tools that force a format (bundler, dev server, ts-r-run) never get here.
    fn esm_under_cjs_option(&self) -> bool {
        self.opts.cjs_module_option
            && self.opts.module
                == ModuleFormat::Esm {
                    node_require: false,
                }
    }

    fn program(&mut self) {
        let root = self.ast.root;
        let Node::Program { stmts } = self.n(root) else {
            return;
        };
        let items = self.items(stmts);
        let prologue = items
            .iter()
            .take_while(|&&s| self.kind(s) == NodeKind::Directive)
            .count();
        let has_use_strict = items[..prologue].iter().any(|&s| {
            matches!(self.n(s), Node::Directive { value } if self.ast.atom(value) == "use strict")
        });
        if self.opts.is_cjs() {
            crate::cjs::prepare(self, &items[prologue..]);
        }
        // `module: preserve`: tsc 7 never adds the `export {};` marker, so a module whose
        // statements were all erased is an empty file (observed: `.ts`, `.mts`, `.cts`; a
        // source `export {}` kept by verbatimModuleSyntax stays). With
        // `preserve_module_marker` (the bundler) only files tsc emits CommonJS-style drop
        // it: `.cts`/`.cjs`, `import x = require()` or `export =` (even erased ones).
        let preserve = self.opts.module == ModuleFormat::Preserve
            && (!self.opts.preserve_module_marker
                || self.opts.cjs_extension
                || items[prologue..].iter().any(|&s| match self.n(s) {
                    Node::ExportAssignment { .. } => true,
                    Node::ImportEquals { module_ref, .. } => {
                        self.kind(module_ref) == NodeKind::ExternalModuleRef
                    }
                    _ => false,
                }));
        let mut out = Vec::with_capacity(items.len() + 4);
        for &s in &items[prologue..] {
            self.stmt(s, &mut out);
        }
        let mut jsx_imports = crate::jsx::finish(self);
        if self.opts.is_cjs() {
            out = crate::cjs::finish(self, out, std::mem::take(&mut jsx_imports));
        }
        // Module marker: an ES module whose import/export syntax was all erased.
        if self.is_module
            && !self.opts.is_cjs()
            && !preserve
            && !self.ast.source_kind.is_declaration()
            && !out
                .iter()
                .chain(self.top.iter())
                .chain(jsx_imports.iter())
                .any(|&s| self.is_module_stmt(s))
            && !self.bottom.iter().any(|&s| self.is_module_stmt(s))
        {
            let empty = self.mk(
                Node::ExportNamed {
                    type_only: false,
                    specifiers: NodeList::EMPTY,
                    source: None,
                    attributes: None,
                },
                0,
            );
            self.bottom.push(empty);
        }
        let mut all: Vec<NodeId> = Vec::with_capacity(out.len() + 8);
        let needs_strict = self.opts.always_strict
            && !has_use_strict
            && (self.opts.is_cjs() || self.esm_under_cjs_option() || !self.is_module);
        if needs_strict {
            let v = self.ast.intern("use strict");
            let d = self.mk(Node::Directive { value: v }, 0);
            all.push(d);
        }
        all.extend_from_slice(&items[..prologue]);
        let temps = std::mem::take(&mut self.temps[0]);
        if let Some(t) = self.temps_decl(temps) {
            all.push(t);
        }
        all.extend(jsx_imports);
        let top = std::mem::take(&mut self.top);
        all.extend(top);
        all.extend(out);
        let bottom = std::mem::take(&mut self.bottom);
        all.extend(bottom);
        let list = self.new_list(&all);
        self.set(root, Node::Program { stmts: list });
    }

    fn is_module_stmt(&self, s: NodeId) -> bool {
        match self.n(s) {
            Node::ImportDecl { .. }
            | Node::ExportNamed { .. }
            | Node::ExportAll { .. }
            | Node::ExportDefaultExpr { .. } => true,
            Node::VarDecl { modifiers, .. }
            | Node::FunctionDecl { modifiers, .. }
            | Node::ClassDecl { modifiers, .. } => modifiers.contains(Modifiers::EXPORT),
            _ => false,
        }
    }

    // ---- statements ----

    /// Transforms a statement list (block, function body, namespace body, switch case).
    pub fn stmt_list(&mut self, list: NodeList) -> NodeList {
        let items = self.items(list);
        let mut out = Vec::with_capacity(items.len());
        self.list_depth += 1;
        for s in items.iter().copied() {
            self.stmt(s, &mut out);
        }
        self.list_depth -= 1;
        if out == items {
            list
        } else {
            self.new_list(&out)
        }
    }

    /// A statement in a single-statement position (`if` branch, loop body).
    fn sub_stmt(&mut self, id: NodeId) {
        let mut out = Vec::new();
        self.list_depth += 1;
        self.stmt(id, &mut out);
        self.list_depth -= 1;
        match out.len() {
            1 if out[0] == id => {}
            1 => replace(self.ast, id, out[0]),
            0 => {
                let at = self.at(id);
                let e = self.mk(Node::Empty {}, at);
                replace(self.ast, id, e);
            }
            _ => {
                let at = self.at(id);
                let b = self.block(&out, at);
                replace(self.ast, id, b);
            }
        }
    }

    pub fn stmt(&mut self, id: NodeId, out: &mut Vec<NodeId>) {
        let n = self.n(id);
        let mods = n.modifiers();
        if mods.contains(Modifiers::DECLARE) && !matches!(n, Node::PropertyDef { .. }) {
            self.erase(id);
            return;
        }
        match n {
            Node::Interface { .. } | Node::TypeAlias { .. } | Node::NamespaceExport { .. } => {
                self.erase(id);
            }
            Node::Enum { .. } | Node::Module { .. } | Node::ImportEquals { .. }
                if self.opts.is_cjs() && self.list_depth == 0 =>
            {
                let names = crate::cjs::declared_names(self, id);
                match n {
                    Node::Enum { .. } => crate::enums::enum_decl(self, id, out),
                    Node::Module { .. } => crate::namespace::module_decl(self, id, out),
                    _ => self.import_equals(id, out),
                }
                crate::cjs::after_decl(self, &names, out);
            }
            Node::Enum { .. } => crate::enums::enum_decl(self, id, out),
            Node::Module { .. } => crate::namespace::module_decl(self, id, out),
            Node::ImportDecl { .. } => self.import_decl(id, out),
            Node::ImportEquals { .. } => self.import_equals(id, out),
            Node::ExportNamed { .. } => self.export_named(id, out),
            Node::ExportAll {
                type_only, source, ..
            } => {
                if type_only {
                    self.erase(id);
                } else if self.opts.is_cjs() {
                    crate::cjs::export_all(self, id, out);
                } else {
                    self.rewrite_specifier(source);
                    out.push(id);
                }
            }
            Node::ExportDefaultExpr { expr } => {
                // `export default Iface` where Iface is a type: erased. So is an imported
                // name that stands for no value (`import I from "./i"` of an interface).
                if let Node::Ident { name } = self.n(expr) {
                    let erase = match self.sc.binding_of_ref(expr) {
                        None => self.sc.type_names.contains(&name),
                        Some(b) => self.type_alias_imports.contains(&b),
                    };
                    if erase {
                        self.erase(id);
                        return;
                    }
                }
                self.expr(expr);
                if self.opts.is_cjs() {
                    crate::cjs::export_default_expr(self, id, out);
                } else {
                    out.push(id);
                }
            }
            Node::ExportAssignment { expr } => {
                self.expr(expr);
                match self.opts.module {
                    ModuleFormat::CommonJs | ModuleFormat::Preserve => {
                        let at = self.at(id);
                        let m = self.dotted("module.exports", at);
                        let a = self.assign(m, expr, at);
                        let s = self.expr_stmt(a, at);
                        // Source maps (tsc's): the statement maps to `export = x;`.
                        let span = self.span(id);
                        self.map_range(s, span);
                        if self.opts.is_cjs() && self.ns_stack.is_empty() {
                            // tsc writes `module.exports = x;` last, without the comments
                            // on the `export =` line (observed).
                            self.erase(id);
                            self.cjs.export_assignments.push(s);
                        } else {
                            out.push(s);
                        }
                    }
                    ModuleFormat::Esm { .. } => {
                        self.error(
                            1203,
                            "Export assignment cannot be used when targeting ECMAScript modules.",
                            self.span(id),
                        );
                        self.erase(id);
                    }
                }
            }
            Node::FunctionDecl { body: None, .. } => {
                // Overload signature.
                self.erase(id);
            }
            Node::FunctionDecl {
                modifiers, name, ..
            } => {
                self.function(id);
                if !self.ns_stack.is_empty() && modifiers.contains(Modifiers::EXPORT) {
                    crate::namespace::exported_decl(self, id, name, out);
                } else if self.opts.is_cjs() && self.list_depth == 0 {
                    crate::cjs::exported_function(self, id, out);
                } else {
                    out.push(id);
                }
            }
            Node::ClassDecl { .. } => crate::class::class_decl(self, id, out),
            Node::VarDecl { modifiers, .. } => {
                self.var_decl(id);
                if !self.ns_stack.is_empty() && modifiers.contains(Modifiers::EXPORT) {
                    crate::namespace::exported_var(self, id, out);
                } else if self.opts.is_cjs() && self.list_depth == 0 {
                    crate::cjs::exported_var(self, id, out);
                } else {
                    out.push(id);
                }
            }
            Node::Block { stmts } => {
                let l = self.stmt_list(stmts);
                self.set(id, Node::Block { stmts: l });
                out.push(id);
            }
            Node::ExprStmt { expr } => {
                let mut e = expr;
                while let Node::Paren { expr } = self.n(e) {
                    e = expr;
                }
                self.cjs.stmt_expr = Some(e);
                self.expr(expr);
                out.push(id);
            }
            Node::If { test, cons, alt } => {
                self.expr(test);
                self.sub_stmt(cons);
                if let Some(a) = alt {
                    self.sub_stmt(a);
                }
                out.push(id);
            }
            Node::For {
                init,
                test,
                update,
                body,
            } => {
                if let Some(i) = init {
                    self.for_init(i);
                }
                if let Some(t) = test {
                    self.expr(t);
                }
                if let Some(u) = update {
                    self.expr(u);
                }
                self.sub_stmt(body);
                out.push(id);
            }
            Node::ForIn { left, right, body }
            | Node::ForOf {
                left, right, body, ..
            } => {
                self.for_init(left);
                self.expr(right);
                self.sub_stmt(body);
                out.push(id);
            }
            Node::While { test, body } | Node::DoWhile { body, test } => {
                self.expr(test);
                self.sub_stmt(body);
                out.push(id);
            }
            Node::Return { arg } => {
                if let Some(a) = arg {
                    self.expr(a);
                }
                out.push(id);
            }
            Node::Throw { arg } => {
                self.expr(arg);
                out.push(id);
            }
            Node::Labeled { body, .. } => {
                self.sub_stmt(body);
                out.push(id);
            }
            Node::Switch { disc, cases } => {
                self.expr(disc);
                for c in self.items(cases) {
                    if let Node::SwitchCase { test, body } = self.n(c) {
                        if let Some(t) = test {
                            self.expr(t);
                        }
                        let b = self.stmt_list(body);
                        self.set(c, Node::SwitchCase { test, body: b });
                    }
                }
                out.push(id);
            }
            Node::Try {
                block,
                handler,
                finalizer,
            } => {
                self.sub_stmt(block);
                if let Some(h) = handler
                    && let Node::CatchClause {
                        param,
                        type_ann,
                        body,
                    } = self.n(h)
                {
                    self.erase_opt(type_ann);
                    if let Some(p) = param {
                        self.pattern(p);
                    }
                    self.sub_stmt(body);
                    self.set(
                        h,
                        Node::CatchClause {
                            param,
                            type_ann: None,
                            body,
                        },
                    );
                }
                if let Some(f) = finalizer {
                    self.sub_stmt(f);
                }
                out.push(id);
            }
            Node::With { obj, body } => {
                self.expr(obj);
                self.sub_stmt(body);
                out.push(id);
            }
            _ => out.push(id),
        }
    }

    fn for_init(&mut self, id: NodeId) {
        if self.kind(id) == NodeKind::VarDecl {
            self.var_decl(id);
        } else {
            self.expr(id);
        }
    }

    /// Strips types from a variable declaration and transforms initializers.
    /// `x` for `x = class {}` (named evaluation of an anonymous class).
    fn anonymous_class_name(&self, target: NodeId, value: NodeId) -> Option<Atom> {
        match (self.n(target), self.n(value)) {
            (Node::Ident { name }, Node::ClassExpr { name: None, .. }) => Some(name),
            _ => None,
        }
    }

    pub fn var_decl(&mut self, id: NodeId) {
        let Node::VarDecl { decls, .. } = self.n(id) else {
            return;
        };
        for d in self.items(decls) {
            if let Node::VarDeclarator {
                name,
                type_ann,
                init,
                ..
            } = self.n(d)
            {
                self.erase_opt(type_ann);
                self.pattern(name);
                if let Some(i) = init {
                    self.class_name_hint = self.anonymous_class_name(name, i);
                    self.expr(i);
                    self.class_name_hint = None;
                }
                self.set(
                    d,
                    Node::VarDeclarator {
                        name,
                        definite: false,
                        type_ann: None,
                        init,
                    },
                );
            }
        }
    }

    /// Binding patterns: defaults and computed keys are expressions.
    pub fn pattern(&mut self, id: NodeId) {
        match self.n(id) {
            Node::ObjectPattern { props } => {
                for p in self.items(props) {
                    self.pattern(p);
                }
            }
            Node::ArrayPattern { elements } => {
                for e in self.items(elements) {
                    self.pattern(e);
                }
            }
            Node::BindingProperty { key, value, init } => {
                if let Node::ComputedKey { expr } = self.n(key) {
                    self.expr(expr);
                }
                if let Some(v) = value {
                    self.pattern(v);
                }
                if let Some(i) = init {
                    self.expr(i);
                }
            }
            Node::AssignPattern { left, right } => {
                self.pattern(left);
                self.expr(right);
            }
            Node::Rest { arg } => self.pattern(arg),
            _ => {}
        }
    }

    // ---- functions ----

    /// Strips a function's types and `this` parameter and transforms its body.
    /// Works for `FunctionDecl`, `FunctionExpr` and `Arrow`.
    pub fn function(&mut self, id: NodeId) {
        match self.n(id) {
            Node::FunctionDecl {
                modifiers,
                flags,
                name,
                type_params,
                params,
                return_type,
                body,
            } => {
                self.erase_opt(type_params);
                self.erase_opt(return_type);
                self.temps.push(Vec::new());
                let params = self.params(params);
                let pending = self.temps.pop().unwrap_or_default();
                if let Some(b) = body {
                    self.fn_body_with(b, pending);
                } else if let Some(t) = self.temps.last_mut() {
                    t.extend(pending);
                }
                let mut m = modifiers;
                m.remove(Modifiers::ASYNC);
                self.set(
                    id,
                    Node::FunctionDecl {
                        modifiers: m,
                        flags,
                        name,
                        type_params: None,
                        params,
                        return_type: None,
                        body,
                    },
                );
            }
            Node::FunctionExpr {
                modifiers,
                flags,
                name,
                type_params,
                params,
                return_type,
                body,
            } => {
                self.erase_opt(type_params);
                self.erase_opt(return_type);
                self.temps.push(Vec::new());
                let params = self.params(params);
                let pending = self.temps.pop().unwrap_or_default();
                if let Some(b) = body {
                    self.fn_body_with(b, pending);
                } else if let Some(t) = self.temps.last_mut() {
                    t.extend(pending);
                }
                let _ = modifiers;
                self.set(
                    id,
                    Node::FunctionExpr {
                        modifiers: Modifiers::NONE,
                        flags,
                        name,
                        type_params: None,
                        params,
                        return_type: None,
                        body,
                    },
                );
            }
            Node::Arrow {
                flags,
                type_params,
                params,
                return_type,
                body,
            } => {
                self.erase_opt(type_params);
                self.erase_opt(return_type);
                let params = self.params(params);
                if flags.contains(FnFlags::EXPRESSION_BODY) {
                    self.expr(body);
                } else {
                    self.fn_body(body);
                }
                self.set(
                    id,
                    Node::Arrow {
                        flags,
                        type_params: None,
                        params,
                        return_type: None,
                        body,
                    },
                );
            }
            _ => {}
        }
    }

    pub fn fn_body(&mut self, body: NodeId) {
        self.fn_body_with(body, Vec::new());
    }

    /// [`Tx::fn_body`] with temporaries the parameters already asked for (a parameter
    /// initializer's temporaries are declared in the function, observed).
    pub fn fn_body_with(&mut self, body: NodeId, pending: Vec<Atom>) {
        if let Node::Block { stmts } = self.n(body) {
            let saved_depth = self.list_depth;
            self.list_depth = self.list_depth.max(1);
            self.temps.push(pending);
            let mut l = self.stmt_list(stmts);
            let temps = self.temps.pop().unwrap_or_default();
            if let Some(t) = self.temps_decl(temps) {
                let mut items = self.items(l);
                let pro = items
                    .iter()
                    .take_while(|&&s| self.kind(s) == NodeKind::Directive)
                    .count();
                items.insert(pro, t);
                l = self.new_list(&items);
            }
            self.list_depth = saved_depth;
            self.set(body, Node::Block { stmts: l });
        } else {
            if let Some(t) = self.temps.last_mut() {
                t.extend(pending);
            }
            self.expr(body);
        }
    }

    /// Strips parameter types/modifiers, drops `this` parameters. Returns the new list.
    pub fn params(&mut self, params: NodeList) -> NodeList {
        let items = self.items(params);
        let mut out = Vec::with_capacity(items.len());
        for p in items.iter().copied() {
            if let Node::Param {
                decorators,
                rest,
                pattern,
                type_ann,
                init,
                ..
            } = self.n(p)
            {
                if matches!(self.n(pattern), Node::Ident { name } if self.ast.atom(name) == "this")
                {
                    self.erase(p);
                    continue;
                }
                self.erase_opt(type_ann);
                self.pattern(pattern);
                if let Some(i) = init {
                    self.expr(i);
                }
                self.set(
                    p,
                    Node::Param {
                        decorators,
                        modifiers: Modifiers::NONE,
                        rest,
                        pattern,
                        optional: false,
                        type_ann: None,
                        init,
                    },
                );
            }
            out.push(p);
        }
        if out == items {
            params
        } else {
            self.new_list(&out)
        }
    }

    // ---- expressions ----

    pub fn expr(&mut self, id: NodeId) {
        let n = self.n(id);
        match n {
            Node::Ident { .. } => self.ident_ref(id),
            Node::As { expr, ty }
            | Node::Satisfies { expr, ty }
            | Node::TypeAssertion { expr, ty } => {
                let end = self.span(id).end;
                self.erase(ty);
                self.expr(expr);
                replace(self.ast, id, expr);
                self.erased_range(id, end);
            }
            Node::NonNull { expr } => {
                let end = self.span(id).end;
                self.expr(expr);
                replace(self.ast, id, expr);
                self.erased_range(id, end);
            }
            Node::Instantiation { expr, type_args } => {
                // `a?.b<T>.c` ends the chain: `(a?.b).c` (observed).
                let continued = self.chain_target == Some(id);
                self.erase(type_args);
                self.expr(expr);
                if continued && self.is_optional_chain(expr) {
                    self.set(id, Node::Paren { expr });
                } else {
                    replace(self.ast, id, expr);
                    self.instantiated.insert(id);
                }
            }
            Node::Paren { expr } => {
                let inner = self.kind(expr);
                let was_type = matches!(
                    inner,
                    NodeKind::As
                        | NodeKind::Satisfies
                        | NodeKind::TypeAssertion
                        | NodeKind::NonNull
                );
                // tsc keeps `(a?.b!)` everywhere, and `(a?.b as T)` only where the chain
                // would otherwise continue (observed).
                let keep_chain = inner == NodeKind::NonNull || self.chain_target == Some(id);
                // Comments with line breaks after the `(` need parentheses to keep
                // `return (\n// c\nx)` one statement; inline comments move out.
                let (open, start) = (self.span(id).start, self.span(expr).start);
                let has_comments = self.ast.comments_between(open, start).iter().any(|c| {
                    c.kind == CommentKind::Line
                        || c.newline_before
                        || self
                            .src
                            .get(c.span.start as usize..c.span.end as usize)
                            .is_some_and(|s| s.contains(['\r', '\n', '\u{2028}', '\u{2029}']))
                });
                let paren_end = self.span(id).end;
                self.expr(expr);
                if was_type && !has_comments && !(keep_chain && self.is_optional_chain(expr)) {
                    replace(self.ast, id, expr);
                    // Source maps (tsc's): it ends where the parentheses did.
                    let start = self.span(id).start;
                    self.map_range(id, Span::new(start, paren_end));
                }
            }
            Node::Call {
                callee,
                optional,
                type_args,
                args,
            } => {
                self.erase_opt(type_args);
                self.chain_target = (!optional).then_some(callee);
                self.expr(callee);
                for a in self.items(args) {
                    self.expr(a);
                }
                self.rewrite_require(callee, args);
                self.set(
                    id,
                    Node::Call {
                        callee,
                        optional,
                        type_args: None,
                        args,
                    },
                );
                self.fix_detached_callee(callee);
            }
            Node::New {
                callee,
                type_args,
                has_args,
                args,
            } => {
                self.erase_opt(type_args);
                self.chain_target = Some(callee);
                self.expr(callee);
                for a in self.items(args) {
                    self.expr(a);
                }
                self.set(
                    id,
                    Node::New {
                        callee,
                        type_args: None,
                        has_args,
                        args,
                    },
                );
            }
            Node::TaggedTemplate {
                tag,
                type_args,
                quasi,
            } => {
                self.erase_opt(type_args);
                self.chain_target = Some(tag);
                self.expr(tag);
                self.expr(quasi);
                self.set(
                    id,
                    Node::TaggedTemplate {
                        tag,
                        type_args: None,
                        quasi,
                    },
                );
                self.fix_detached_callee(tag);
            }
            Node::Member { obj, optional, .. } => {
                if crate::enums::inline_const_enum_access(self, id) {
                    return;
                }
                self.chain_target = (!optional).then_some(obj);
                self.expr(obj);
            }
            Node::Index {
                obj,
                index,
                optional,
            } => {
                if crate::enums::inline_const_enum_access(self, id) {
                    return;
                }
                self.chain_target = (!optional).then_some(obj);
                self.expr(obj);
                self.expr(index);
            }
            Node::FunctionExpr { .. } | Node::Arrow { .. } => self.function(id),
            Node::ClassExpr { .. } => crate::class::class_expr(self, id),
            Node::ObjectLit { props } => {
                for p in self.items(props) {
                    self.object_member(p);
                }
            }
            Node::Assign { left, right, .. } => {
                let live = crate::cjs::live_exports_of(self, left);
                let hint = self.anonymous_class_name(left, right);
                self.assign_target(left);
                self.class_name_hint = hint;
                self.expr(right);
                self.class_name_hint = None;
                if !live.is_empty() {
                    crate::cjs::update_live_exports(self, id, &live);
                }
            }
            Node::Update { arg, .. } => {
                let live = crate::cjs::live_exports_of(self, arg);
                self.expr(arg);
                if !live.is_empty() {
                    crate::cjs::update_live_exports(self, id, &live);
                }
            }
            Node::ImportCall { arg, options, .. } => {
                self.expr(arg);
                if let Some(o) = options {
                    self.expr(o);
                }
                self.rewrite_dynamic_specifier(arg);
                if self.opts.is_cjs() && !self.opts.node_module {
                    crate::cjs::import_call(self, id);
                }
            }
            Node::JsxElement { .. } | Node::JsxFragment { .. } => crate::jsx::jsx(self, id),
            Node::Hole {} | Node::This {} | Node::Super {} => {}
            _ => {
                if is_type_kind(n.kind()) {
                    return;
                }
                let mut children = Vec::new();
                self.ast.for_each_child(id, |c| children.push(c));
                for c in children {
                    self.expr(c);
                }
            }
        }
    }

    /// `x?.y`, `x?.()`, ... (an optional chain whose parentheses are significant).
    fn is_optional_chain(&self, mut id: NodeId) -> bool {
        loop {
            match self.n(id) {
                Node::Member { obj, optional, .. } | Node::Index { obj, optional, .. } => {
                    if optional {
                        return true;
                    }
                    id = obj;
                }
                Node::Call {
                    callee, optional, ..
                } => {
                    if optional {
                        return true;
                    }
                    id = callee;
                }
                Node::NonNull { expr } => id = expr,
                _ => return false,
            }
        }
    }

    /// After rewriting `f` into `mod.f` (CommonJS imports), a call `f()` must not pass `mod`
    /// as `this`: print `(0, mod.f)()`.
    fn fix_detached_callee(&mut self, callee: NodeId) {
        if self.cjs.detached.remove(&callee) {
            let at = self.at(callee);
            let moved = crate::builder::clone_node(self.ast, callee);
            // Source maps (tsc's): `mod.f` maps to `f`; `(0, mod.f)` has no range of its own.
            if let Some(i) = self.map_ranges.iter().rposition(|&(n, _)| n == callee) {
                self.map_ranges[i].0 = moved;
            }
            let zero = self.number(0.0, at);
            let seq = self.seq(&[zero, moved], at);
            let p = self.paren(seq, at);
            replace(self.ast, callee, p);
        }
    }

    fn object_member(&mut self, p: NodeId) {
        match self.n(p) {
            Node::Property { key, value, .. } => {
                if let Node::ComputedKey { expr } = self.n(key) {
                    self.expr(expr);
                }
                self.expr(value);
            }
            Node::ShorthandProp {
                modifiers,
                name,
                init,
            } => {
                // `{ x }` where `x` is rewritten (namespace export, CJS import): expand.
                let at = self.at(p);
                let key = crate::builder::clone_node(self.ast, name);
                self.ident_ref(name);
                if let Some(i) = init {
                    self.expr(i);
                }
                if self.kind(name) != NodeKind::Ident {
                    self.cjs.detached.remove(&name);
                    let value = match init {
                        Some(i) => self.assign(name, i, at),
                        None => name,
                    };
                    self.set(
                        p,
                        Node::Property {
                            modifiers,
                            key,
                            value,
                        },
                    );
                }
            }
            Node::MethodDef {
                key, value, kind, ..
            } => {
                if let Node::ComputedKey { expr } = self.n(key) {
                    self.expr(expr);
                }
                self.accessor_body(kind, value);
                self.function(value);
            }
            Node::Spread { arg } => self.expr(arg),
            _ => self.expr(p),
        }
    }

    /// An accessor written without a body (an error) gets an empty one: `get x() { }`.
    pub(crate) fn accessor_body(&mut self, kind: ts_r_ast::MethodKind, value: NodeId) {
        if !matches!(kind, ts_r_ast::MethodKind::Get | ts_r_ast::MethodKind::Set) {
            return;
        }
        if let Node::FunctionExpr {
            modifiers,
            flags,
            name,
            type_params,
            params,
            return_type,
            body: None,
        } = self.n(value)
        {
            let at = self.at(value);
            let block = self.mk(
                Node::Block {
                    stmts: ts_r_ast::NodeList::EMPTY,
                },
                at,
            );
            self.mark_single_line(block);
            self.set(
                value,
                Node::FunctionExpr {
                    modifiers,
                    flags,
                    name,
                    type_params,
                    params,
                    return_type,
                    body: Some(block),
                },
            );
        }
    }

    /// Assignment targets: `ObjectLit`/`ArrayLit` patterns or simple targets.
    fn assign_target(&mut self, id: NodeId) {
        match self.n(id) {
            Node::ObjectLit { props } => {
                for p in self.items(props) {
                    self.object_member(p);
                }
            }
            Node::ArrayLit { elements } => {
                for e in self.items(elements) {
                    self.assign_target(e);
                }
            }
            Node::Spread { arg } => self.assign_target(arg),
            Node::Assign { left, right, .. } => {
                self.assign_target(left);
                self.expr(right);
            }
            _ => self.expr(id),
        }
    }

    /// Rewrites a value reference: namespace exports (`N.x`), CommonJS imports and exports.
    pub fn ident_ref(&mut self, id: NodeId) {
        let orig = self.span(id);
        self.ident_ref_inner(id);
        // A reference rewritten to `N.x` / `exports.x` maps to the identifier it replaces.
        let now = self.ast.span(id);
        if now.start >= now.end && orig.start < orig.end {
            self.map_range(id, orig);
        }
    }

    fn ident_ref_inner(&mut self, id: NodeId) {
        let Some(b) = self.sc.binding_of_ref(id) else {
            return;
        };
        let binding = self.sc.binding(b).clone();
        if binding.kind == BindingKind::EnumMember {
            if let Some(&p) = self.enum_params.get(&binding.scope) {
                let at = self.at(id);
                let obj = self.ident_atom(p, at);
                let name = self.ast.atom(binding.name).to_owned();
                let m = self.member_or_index(obj, &name, at);
                replace(self.ast, id, m);
            }
            return;
        }
        // Namespace exports.
        if let crate::scope::ScopeKind::Namespace(_) = self.sc.scopes[binding.scope as usize].kind {
            let rewrite = match binding.kind {
                BindingKind::MergedNsExport => true,
                BindingKind::Var | BindingKind::Let | BindingKind::Const => binding.exported,
                _ => false,
            };
            if rewrite && let Some(param) = crate::namespace::param_for_scope(self, binding.scope) {
                let at = self.at(id);
                let obj = self.ident_atom(param, at);
                let name = self.ast.atom(binding.name).to_owned();
                let m = self.member(obj, &name, at);
                replace(self.ast, id, m);
            }
            return;
        }
        if self.opts.is_cjs() && binding.scope == 0 {
            crate::cjs::rewrite_ref(self, id, b);
        }
    }

    // ---- imports and exports (ES module output) ----

    fn binding_used(&self, local: NodeId) -> bool {
        match self.sc.decl_of.get(&local) {
            Some(&b) => self.sc.binding(b).refs > 0,
            None => true,
        }
    }

    fn import_decl(&mut self, id: NodeId, out: &mut Vec<NodeId>) {
        let Node::ImportDecl {
            type_only,
            phase,
            has_clause,
            specifiers,
            source,
            attributes,
        } = self.n(id)
        else {
            return;
        };
        if type_only {
            self.erase(id);
            return;
        }
        if self.sc.has_jsx {
            crate::jsx::read_pragmas(self);
        }
        // JavaScript files have no types: tsc keeps their imports as written (observed:
        // unused imports and `import {} from` stay).
        let verbatim = self.opts.verbatim_module_syntax || !self.ast.source_kind.is_typescript();
        let specs = self.items(specifiers);
        let mut kept = Vec::with_capacity(specs.len());
        for &s in &specs {
            let keep = match self.n(s) {
                Node::ImportSpecifier {
                    type_only: true, ..
                } => false,
                Node::ImportSpecifier { local, .. }
                | Node::ImportDefaultSpecifier { local }
                | Node::ImportNamespaceSpecifier { local } => {
                    verbatim
                        || self.jsx_needs(local)
                        || (self.binding_used(local) && {
                            let value = self.value_alias(s);
                            if !value && let Some(&b) = self.sc.decl_of.get(&local) {
                                self.type_alias_imports.insert(b);
                            }
                            value
                        })
                }
                _ => true,
            };
            if keep {
                kept.push(s);
            } else {
                self.erase(s);
            }
        }
        if has_clause && kept.is_empty() && !(verbatim || specs.is_empty()) {
            self.erase(id);
            return;
        }
        if has_clause && kept.is_empty() && specs.is_empty() && !verbatim {
            // `import {} from "x"` without verbatimModuleSyntax is elided by tsc.
            self.erase(id);
            return;
        }
        if self.opts.is_cjs() {
            crate::cjs::import_decl(self, id, &kept, out);
            return;
        }
        let specifiers = if kept.len() == specs.len() {
            specifiers
        } else {
            self.new_list(&kept)
        };
        self.rewrite_specifier(source);
        self.set(
            id,
            Node::ImportDecl {
                type_only: false,
                phase,
                has_clause,
                specifiers,
                source,
                attributes,
            },
        );
        out.push(id);
    }

    /// Whether the alias declared by `decl` (an import or export specifier) stands for a
    /// value, as far as the [`crate::EmitResolver`] knows. Synthesized nodes are not asked.
    /// An `Unknown` answer is recorded (the emitter skips the file) and keeps the alias.
    pub(crate) fn value_alias(&mut self, decl: NodeId) -> bool {
        let span = self.span(decl);
        if span.start >= span.end {
            return true;
        }
        match self.resolver.is_value_alias(self.file, decl) {
            crate::Answer::Yes => true,
            crate::Answer::No => false,
            crate::Answer::Unknown => {
                self.unknown.push((span, "import or export alias"));
                true
            }
        }
    }

    /// The classic JSX factory's root identifier keeps its import alive.
    fn jsx_needs(&self, local: NodeId) -> bool {
        if !self.sc.has_jsx {
            return false;
        }
        let root = self.jsx.factory_root(self.opts);
        self.name(local) == root || self.name(local) == self.jsx.fragment_root(self.opts)
    }

    fn import_equals(&mut self, id: NodeId, out: &mut Vec<NodeId>) {
        let Node::ImportEquals {
            modifiers,
            type_only,
            name,
            module_ref,
        } = self.n(id)
        else {
            return;
        };
        let exported = modifiers.contains(Modifiers::EXPORT);
        let kept_by_verbatim = (self.opts.verbatim_module_syntax
            || !self.ast.source_kind.is_typescript())
            && self.kind(module_ref) == NodeKind::ExternalModuleRef;
        if type_only || !(exported || self.binding_used(name) || kept_by_verbatim) {
            self.erase(id);
            return;
        }
        let at = self.at(id);
        match self.n(module_ref) {
            Node::ExternalModuleRef { expr } => {
                self.rewrite_specifier(expr);
                let require = match self.opts.module {
                    ModuleFormat::CommonJs | ModuleFormat::Preserve => self.ident("require", at),
                    ModuleFormat::Esm { node_require: true } => {
                        let alias = crate::cjs::create_require(self);
                        self.ident_atom(alias, at)
                    }
                    ModuleFormat::Esm {
                        node_require: false,
                    } => {
                        self.error(
                            1202,
                            "Import assignment cannot be used when targeting ECMAScript modules.",
                            self.span(id),
                        );
                        self.erase(id);
                        return;
                    }
                };
                let expr = crate::cjs::double_quoted(self, expr);
                let call = self.call(require, &[expr], at);
                if self.opts.is_cjs() && exported {
                    let target = self.dotted("exports", at);
                    let nm = self.name(name).to_owned();
                    let m = self.member(target, &nm, at);
                    let a = self.assign(m, call, at);
                    let s = self.expr_stmt(a, at);
                    out.push(s);
                    return;
                }
                let v = self.var(VarKind::Const, Modifiers::NONE, name, Some(call), at);
                // Source maps (tsc's): the declaration maps to the `import x = ...`.
                let span = self.span(id);
                self.map_range(v, span);
                out.push(v);
                if exported {
                    // `export { x };`
                    let local = crate::builder::clone_node(self.ast, name);
                    let spec = self.mk(
                        Node::ExportSpecifier {
                            type_only: false,
                            local,
                            exported: None,
                        },
                        at,
                    );
                    let specifiers = self.new_list(&[spec]);
                    let e = self.mk(
                        Node::ExportNamed {
                            type_only: false,
                            specifiers,
                            source: None,
                            attributes: None,
                        },
                        at,
                    );
                    out.push(e);
                }
            }
            _ => {
                // `import x = A.B` → `var x = A.B`.
                let value = self.entity_to_expr(module_ref);
                self.expr(value);
                if !self.ns_stack.is_empty() && exported {
                    let param = self.ns_stack.last().map_or(Atom::EMPTY, |f| f.param);
                    let obj = self.ident_atom(param, at);
                    let nm = self.name(name).to_owned();
                    let m = self.member(obj, &nm, at);
                    let a = self.assign(m, value, at);
                    let s = self.expr_stmt(a, at);
                    out.push(s);
                    return;
                }
                let mods = if exported {
                    Modifiers::EXPORT
                } else {
                    Modifiers::NONE
                };
                let v = self.var(VarKind::Var, mods, name, Some(value), at);
                if self.opts.is_cjs() && exported && self.list_depth == 0 {
                    crate::cjs::exported_var(self, v, out);
                } else {
                    let span = self.span(id);
                    self.map_range(v, span);
                    out.push(v);
                }
            }
        }
    }

    /// `A.B.C` (qualified name) → member expression with references resolved.
    pub fn entity_to_expr(&mut self, id: NodeId) -> NodeId {
        match self.n(id) {
            Node::QualifiedName { left, right } => {
                let at = self.at(id);
                let l = self.entity_to_expr(left);
                let r = self.name(right).to_owned();
                self.member(l, &r, at)
            }
            _ => id,
        }
    }

    fn export_named(&mut self, id: NodeId, out: &mut Vec<NodeId>) {
        let Node::ExportNamed {
            type_only,
            specifiers,
            source,
            attributes,
        } = self.n(id)
        else {
            return;
        };
        if type_only {
            self.erase(id);
            return;
        }
        let specs = self.items(specifiers);
        let mut kept = Vec::with_capacity(specs.len());
        for &s in &specs {
            if let Node::ExportSpecifier {
                type_only, local, ..
            } = self.n(s)
            {
                let keep = !type_only
                    && (source.is_some() || !self.is_type_only_local(local))
                    && (self.opts.verbatim_module_syntax || self.value_alias(s));
                if keep {
                    kept.push(s);
                } else {
                    self.erase(s);
                }
            }
        }
        // Without verbatimModuleSyntax an export list that loses everything (or was empty)
        // goes; with it, the statement stays (`export {}`).
        if kept.is_empty() && !self.opts.verbatim_module_syntax {
            self.erase(id);
            return;
        }
        if self.opts.is_cjs() {
            crate::cjs::export_named(self, id, &kept, out);
            return;
        }
        let specifiers = if kept.len() == specs.len() {
            specifiers
        } else {
            self.new_list(&kept)
        };
        if let Some(s) = source {
            self.rewrite_specifier(s);
        }
        self.set(
            id,
            Node::ExportNamed {
                type_only: false,
                specifiers,
                source,
                attributes,
            },
        );
        out.push(id);
    }

    /// `export { T }` where `T` is only a type in this file.
    pub(crate) fn is_type_only_local(&self, local: NodeId) -> bool {
        let Node::Ident { name } = self.n(local) else {
            return false;
        };
        match self.sc.module_binding(name) {
            Some(b) => {
                let bnd = self.sc.binding(b);
                match bnd.kind {
                    BindingKind::TypeImport => true,
                    BindingKind::Namespace => !crate::namespace::is_instantiated_binding(self, b),
                    BindingKind::ConstEnum => !self.opts.preserve_const_enums,
                    _ => false,
                }
            }
            None => self.sc.type_names.contains(&name),
        }
    }

    // ---- module specifiers ----

    /// `rewriteRelativeImportExtensions`: `./a.ts` → `./a.js` (in place, synthesized).
    pub fn rewrite_specifier(&mut self, source: NodeId) {
        if !self.opts.rewrite_relative_import_extensions {
            return;
        }
        let Node::StringLit { value } = self.n(source) else {
            return;
        };
        let spec = self.ast.atom(value).to_owned();
        if let Some(new) = rewrite_ts_extension(&spec, self.opts.jsx_preserve_explicit) {
            let at = self.at(source);
            let s = self.string(&new, at);
            replace(self.ast, source, s);
        }
    }

    /// `require("./a.ts")` in JavaScript files is an import too (observed).
    fn rewrite_require(&mut self, callee: NodeId, args: NodeList) {
        if !self.opts.rewrite_relative_import_extensions
            || self.ast.source_kind.is_typescript()
            || self.name(callee) != "require"
            || self.kind(callee) != NodeKind::Ident
            || self.sc.binding_of_ref(callee).is_some()
        {
            return;
        }
        if let [arg] = *self.ast.list(args)
            && self.kind(arg) != NodeKind::Spread
        {
            self.rewrite_dynamic_specifier(arg);
        }
    }

    fn rewrite_dynamic_specifier(&mut self, arg: NodeId) {
        if !self.opts.rewrite_relative_import_extensions {
            return;
        }
        match self.n(arg) {
            Node::StringLit { .. } => self.rewrite_specifier(arg),
            _ => {
                let at = self.at(arg);
                let f = self.helper_ref(Helper::RewriteRelativeImportExtension, at);
                let moved = crate::builder::clone_node(self.ast, arg);
                let mut args = vec![moved];
                if self.opts.jsx_preserve_explicit {
                    let t = self.bool_lit(true, at);
                    args.push(t);
                }
                let c = self.call(f, &args, at);
                replace(self.ast, arg, c);
            }
        }
    }
}

/// `./a.ts` → `./a.js`, `./b.tsx` → `./b.js` (or `.jsx` when JSX is preserved), `.mts` →
/// `.mjs`, `.cts` → `.cjs`. Declaration files and non-relative specifiers are unchanged.
pub fn rewrite_ts_extension(spec: &str, preserve_jsx: bool) -> Option<String> {
    if !(spec.starts_with("./") || spec.starts_with("../")) {
        return None;
    }
    let lower = spec.to_ascii_lowercase();
    if lower.ends_with(".d.ts") || lower.ends_with(".d.mts") || lower.ends_with(".d.cts") {
        return None;
    }
    // `foo.d.css.ts`-style declaration files.
    if let Some(stem) = lower.strip_suffix(".ts")
        && stem.rsplit('/').next().is_some_and(|f| f.contains(".d."))
    {
        return None;
    }
    let (cut, new) = if lower.ends_with(".tsx") {
        (4, if preserve_jsx { ".jsx" } else { ".js" })
    } else if lower.ends_with(".mts") {
        (4, ".mjs")
    } else if lower.ends_with(".cts") {
        (4, ".cjs")
    } else if lower.ends_with(".ts") {
        (3, ".js")
    } else {
        return None;
    };
    Some(format!("{}{}", &spec[..spec.len() - cut], new))
}

#[cfg(test)]
mod tests {
    use super::rewrite_ts_extension as r;

    #[test]
    fn rewrite_extensions() {
        assert_eq!(r("./a.ts", false).as_deref(), Some("./a.js"));
        assert_eq!(r("../b/c.tsx", false).as_deref(), Some("../b/c.js"));
        assert_eq!(r("./c.tsx", true).as_deref(), Some("./c.jsx"));
        assert_eq!(r("./d.MTS", false).as_deref(), Some("./d.mjs"));
        assert_eq!(r("./e.cts", false).as_deref(), Some("./e.cjs"));
        assert_eq!(r("./f.d.ts", false), None);
        assert_eq!(r("./g.d.css.ts", false), None);
        assert_eq!(r("pkg/a.ts", false), None);
        assert_eq!(r("./h.js", false), None);
    }
}
