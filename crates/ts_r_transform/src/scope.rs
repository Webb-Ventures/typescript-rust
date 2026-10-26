//! Scope analysis for transforms: which declaration each *value* reference resolves to.
//!
//! This is not the checker's binder. It knows lexical scoping (var hoisting, block scopes,
//! function/class/catch/namespace/enum scopes) and skips every type position, so a
//! reference counted here is a reference that survives type erasure. That is exactly what
//! tsc's import elision, namespace export rewriting and CommonJS rewriting need.
//!
//! The walk is recursive (callers run it under `with_stack_for`).

use rustc_hash::{FxHashMap, FxHashSet};
use ts_r_ast::{Ast, Atom, Modifiers, ModuleKind, Node, NodeId, NodeKind, NodeList, VarKind};

pub(crate) type BindingId = u32;
pub(crate) const NO_BINDING: u32 = u32::MAX;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum BindingKind {
    Var,
    Let,
    Const,
    Function,
    Class,
    Param,
    Catch,
    /// A value import (`import x`, `{ x }`, `* as x`).
    Import,
    /// `import type` / `{ type x }`: never a runtime value.
    TypeImport,
    ImportEquals,
    Enum,
    ConstEnum,
    Namespace,
    /// A member of the enum whose scope it is in.
    EnumMember,
    /// A name exported by another declaration of the same (merged) namespace.
    MergedNsExport,
}

#[derive(Clone, Debug)]
pub(crate) struct Binding {
    pub name: Atom,
    pub kind: BindingKind,
    pub scope: u32,
    /// The declaring identifier (or statement for enums/namespaces without one).
    pub decl: NodeId,
    /// Value references (including `export { x }`).
    pub refs: u32,
    /// Declared with `export` (in a module or namespace body).
    pub exported: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ScopeKind {
    Module,
    Function,
    Block,
    Class,
    /// A namespace body; the node is the `Module` declaration.
    Namespace(NodeId),
    Enum,
}

#[derive(Debug)]
pub(crate) struct Scope {
    pub parent: Option<u32>,
    pub kind: ScopeKind,
    names: FxHashMap<Atom, BindingId>,
}

/// Result of [`analyze`].
#[derive(Debug)]
pub(crate) struct Scopes {
    pub scopes: Vec<Scope>,
    pub bindings: Vec<Binding>,
    /// Per node id: the binding a value-reference `Ident` resolves to, or [`NO_BINDING`].
    resolved: Vec<u32>,
    /// Per node id: the scope a reference `Ident` was found in (only for references).
    ref_scope: FxHashMap<NodeId, u32>,
    /// Binding identifier node → binding.
    pub decl_of: FxHashMap<NodeId, BindingId>,
    /// Names of module-level type-only declarations (interfaces, type aliases).
    pub type_names: FxHashSet<Atom>,
    /// The file contains JSX.
    pub has_jsx: bool,
    /// Every identifier name in the file (for fresh-name generation).
    pub names: FxHashSet<Atom>,
    /// The file has `import`/`export` syntax (ESM module indicator).
    pub has_module_syntax: bool,
    /// The file assigns to `exports.x`, `exports[k]` or `module.exports`, or calls
    /// `require(...)`, anywhere (tsc's CommonJS module indicator for JavaScript files;
    /// observed: shadowed names count too, reads do not).
    pub has_cjs_syntax: bool,
    /// Namespace name segment identifier → the scope of that segment's body.
    pub ns_scope_of: FxHashMap<NodeId, u32>,
    /// Enum declaration → its member scope.
    pub enum_scope_of: FxHashMap<NodeId, u32>,
    /// Enum binding → the binding of the first declaration of the same merged enum, for
    /// enums declared in several blocks of one namespace (each block has its own scope).
    enum_canon: FxHashMap<BindingId, BindingId>,
    /// `(namespace binding, name)` → the first exported enum of that name in the namespace.
    ns_enums: FxHashMap<(BindingId, Atom), BindingId>,
    /// `(namespace binding, name)` → a namespace declared in the namespace.
    ns_namespaces: FxHashMap<(BindingId, Atom), BindingId>,
    /// JSX element or fragment → the scope it is in (classic factories resolve there).
    pub jsx_scope: FxHashMap<NodeId, u32>,
    /// `const x = init` without a type annotation: declaring identifier → initializer (enum
    /// initializers fold such constants, as tsc's evaluator does).
    pub const_inits: FxHashMap<NodeId, NodeId>,
    /// Identifiers declared by a `declare var/let/const`, `declare class` or `declare
    /// function` (no declaration in the output).
    pub ambient_vars: FxHashSet<NodeId>,
}

impl Scopes {
    pub fn binding(&self, b: BindingId) -> &Binding {
        &self.bindings[b as usize]
    }
    /// A module-scope binding by name.
    pub fn module_binding(&self, name: Atom) -> Option<BindingId> {
        self.scopes[0].names.get(&name).copied()
    }
    pub fn binding_of_ref(&self, id: NodeId) -> Option<BindingId> {
        match self.resolved.get(id.index()) {
            Some(&b) if b != NO_BINDING => Some(b),
            _ => None,
        }
    }
    /// The enum `name` declared in namespace `ns` (any of its blocks).
    pub fn ns_enum(&self, ns: BindingId, name: Atom) -> Option<BindingId> {
        self.ns_enums.get(&(ns, name)).copied()
    }
    /// The namespace `name` declared in namespace `ns` (`namespace A.B` or a nested one).
    pub fn ns_namespace(&self, ns: BindingId, name: Atom) -> Option<BindingId> {
        self.ns_namespaces.get(&(ns, name)).copied()
    }
    /// The binding that identifies a (merged) enum across namespace blocks.
    pub fn enum_key(&self, b: BindingId) -> BindingId {
        self.enum_canon.get(&b).copied().unwrap_or(b)
    }
    /// Looks `name` up from `scope` outwards.
    pub fn lookup(&self, mut scope: u32, name: Atom) -> Option<BindingId> {
        loop {
            let s = &self.scopes[scope as usize];
            if let Some(&b) = s.names.get(&name) {
                return Some(b);
            }
            scope = s.parent?;
        }
    }
}

struct Walker<'a> {
    ast: &'a Ast,
    out: Scopes,
    refs: Vec<(NodeId, u32)>,
    metadata: bool,
    /// `strictNullChecks`: in a decorated signature's union type, `null` and `undefined`
    /// are skipped only without it (the entity rule of `metadata_entity`).
    strict_null: bool,
    /// For decorator metadata: the emitter's resolver, which tells type references that
    /// keep no import alive (reached through `import type`/`export type`).
    resolver: Option<(&'a dyn crate::EmitResolver, ts_r_diagnostics::FileId)>,
    /// The file has a computed key somewhere; only then are types walked (`type_keys`).
    computed_keys: bool,
    /// Namespace body scope → the namespace's binding (shared by its merged blocks).
    ns_binding_of_scope: FxHashMap<u32, BindingId>,
}

/// Analyzes the whole file. `decorator_metadata`: type annotations of decorated members are
/// runtime references (`emitDecoratorMetadata`), as far as `resolver` tells.
pub(crate) fn analyze<'a>(
    ast: &'a Ast,
    decorator_metadata: bool,
    strict_null_checks: bool,
    resolver: Option<(&'a dyn crate::EmitResolver, ts_r_diagnostics::FileId)>,
) -> Scopes {
    let mut w = Walker {
        ast,
        out: Scopes {
            scopes: vec![Scope {
                parent: None,
                kind: ScopeKind::Module,
                names: FxHashMap::default(),
            }],
            bindings: Vec::new(),
            resolved: vec![NO_BINDING; ast.nodes.len()],
            ref_scope: FxHashMap::default(),
            decl_of: FxHashMap::default(),
            type_names: FxHashSet::default(),
            has_jsx: false,
            names: FxHashSet::default(),
            has_module_syntax: false,
            has_cjs_syntax: false,
            ns_scope_of: FxHashMap::default(),
            enum_scope_of: FxHashMap::default(),
            enum_canon: FxHashMap::default(),
            ns_enums: FxHashMap::default(),
            ns_namespaces: FxHashMap::default(),
            jsx_scope: FxHashMap::default(),
            const_inits: FxHashMap::default(),
            ambient_vars: FxHashSet::default(),
        },
        refs: Vec::new(),
        metadata: decorator_metadata,
        strict_null: strict_null_checks,
        resolver,
        computed_keys: ast
            .nodes
            .iter()
            .any(|n| matches!(n, Node::ComputedKey { .. })),
        ns_binding_of_scope: FxHashMap::default(),
    };
    if let Node::Program { stmts } = *ast.node(ast.root) {
        w.stmts(stmts, 0);
    }
    for (i, n) in ast.nodes.iter().enumerate().skip(1) {
        if let Node::Ident { name } = n {
            w.out.names.insert(*name);
        }
        let _ = i;
    }
    let refs = std::mem::take(&mut w.refs);
    for (id, scope) in refs {
        let Node::Ident { name } = *ast.node(id) else {
            continue;
        };
        w.out.ref_scope.insert(id, scope);
        if let Some(b) = w.out.lookup(scope, name) {
            w.out.resolved[id.index()] = b;
            w.out.bindings[b as usize].refs += 1;
        }
    }
    w.out
}

impl Walker<'_> {
    fn new_scope(&mut self, parent: u32, kind: ScopeKind) -> u32 {
        self.out.scopes.push(Scope {
            parent: Some(parent),
            kind,
            names: FxHashMap::default(),
        });
        (self.out.scopes.len() - 1) as u32
    }

    /// The scope `var` declarations go to.
    fn var_scope(&self, mut scope: u32) -> u32 {
        loop {
            let s = &self.out.scopes[scope as usize];
            match s.kind {
                ScopeKind::Module | ScopeKind::Function | ScopeKind::Namespace(_) => return scope,
                _ => match s.parent {
                    Some(p) => scope = p,
                    None => return scope,
                },
            }
        }
    }

    fn declare(
        &mut self,
        scope: u32,
        ident: NodeId,
        kind: BindingKind,
        exported: bool,
    ) -> Option<BindingId> {
        let Node::Ident { name } = *self.ast.node(ident) else {
            return None;
        };
        let names = &self.out.scopes[scope as usize].names;
        let local_value = |k: BindingKind| {
            matches!(
                k,
                BindingKind::Var
                    | BindingKind::Let
                    | BindingKind::Const
                    | BindingKind::Function
                    | BindingKind::Class
            )
        };
        // An import and a local declaration of the same name (TS2440): references mean the
        // local, so the import is unused and elided (observed). The import gets a binding of
        // its own that no name lookup reaches.
        let shadowed_import = names.get(&name).is_some_and(|&b| {
            let old = self.out.bindings[b as usize].kind;
            (old == BindingKind::Import && local_value(kind))
                || (kind == BindingKind::Import && local_value(old))
        });
        if shadowed_import {
            let b = self.out.bindings.len() as u32;
            self.out.bindings.push(Binding {
                name,
                kind,
                scope,
                decl: ident,
                refs: 0,
                exported,
            });
            if kind != BindingKind::Import {
                self.out.scopes[scope as usize].names.insert(name, b);
            }
            self.out.decl_of.insert(ident, b);
            return Some(b);
        }
        if let Some(&b) = names.get(&name) {
            // Redeclaration (var twice, merged declarations, overloads): one binding.
            let bnd = &mut self.out.bindings[b as usize];
            bnd.exported |= exported;
            if bnd.kind == BindingKind::MergedNsExport
                || (bnd.kind == BindingKind::TypeImport && kind != BindingKind::TypeImport)
            {
                bnd.kind = kind;
                bnd.decl = ident;
            }
            if kind != BindingKind::MergedNsExport {
                self.out.decl_of.insert(ident, b);
            }
            return Some(b);
        }
        let b = self.out.bindings.len() as u32;
        self.out.bindings.push(Binding {
            name,
            kind,
            scope,
            decl: ident,
            refs: 0,
            exported,
        });
        self.out.scopes[scope as usize].names.insert(name, b);
        // A name exported by another block of a namespace: the identifier belongs to that
        // block's declaration, whose own binding claims it.
        if kind != BindingKind::MergedNsExport {
            self.out.decl_of.insert(ident, b);
        }
        Some(b)
    }

    fn reference(&mut self, ident: NodeId, scope: u32) {
        if matches!(self.ast.node(ident), Node::Ident { .. }) {
            self.refs.push((ident, scope));
        }
    }

    fn list(&self, l: NodeList) -> Vec<NodeId> {
        self.ast.list(l).to_vec()
    }

    // ---- statements ----

    fn stmts(&mut self, list: NodeList, scope: u32) {
        let items = self.list(list);
        // Merged namespaces: names exported by each declaration of a namespace name (the
        // whole dotted path for `namespace A.B {}`).
        let mut ns_groups: FxHashMap<Vec<Atom>, Vec<NodeId>> = FxHashMap::default();
        let path_of = |w: &Self, s: NodeId| -> Option<Vec<Atom>> {
            let Node::Module {
                kind: ModuleKind::Namespace | ModuleKind::Module,
                name,
                body: Some(_),
                ..
            } = *w.ast.node(s)
            else {
                return None;
            };
            let mut segs = Vec::new();
            w.namespace_segments(name, &mut segs);
            segs.iter()
                .map(|&seg| match *w.ast.node(seg) {
                    Node::Ident { name } => Some(name),
                    _ => None,
                })
                .collect()
        };
        for &s in &items {
            if let Some(path) = path_of(self, s) {
                ns_groups.entry(path).or_default().push(s);
            }
        }
        for &s in &items {
            let group = path_of(self, s)
                .and_then(|p| ns_groups.get(&p))
                .cloned()
                .unwrap_or_default();
            self.stmt(s, scope, &group);
        }
    }

    fn stmt(&mut self, id: NodeId, scope: u32, ns_group: &[NodeId]) {
        let n = *self.ast.node(id);
        match n {
            Node::Block { stmts } => {
                let s = self.new_scope(scope, ScopeKind::Block);
                self.stmts(stmts, s);
            }
            Node::VarDecl {
                modifiers,
                kind,
                decls,
            } => self.var_decl(modifiers, kind, decls, scope),
            Node::FunctionDecl {
                modifiers,
                name,
                params,
                return_type,
                body,
                ..
            } => {
                if let Some(nm) = name {
                    let target = if self.out.scopes[scope as usize].kind == ScopeKind::Block {
                        scope
                    } else {
                        self.var_scope(scope)
                    };
                    self.declare(
                        target,
                        nm,
                        BindingKind::Function,
                        modifiers.contains(Modifiers::EXPORT),
                    );
                    if modifiers.contains(Modifiers::DECLARE) {
                        self.out.ambient_vars.insert(nm);
                    }
                }
                if modifiers.contains(Modifiers::EXPORT) {
                    self.out.has_module_syntax |= scope == 0;
                }
                if !modifiers.contains(Modifiers::DECLARE) && body.is_some() {
                    self.function(None, params, return_type, body, scope);
                }
            }
            Node::ClassDecl {
                modifiers, name, ..
            } => {
                if let Some(nm) = name {
                    self.declare(
                        scope,
                        nm,
                        BindingKind::Class,
                        modifiers.contains(Modifiers::EXPORT),
                    );
                    if modifiers.contains(Modifiers::DECLARE) {
                        self.out.ambient_vars.insert(nm);
                    }
                }
                if modifiers.contains(Modifiers::EXPORT) && scope == 0 {
                    self.out.has_module_syntax = true;
                }
                if !modifiers.contains(Modifiers::DECLARE) {
                    self.class(id, scope);
                }
            }
            Node::Interface {
                name, modifiers, ..
            }
            | Node::TypeAlias {
                name, modifiers, ..
            } => {
                if let Node::Ident { name } = *self.ast.node(name) {
                    self.out.type_names.insert(name);
                }
                if modifiers.contains(Modifiers::EXPORT) && scope == 0 {
                    self.out.has_module_syntax = true;
                }
                if !modifiers.contains(Modifiers::DECLARE) {
                    self.type_keys(id, scope);
                }
            }
            Node::Enum {
                modifiers,
                name,
                members,
            } => {
                let kind = if modifiers.contains(Modifiers::CONST) {
                    BindingKind::ConstEnum
                } else {
                    BindingKind::Enum
                };
                let b = self.declare(scope, name, kind, modifiers.contains(Modifiers::EXPORT));
                // Only exported enums merge across blocks; a private one is local to its block.
                if let (true, Some(b), Some(&ns), Node::Ident { name: atom }) = (
                    modifiers.contains(Modifiers::EXPORT),
                    b,
                    self.ns_binding_of_scope.get(&scope),
                    *self.ast.node(name),
                ) {
                    let first = *self.out.ns_enums.entry((ns, atom)).or_insert(b);
                    if first != b {
                        self.out.enum_canon.insert(b, first);
                    }
                }
                if modifiers.contains(Modifiers::EXPORT) && scope == 0 {
                    self.out.has_module_syntax = true;
                }
                if modifiers.contains(Modifiers::DECLARE) {
                    return;
                }
                let s = self.new_scope(scope, ScopeKind::Enum);
                self.out.enum_scope_of.insert(id, s);
                let ms = self.list(members);
                for &m in &ms {
                    if let Node::EnumMember { name, .. } = *self.ast.node(m)
                        && matches!(self.ast.node(name), Node::Ident { .. })
                    {
                        self.declare(s, name, BindingKind::EnumMember, false);
                    }
                }
                for &m in &ms {
                    if let Node::EnumMember {
                        name,
                        init: Some(init),
                    } = *self.ast.node(m)
                    {
                        if let Node::ComputedKey { expr } = *self.ast.node(name) {
                            self.expr(expr, scope);
                        }
                        self.expr(init, s);
                    }
                }
            }
            Node::Module {
                modifiers,
                kind,
                name,
                body,
            } => {
                if kind == ModuleKind::Global
                    || matches!(self.ast.node(name), Node::StringLit { .. })
                {
                    return;
                }
                let exported = modifiers.contains(Modifiers::EXPORT);
                if exported && scope == 0 {
                    self.out.has_module_syntax = true;
                }
                let first = self.namespace_name(name);
                let mut ns_binding = self.declare(scope, first, BindingKind::Namespace, exported);
                if let (Some(b), Some(&parent), Node::Ident { name: atom }) = (
                    ns_binding,
                    self.ns_binding_of_scope.get(&scope),
                    *self.ast.node(first),
                ) {
                    self.out.ns_namespaces.entry((parent, atom)).or_insert(b);
                }
                if modifiers.contains(Modifiers::DECLARE) {
                    return;
                }
                let Some(body) = body else { return };
                // `namespace A.B.C {}`: one scope per segment.
                let mut s = self.new_scope(scope, ScopeKind::Namespace(id));
                if let Some(b) = ns_binding {
                    self.ns_binding_of_scope.insert(s, b);
                }
                let mut segs = Vec::new();
                self.namespace_segments(name, &mut segs);
                if let Some(&seg) = segs.first() {
                    self.out.ns_scope_of.insert(seg, s);
                }
                for &seg in segs.iter().skip(1) {
                    let parent = ns_binding;
                    ns_binding = self.declare(s, seg, BindingKind::Namespace, true);
                    if let (Some(p), Some(b), Node::Ident { name: atom }) =
                        (parent, ns_binding, *self.ast.node(seg))
                    {
                        self.out.ns_namespaces.entry((p, atom)).or_insert(b);
                    }
                    s = self.new_scope(s, ScopeKind::Namespace(id));
                    self.out.ns_scope_of.insert(seg, s);
                    if let Some(b) = ns_binding {
                        self.ns_binding_of_scope.insert(s, b);
                    }
                }
                // Names exported by the other declarations of this namespace.
                for &other in ns_group {
                    if other == id {
                        continue;
                    }
                    for nm in self.exported_names(other) {
                        self.declare(s, nm, BindingKind::MergedNsExport, true);
                    }
                }
                if let Node::ModuleBlock { stmts } = *self.ast.node(body) {
                    self.stmts(stmts, s);
                }
            }
            Node::ImportDecl {
                type_only,
                specifiers,
                ..
            } => {
                self.out.has_module_syntax = true;
                for s in self.list(specifiers) {
                    match *self.ast.node(s) {
                        Node::ImportDefaultSpecifier { local }
                        | Node::ImportNamespaceSpecifier { local } => {
                            let k = if type_only {
                                BindingKind::TypeImport
                            } else {
                                BindingKind::Import
                            };
                            self.declare(scope, local, k, false);
                        }
                        Node::ImportSpecifier {
                            type_only: t,
                            local,
                            ..
                        } => {
                            let k = if type_only || t {
                                BindingKind::TypeImport
                            } else {
                                BindingKind::Import
                            };
                            self.declare(scope, local, k, false);
                        }
                        _ => {}
                    }
                }
            }
            Node::ImportEquals {
                modifiers,
                type_only,
                name,
                module_ref,
            } => {
                if matches!(self.ast.node(module_ref), Node::ExternalModuleRef { .. }) {
                    self.out.has_module_syntax |= scope == 0;
                }
                let k = if type_only {
                    BindingKind::TypeImport
                } else {
                    BindingKind::ImportEquals
                };
                self.declare(scope, name, k, modifiers.contains(Modifiers::EXPORT));
                if let Some(left) = self.leftmost_entity(module_ref) {
                    self.reference(left, scope);
                }
            }
            Node::ExportNamed {
                type_only,
                specifiers,
                source,
                ..
            } => {
                self.out.has_module_syntax = true;
                if source.is_none() && !type_only {
                    for s in self.list(specifiers) {
                        if let Node::ExportSpecifier {
                            type_only: false,
                            local,
                            ..
                        } = *self.ast.node(s)
                        {
                            self.reference(local, scope);
                        }
                    }
                }
            }
            Node::ExportAll { .. } | Node::NamespaceExport { .. } => {
                self.out.has_module_syntax = true;
            }
            Node::ExportDefaultExpr { expr } => {
                self.out.has_module_syntax = true;
                self.expr(expr, scope);
            }
            Node::ExportAssignment { expr } => {
                self.out.has_module_syntax = true;
                self.expr(expr, scope);
            }
            Node::ExprStmt { expr } | Node::Throw { arg: expr } => self.expr(expr, scope),
            Node::Return { arg } => {
                if let Some(a) = arg {
                    self.expr(a, scope);
                }
            }
            Node::If { test, cons, alt } => {
                self.expr(test, scope);
                self.sub_stmt(cons, scope);
                if let Some(a) = alt {
                    self.sub_stmt(a, scope);
                }
            }
            Node::For {
                init,
                test,
                update,
                body,
            } => {
                let s = self.new_scope(scope, ScopeKind::Block);
                if let Some(i) = init {
                    self.for_init(i, s);
                }
                if let Some(t) = test {
                    self.expr(t, s);
                }
                if let Some(u) = update {
                    self.expr(u, s);
                }
                self.sub_stmt(body, s);
            }
            Node::ForIn { left, right, body }
            | Node::ForOf {
                left, right, body, ..
            } => {
                let s = self.new_scope(scope, ScopeKind::Block);
                self.for_init(left, s);
                self.expr(right, s);
                self.sub_stmt(body, s);
            }
            Node::While { test, body } | Node::DoWhile { body, test } => {
                self.expr(test, scope);
                self.sub_stmt(body, scope);
            }
            Node::Labeled { body, .. } => self.sub_stmt(body, scope),
            Node::Switch { disc, cases } => {
                self.expr(disc, scope);
                let s = self.new_scope(scope, ScopeKind::Block);
                for c in self.list(cases) {
                    if let Node::SwitchCase { test, body } = *self.ast.node(c) {
                        if let Some(t) = test {
                            self.expr(t, s);
                        }
                        self.stmts(body, s);
                    }
                }
            }
            Node::Try {
                block,
                handler,
                finalizer,
            } => {
                self.stmt(block, scope, &[]);
                if let Some(h) = handler
                    && let Node::CatchClause { param, body, .. } = *self.ast.node(h)
                {
                    let s = self.new_scope(scope, ScopeKind::Block);
                    if let Some(p) = param {
                        self.pattern(p, s, BindingKind::Catch, false);
                    }
                    if let Node::Block { stmts } = *self.ast.node(body) {
                        self.stmts(stmts, s);
                    }
                }
                if let Some(f) = finalizer {
                    self.stmt(f, scope, &[]);
                }
            }
            Node::With { obj, body } => {
                self.expr(obj, scope);
                self.sub_stmt(body, scope);
            }
            _ => {
                if !n.is_statement() {
                    self.expr(id, scope);
                }
            }
        }
    }

    /// A statement in a single-statement position (if/loop body). An `import` or `export`
    /// there (a syntax error) does not make the file a module: tsc looks only at top-level
    /// statements (observed: `if (true) export type {};` keeps `"use strict"`).
    fn sub_stmt(&mut self, id: NodeId, scope: u32) {
        let before = self.out.has_module_syntax;
        let direct = match *self.ast.node(id) {
            Node::ImportDecl { .. }
            | Node::ExportNamed { .. }
            | Node::ExportAll { .. }
            | Node::NamespaceExport { .. }
            | Node::ExportDefaultExpr { .. }
            | Node::ExportAssignment { .. } => true,
            ref n => n.modifiers().contains(Modifiers::EXPORT),
        };
        self.stmt(id, scope, &[]);
        if direct {
            self.out.has_module_syntax = before;
        }
    }

    fn for_init(&mut self, id: NodeId, scope: u32) {
        match *self.ast.node(id) {
            Node::VarDecl {
                modifiers,
                kind,
                decls,
            } => self.var_decl(modifiers, kind, decls, scope),
            _ => self.expr(id, scope),
        }
    }

    fn var_decl(&mut self, modifiers: Modifiers, kind: VarKind, decls: NodeList, scope: u32) {
        let exported = modifiers.contains(Modifiers::EXPORT);
        if exported && scope == 0 {
            self.out.has_module_syntax = true;
        }
        let (target, bk) = match kind {
            VarKind::Var => (self.var_scope(scope), BindingKind::Var),
            VarKind::Let => (scope, BindingKind::Let),
            _ => (scope, BindingKind::Const),
        };
        let declare = modifiers.contains(Modifiers::DECLARE);
        for d in self.list(decls) {
            if let Node::VarDeclarator {
                name,
                init,
                type_ann,
                ..
            } = *self.ast.node(d)
            {
                self.pattern(name, target, bk, exported);
                if declare {
                    if matches!(self.ast.node(name), Node::Ident { .. }) {
                        self.out.ambient_vars.insert(name);
                    }
                    continue;
                }
                if kind == VarKind::Const
                    && type_ann.is_none()
                    && let Some(i) = init
                    && matches!(self.ast.node(name), Node::Ident { .. })
                {
                    self.out.const_inits.insert(name, i);
                }
                if let Some(t) = type_ann {
                    self.type_keys(t, scope);
                }
                if let Some(i) = init {
                    self.expr(i, scope);
                }
            }
        }
    }

    /// Declares the names bound by a pattern; walks defaults and computed keys.
    fn pattern(&mut self, id: NodeId, scope: u32, kind: BindingKind, exported: bool) {
        match *self.ast.node(id) {
            Node::Ident { .. } => {
                self.declare(scope, id, kind, exported);
            }
            Node::ObjectPattern { props } => {
                for p in self.list(props) {
                    self.pattern(p, scope, kind, exported);
                }
            }
            Node::ArrayPattern { elements } => {
                for e in self.list(elements) {
                    self.pattern(e, scope, kind, exported);
                }
            }
            Node::BindingProperty { key, value, init } => {
                if let Node::ComputedKey { expr } = *self.ast.node(key) {
                    self.expr(expr, scope);
                }
                match value {
                    Some(v) => self.pattern(v, scope, kind, exported),
                    None => self.pattern(key, scope, kind, exported),
                }
                if let Some(i) = init {
                    self.expr(i, scope);
                }
            }
            Node::AssignPattern { left, right } => {
                self.pattern(left, scope, kind, exported);
                self.expr(right, scope);
            }
            Node::Rest { arg } => self.pattern(arg, scope, kind, exported),
            Node::Param {
                decorators,
                pattern,
                init,
                ..
            } => {
                self.decorators(decorators, scope);
                self.pattern(pattern, scope, kind, exported);
                if let Some(i) = init {
                    self.expr(i, scope);
                }
            }
            _ => {}
        }
    }

    fn decorators(&mut self, list: NodeList, scope: u32) {
        for d in self.list(list) {
            if let Node::Decorator { expr } = *self.ast.node(d) {
                self.expr(expr, scope);
            }
        }
    }

    /// Params and body of a function in a new function scope.
    fn function(
        &mut self,
        name: Option<NodeId>,
        params: NodeList,
        return_type: Option<NodeId>,
        body: Option<NodeId>,
        scope: u32,
    ) {
        let s = self.new_scope(scope, ScopeKind::Function);
        if let Some(n) = name {
            self.declare(s, n, BindingKind::Function, false);
        }
        for p in self.list(params) {
            if let Node::Param {
                pattern, type_ann, ..
            } = *self.ast.node(p)
            {
                if let Some(t) = type_ann {
                    self.type_keys(t, s);
                }
                // `this` parameters are erased.
                if matches!(self.ast.node(pattern), Node::Ident { name } if self.ast.atom(*name) == "this")
                {
                    continue;
                }
            }
            self.pattern(p, s, BindingKind::Param, false);
        }
        if let Some(t) = return_type {
            self.type_keys(t, s);
        }
        if let Some(b) = body {
            match *self.ast.node(b) {
                Node::Block { stmts } => self.stmts(stmts, s),
                _ => self.expr(b, s),
            }
        }
    }

    fn class(&mut self, id: NodeId, scope: u32) {
        let (decorators, name, heritage, members) = match *self.ast.node(id) {
            Node::ClassDecl {
                decorators,
                name,
                heritage,
                members,
                ..
            }
            | Node::ClassExpr {
                decorators,
                name,
                heritage,
                members,
                ..
            } => (decorators, name, heritage, members),
            _ => return,
        };
        self.decorators(decorators, scope);
        let s = self.new_scope(scope, ScopeKind::Class);
        if self.ast.kind(id) == NodeKind::ClassExpr
            && let Some(n) = name
        {
            self.declare(s, n, BindingKind::Class, false);
        }
        for h in self.list(heritage) {
            if let Node::HeritageClause {
                kind: ts_r_ast::HeritageKind::Extends,
                types,
            } = *self.ast.node(h)
            {
                for t in self.list(types) {
                    match *self.ast.node(t) {
                        Node::ExprWithTypeArgs { expr, .. } => self.expr(expr, scope),
                        _ => self.expr(t, scope),
                    }
                }
            }
        }
        let decorated = !decorators.is_empty();
        for m in self.list(members) {
            match *self.ast.node(m) {
                Node::PropertyDef {
                    decorators: ds,
                    modifiers,
                    key,
                    type_ann,
                    value,
                    ..
                } => {
                    if modifiers.contains(Modifiers::DECLARE) {
                        // A decorated `declare` field is still decorated (legacy decorators).
                        if !ds.is_empty() {
                            self.decorators(ds, s);
                            if self.metadata
                                && let Some(t) = type_ann
                            {
                                self.type_as_value(t, s);
                            }
                            self.key(key, s);
                        }
                        continue;
                    }
                    // `abstract [k]: T` is erased but still references `k` (observed).
                    if modifiers.contains(Modifiers::ABSTRACT) {
                        self.key(key, s);
                        continue;
                    }
                    if let Some(t) = type_ann {
                        self.type_keys(t, s);
                    }
                    self.decorators(ds, s);
                    if self.metadata
                        && !ds.is_empty()
                        && let Some(t) = type_ann
                    {
                        self.type_as_value(t, s);
                    }
                    self.key(key, s);
                    if let Some(v) = value {
                        let fs = self.new_scope(s, ScopeKind::Function);
                        self.expr(v, fs);
                    }
                }
                Node::MethodDef {
                    decorators: ds,
                    key,
                    value,
                    kind,
                    ..
                } => {
                    self.decorators(ds, s);
                    self.key(key, s);
                    if let Node::FunctionExpr {
                        params,
                        body,
                        return_type,
                        ..
                    } = *self.ast.node(value)
                    {
                        let ctor = kind == ts_r_ast::MethodKind::Constructor;
                        if self.metadata {
                            let any_param_deco = self.list(params).iter().any(|&p| {
                                matches!(self.ast.node(p), Node::Param { decorators, .. } if !decorators.is_empty())
                            });
                            // A decorated accessor keeps its own annotation, or else its
                            // pair's (`get x(): T` / `set x(v: T)`).
                            let accessor = matches!(
                                kind,
                                ts_r_ast::MethodKind::Get | ts_r_ast::MethodKind::Set
                            );
                            if accessor && !ds.is_empty() {
                                let own = self.accessor_annotation(m);
                                let ann = own.or_else(|| {
                                    self.paired_accessor(members, m)
                                        .and_then(|o| self.accessor_annotation(o))
                                });
                                if let Some(t) = ann {
                                    self.type_as_value(t, s);
                                }
                            } else if !ds.is_empty()
                                || any_param_deco
                                || (ctor && decorated && body.is_some())
                            {
                                // A decorated method, a signature with decorated parameters,
                                // or the constructor of a decorated class.
                                for p in self.list(params) {
                                    if let Some(t) = self.param_metadata_type(p) {
                                        self.type_as_value(t, s);
                                    }
                                }
                                if !ctor && let Some(r) = return_type {
                                    self.type_as_value(r, s);
                                }
                            }
                        }
                        if body.is_some() {
                            self.function(None, params, return_type, body, s);
                        }
                    }
                }
                Node::StaticBlock { body } => {
                    let fs = self.new_scope(s, ScopeKind::Function);
                    self.stmts(body, fs);
                }
                _ => {}
            }
        }
    }

    /// For decorator metadata: the entity a decorated signature's type names is a runtime
    /// reference (its import is kept), unless it is reached only through `import type`.
    fn type_as_value(&mut self, t: NodeId, scope: u32) {
        let Some(r) = self.metadata_entity(t, 0) else {
            return;
        };
        let Node::TypeRef { name, .. } = *self.ast.node(r) else {
            return;
        };
        let span = self.ast.span(r);
        if matches!(self.ast.node(name), Node::Ident { .. })
            && let Some((res, file)) = self.resolver
            && span.start < span.end
            && matches!(res.metadata_type(file, r), crate::MetadataType::TypeOnly(_))
        {
            return;
        }
        if let Some(l) = self.leftmost_entity(name) {
            self.reference(l, scope);
        }
    }

    /// The type reference whose entity a metadata type keeps alive: the type itself, the
    /// inside of parentheses or a named tuple member, or the one entity every member of a
    /// union, intersection or conditional's branches agrees on (`metadata_entity_list`).
    /// Anything else (keywords, literals, functions) names no entity.
    fn metadata_entity(&self, t: NodeId, depth: u32) -> Option<NodeId> {
        if depth > 256 {
            return None;
        }
        match *self.ast.node(t) {
            Node::TypeRef { .. } => Some(t),
            Node::ParenType { ty } | Node::NamedTupleMember { ty, .. } => {
                self.metadata_entity(ty, depth + 1)
            }
            Node::UnionType { types } | Node::IntersectionType { types } => {
                self.metadata_entity_list(&self.list(types), depth)
            }
            Node::ConditionalType {
                true_ty, false_ty, ..
            } => self.metadata_entity_list(&[true_ty, false_ty], depth),
            _ => None,
        }
    }

    /// The members agree when each names an entity and, with more than one, all are the
    /// same plain identifier. `never` is skipped, and `null`/`undefined` too without
    /// `strictNullChecks`; any other member naming no entity means none.
    fn metadata_entity_list(&self, types: &[NodeId], depth: u32) -> Option<NodeId> {
        use ts_r_ast::KeywordTypeKind as K;
        let mut common: Option<NodeId> = None;
        for &t in types {
            match *self.ast.node(t) {
                Node::KeywordType { kind: K::Never } => continue,
                Node::KeywordType {
                    kind: K::Null | K::Undefined,
                } if !self.strict_null => continue,
                _ => {}
            }
            let one = self.metadata_entity(t, depth + 1)?;
            match common {
                None => common = Some(one),
                Some(c) => {
                    let ident = |r: NodeId| match *self.ast.node(r) {
                        Node::TypeRef { name, .. } => match *self.ast.node(name) {
                            Node::Ident { name } => Some(name),
                            _ => None,
                        },
                        _ => None,
                    };
                    match (ident(c), ident(one)) {
                        (Some(a), Some(b)) if a == b => {}
                        _ => return None,
                    }
                }
            }
        }
        common
    }

    /// A parameter's type for metadata: a rest parameter's element type (`...a: T[]`,
    /// `...a: Array<T>`, any reference with one type argument).
    fn param_metadata_type(&self, p: NodeId) -> Option<NodeId> {
        let Node::Param { rest, type_ann, .. } = *self.ast.node(p) else {
            return None;
        };
        let t = type_ann?;
        if !rest {
            return Some(t);
        }
        match *self.ast.node(t) {
            Node::ArrayType { elem } => Some(elem),
            Node::TypeRef {
                type_args: Some(args),
                ..
            } => match *self.ast.node(args) {
                Node::TypeArgs { args } => {
                    let l = self.list(args);
                    (l.len() == 1).then(|| l[0])
                }
                _ => None,
            },
            _ => None,
        }
    }

    /// A getter's return type or a setter's parameter type.
    fn accessor_annotation(&self, m: NodeId) -> Option<NodeId> {
        let Node::MethodDef { kind, value, .. } = *self.ast.node(m) else {
            return None;
        };
        let Node::FunctionExpr {
            params,
            return_type,
            ..
        } = *self.ast.node(value)
        else {
            return None;
        };
        match kind {
            ts_r_ast::MethodKind::Get => return_type,
            ts_r_ast::MethodKind::Set => {
                self.list(params)
                    .first()
                    .and_then(|&p| match *self.ast.node(p) {
                        Node::Param { type_ann, .. } => type_ann,
                        _ => None,
                    })
            }
            _ => None,
        }
    }

    /// The other accessor of the same name and staticness (`None` for computed keys).
    fn paired_accessor(&self, members: NodeList, m: NodeId) -> Option<NodeId> {
        use ts_r_ast::MethodKind as MK;
        let Node::MethodDef {
            kind,
            key,
            modifiers,
            ..
        } = *self.ast.node(m)
        else {
            return None;
        };
        let want = if kind == MK::Get { MK::Set } else { MK::Get };
        let is_static = modifiers.contains(Modifiers::STATIC);
        let name = self.key_atom(key)?;
        self.list(members).into_iter().find(|&o| {
            matches!(*self.ast.node(o), Node::MethodDef { kind: k, key: ok, modifiers: om, .. }
                if k == want
                    && om.contains(Modifiers::STATIC) == is_static
                    && self.key_atom(ok) == Some(name))
        })
    }

    fn key_atom(&self, key: NodeId) -> Option<ts_r_ast::Atom> {
        match *self.ast.node(key) {
            Node::Ident { name } | Node::PrivateName { name } | Node::StringLit { value: name } => {
                Some(name)
            }
            _ => None,
        }
    }

    /// Computed keys inside a type (`{ [K]: T }`, `interface I { [K](): void }`) reference
    /// the value `K`: tsc keeps its import (observed). Ambient code (`declare`) is never
    /// walked, and there tsc elides it.
    fn type_keys(&mut self, t: NodeId, scope: u32) {
        if !self.computed_keys {
            return;
        }
        match *self.ast.node(t) {
            Node::ComputedKey { expr } => self.expr(expr, scope),
            Node::TypeQuery { .. } => {}
            _ => {
                for c in self.ast.children(t) {
                    self.type_keys(c, scope);
                }
            }
        }
    }

    fn key(&mut self, key: NodeId, scope: u32) {
        if let Node::ComputedKey { expr } = *self.ast.node(key) {
            self.expr(expr, scope);
        }
    }

    fn leftmost_entity(&self, mut id: NodeId) -> Option<NodeId> {
        loop {
            match *self.ast.node(id) {
                Node::Ident { .. } => return Some(id),
                Node::QualifiedName { left, .. } => id = left,
                Node::Member { obj, .. } => id = obj,
                _ => return None,
            }
        }
    }

    fn namespace_name(&self, name: NodeId) -> NodeId {
        self.leftmost_entity(name).unwrap_or(name)
    }

    fn namespace_segments(&self, name: NodeId, out: &mut Vec<NodeId>) {
        match *self.ast.node(name) {
            Node::QualifiedName { left, right } => {
                self.namespace_segments(left, out);
                out.push(right);
            }
            _ => out.push(name),
        }
    }

    /// Identifiers exported by a namespace declaration's body (vars, functions, classes,
    /// enums, namespaces).
    fn exported_names(&self, decl: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let Node::Module {
            body: Some(body), ..
        } = *self.ast.node(decl)
        else {
            return out;
        };
        let Node::ModuleBlock { stmts } = *self.ast.node(body) else {
            return out;
        };
        for &s in self.ast.list(stmts) {
            let n = *self.ast.node(s);
            if !n.modifiers().contains(Modifiers::EXPORT) {
                continue;
            }
            match n {
                Node::VarDecl { decls, .. } => {
                    for &d in self.ast.list(decls) {
                        if let Node::VarDeclarator { name, .. } = *self.ast.node(d) {
                            collect_pattern_idents(self.ast, name, &mut out);
                        }
                    }
                }
                Node::FunctionDecl { name: Some(n), .. }
                | Node::ClassDecl { name: Some(n), .. }
                | Node::Enum { name: n, .. } => out.push(n),
                Node::Module { name, .. } => out.push(self.namespace_name(name)),
                Node::ImportEquals { name, .. } => out.push(name),
                _ => {}
            }
        }
        out
    }

    // ---- expressions ----

    fn expr(&mut self, id: NodeId, scope: u32) {
        let n = *self.ast.node(id);
        match n {
            Node::Ident { .. } => self.reference(id, scope),
            Node::Member { obj, .. } => self.expr(obj, scope),
            Node::Property { key, value, .. } => {
                self.key(key, scope);
                self.expr(value, scope);
            }
            Node::ShorthandProp { name, init, .. } => {
                self.reference(name, scope);
                if let Some(i) = init {
                    self.expr(i, scope);
                }
            }
            Node::MethodDef {
                key,
                value,
                decorators,
                ..
            } => {
                self.decorators(decorators, scope);
                self.key(key, scope);
                if let Node::FunctionExpr {
                    params,
                    return_type,
                    body,
                    ..
                } = *self.ast.node(value)
                {
                    self.function(None, params, return_type, body, scope);
                }
            }
            Node::FunctionExpr {
                name,
                params,
                return_type,
                body,
                ..
            } => self.function(name, params, return_type, body, scope),
            Node::Arrow {
                params,
                return_type,
                body,
                ..
            } => self.function(None, params, return_type, Some(body), scope),
            Node::ClassExpr { .. } => self.class(id, scope),
            Node::Call {
                callee,
                args,
                type_args,
                ..
            } => {
                if let Some(t) = type_args {
                    self.type_keys(t, scope);
                }
                if self.ident_is(callee, "require") {
                    self.out.has_cjs_syntax = true;
                }
                self.expr(callee, scope);
                for a in self.list(args) {
                    self.expr(a, scope);
                }
            }
            Node::New {
                callee,
                args,
                type_args,
                ..
            } => {
                if let Some(t) = type_args {
                    self.type_keys(t, scope);
                }
                self.expr(callee, scope);
                for a in self.list(args) {
                    self.expr(a, scope);
                }
            }
            Node::TaggedTemplate { tag, quasi, .. } => {
                self.expr(tag, scope);
                self.expr(quasi, scope);
            }
            Node::As { expr, ty }
            | Node::Satisfies { expr, ty }
            | Node::TypeAssertion { expr, ty } => {
                self.type_keys(ty, scope);
                self.expr(expr, scope);
            }
            Node::Instantiation { expr, type_args } => {
                self.type_keys(type_args, scope);
                self.expr(expr, scope);
            }
            Node::NonNull { expr } => self.expr(expr, scope),
            Node::ObjectPattern { .. }
            | Node::ArrayPattern { .. }
            | Node::AssignPattern { .. }
            | Node::Rest { .. } => self.target(id, scope),
            Node::JsxElement { .. } | Node::JsxFragment { .. } => {
                self.out.has_jsx = true;
                self.jsx(id, scope);
            }
            // `import.meta` makes a file a module, like `import`/`export` (observed).
            Node::MetaProperty {
                kind: ts_r_ast::MetaPropKind::ImportMeta,
            } => self.out.has_module_syntax = true,
            Node::Assign { left, right, .. } => {
                if self.is_cjs_export_target(left) {
                    self.out.has_cjs_syntax = true;
                }
                self.expr(left, scope);
                self.expr(right, scope);
            }
            _ => {
                if crate::is_type_kind(n.kind()) {
                    self.type_keys(id, scope);
                    return;
                }
                let mut children = Vec::new();
                self.ast.for_each_child(id, |c| children.push(c));
                for c in children {
                    self.expr(c, scope);
                }
            }
        }
    }

    fn ident_is(&self, id: NodeId, name: &str) -> bool {
        matches!(*self.ast.node(id), Node::Ident { name: n } if self.ast.atom(n) == name)
    }

    /// `exports.x`, `exports[k]`, `module.exports` and anything below them.
    fn is_cjs_export_target(&self, mut id: NodeId) -> bool {
        loop {
            match *self.ast.node(id) {
                Node::Member { obj, prop, .. } => {
                    if self.ident_is(obj, "exports")
                        || (self.ident_is(obj, "module") && self.ident_is(prop, "exports"))
                    {
                        return true;
                    }
                    id = obj;
                }
                Node::Index { obj, .. } => {
                    if self.ident_is(obj, "exports") {
                        return true;
                    }
                    id = obj;
                }
                Node::Paren { expr } => id = expr,
                _ => return false,
            }
        }
    }

    /// Patterns used as assignment targets reference (not declare) their names.
    fn target(&mut self, id: NodeId, scope: u32) {
        match *self.ast.node(id) {
            Node::Ident { .. } => self.reference(id, scope),
            Node::ObjectPattern { props } => {
                for p in self.list(props) {
                    self.target(p, scope);
                }
            }
            Node::ArrayPattern { elements } => {
                for e in self.list(elements) {
                    self.target(e, scope);
                }
            }
            Node::BindingProperty { key, value, init } => {
                self.key(key, scope);
                match value {
                    Some(v) => self.target(v, scope),
                    None => self.target(key, scope),
                }
                if let Some(i) = init {
                    self.expr(i, scope);
                }
            }
            Node::AssignPattern { left, right } => {
                self.target(left, scope);
                self.expr(right, scope);
            }
            Node::Rest { arg } => self.target(arg, scope),
            _ => self.expr(id, scope),
        }
    }

    fn jsx(&mut self, id: NodeId, scope: u32) {
        if matches!(
            self.ast.node(id),
            Node::JsxElement { .. } | Node::JsxFragment { .. }
        ) {
            self.out.jsx_scope.insert(id, scope);
        }
        match *self.ast.node(id) {
            Node::JsxElement {
                opening, children, ..
            } => {
                if let Node::JsxOpening { name, attrs, .. } = *self.ast.node(opening) {
                    self.jsx_tag(name, scope);
                    for a in self.list(attrs) {
                        match *self.ast.node(a) {
                            Node::JsxAttr { value: Some(v), .. } => self.jsx(v, scope),
                            Node::JsxSpreadAttr { arg } => self.expr(arg, scope),
                            _ => {}
                        }
                    }
                }
                for c in self.list(children) {
                    self.jsx(c, scope);
                }
            }
            Node::JsxFragment { children } => {
                for c in self.list(children) {
                    self.jsx(c, scope);
                }
            }
            Node::JsxExprContainer { expr: Some(e) } | Node::JsxSpreadChild { expr: e } => {
                self.expr(e, scope)
            }
            Node::JsxText {} | Node::JsxExprContainer { expr: None } | Node::StringLit { .. } => {}
            _ => self.expr(id, scope),
        }
    }

    fn jsx_tag(&mut self, name: NodeId, scope: u32) {
        match *self.ast.node(name) {
            Node::Ident { name: a } => {
                if !is_intrinsic_tag(self.ast.atom(a)) {
                    self.reference(name, scope);
                }
            }
            Node::JsxMemberExpr { obj, .. } | Node::Member { obj, .. } => self.jsx_tag(obj, scope),
            _ => {}
        }
    }
}

/// `div`, `my-element`: lowercase first letter or a dash means an intrinsic element.
pub(crate) fn is_intrinsic_tag(name: &str) -> bool {
    name.starts_with(|c: char| c.is_ascii_lowercase()) || name.contains('-')
}

pub(crate) fn collect_pattern_idents(ast: &Ast, id: NodeId, out: &mut Vec<NodeId>) {
    match *ast.node(id) {
        Node::Ident { .. } => out.push(id),
        Node::ObjectPattern { props } => {
            for &p in ast.list(props) {
                collect_pattern_idents(ast, p, out);
            }
        }
        Node::ArrayPattern { elements } => {
            for &e in ast.list(elements) {
                collect_pattern_idents(ast, e, out);
            }
        }
        Node::BindingProperty { key, value, .. } => match value {
            Some(v) => collect_pattern_idents(ast, v, out),
            None => collect_pattern_idents(ast, key, out),
        },
        Node::AssignPattern { left, .. } => collect_pattern_idents(ast, left, out),
        Node::Rest { arg } => collect_pattern_idents(ast, arg, out),
        _ => {}
    }
}
