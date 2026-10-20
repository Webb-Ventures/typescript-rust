//! TEMPORARY lint-local scope analysis (until `ts_r_binder` exists). See the module docs of
//! [`crate::semantic`]. One recursive walk declares symbols and records references; a second
//! step resolves every reference against the finished scope tree, so hoisting needs no special
//! handling.

use std::sync::Arc;

use rustc_hash::FxHashMap;
use ts_r_ast::{
    Ast, Atom, HeritageKind, Modifiers, ModuleKind, Node, NodeId, NodeKind, ParentMap, SourceKind,
    UnaryOp, VarKind,
};
use ts_r_span::Span;

use super::{
    Decl, DeclKind, RefFlags, RefId, Reference, Resolution, Scope, ScopeId, ScopeKind, Semantic,
    Symbol, SymbolFlags, SymbolId,
};
use crate::env::{Globals, LintEnv, SourceType};

/// A variable declared by a `/* global name */` comment.
#[derive(Clone, Debug)]
pub struct CommentGlobal {
    pub name: String,
    pub writable: bool,
    /// `name: off`: removes a configured global instead of declaring one.
    pub off: bool,
    /// The comment's span.
    pub span: Span,
}

/// Maximum nesting the builder follows; deeper subtrees are skipped (the parser already
/// bounds nesting, this is a second guard for the native stack).
const MAX_DEPTH: u32 = 1500;

/// Scope analysis result. See [`Semantic`].
#[derive(Debug)]
pub struct LocalSemantic<'a> {
    ast: &'a Ast,
    scopes: Vec<Scope>,
    symbols: Vec<Symbol>,
    refs: Vec<Reference>,
    decl_map: FxHashMap<NodeId, SymbolId>,
    ref_map: FxHashMap<NodeId, RefId>,
    node_scope: FxHashMap<NodeId, ScopeId>,
    names: FxHashMap<(ScopeId, Atom), SymbolId>,
    /// Innermost scope per node id.
    scope_at: Vec<u32>,
    globals: Arc<Globals>,
    /// TypeScript lib globals (typescript-eslint scope analysis only).
    lib: Option<Arc<crate::lib_globals::LibGlobals>>,
    /// Names of comment globals that are not atoms of the file (never referenced).
    extra_names: Vec<String>,
}

const UNSET: u32 = u32::MAX;

impl<'a> LocalSemantic<'a> {
    pub fn build(
        ast: &'a Ast,
        parents: &ParentMap,
        env: &LintEnv,
        comment_globals: &[CommentGlobal],
    ) -> LocalSemantic<'a> {
        let mut b = Builder {
            ast,
            sem: LocalSemantic {
                ast,
                scopes: Vec::new(),
                symbols: Vec::new(),
                refs: Vec::new(),
                decl_map: FxHashMap::default(),
                ref_map: FxHashMap::default(),
                node_scope: FxHashMap::default(),
                names: FxHashMap::default(),
                scope_at: vec![UNSET; ast.nodes.len()],
                globals: if comment_globals.iter().any(|g| g.off) {
                    let mut g = (*env.globals).clone();
                    for c in comment_globals.iter().filter(|c| c.off) {
                        g.insert(&c.name, crate::env::GlobalAccess::Off);
                    }
                    Arc::new(g)
                } else {
                    env.globals.clone()
                },
                lib: env.lib.clone(),
                extra_names: Vec::new(),
            },
            cur: ScopeId(0),
            depth: 0,
            ambient: ast.source_kind == SourceKind::Dts,
            first_jsx: None,
            first_fragment: None,
        };
        b.run(env, comment_globals);
        let mut sem = b.sem;
        sem.resolve_all();
        sem.fill_scope_at(parents);
        sem
    }

    fn resolve_all(&mut self) {
        let mut implicit_args: FxHashMap<ScopeId, SymbolId> = FxHashMap::default();
        let arguments = self.ast.atoms.find("arguments");
        for i in 0..self.refs.len() {
            let r = self.refs[i];
            let res = self.resolve(r.scope, r.name, r.flags, arguments, &mut implicit_args);
            self.refs[i].resolution = res;
            if let Resolution::Symbol(s) = res {
                self.symbols[s.0 as usize].refs.push(RefId(i as u32));
            }
        }
    }

    fn resolve(
        &mut self,
        from: ScopeId,
        name: Atom,
        flags: RefFlags,
        arguments: Option<Atom>,
        implicit_args: &mut FxHashMap<ScopeId, SymbolId>,
    ) -> Resolution {
        let want_type = flags.contains(RefFlags::TYPE);
        let any = flags.contains(RefFlags::EXPORT);
        let mut s = Some(from);
        while let Some(sc) = s {
            if let Some(&sym) = self.names.get(&(sc, name)) {
                let f = self.symbols[sym.0 as usize].flags;
                let ok = if any {
                    true
                } else if want_type {
                    f.intersects(SymbolFlags::TYPE)
                } else {
                    f.intersects(SymbolFlags::VALUE)
                };
                if ok {
                    return Resolution::Symbol(sym);
                }
            }
            let scope = &self.scopes[sc.0 as usize];
            if !want_type
                && Some(name) == arguments
                && scope.kind == ScopeKind::Function
                && !matches!(
                    self.ast.node(scope.node),
                    Node::Arrow { .. } | Node::Program { .. }
                )
            {
                let sym = *implicit_args.entry(sc).or_insert_with(|| {
                    let id = SymbolId(self.symbols.len() as u32);
                    self.symbols.push(Symbol {
                        name,
                        scope: sc,
                        flags: SymbolFlags::ARGUMENTS,
                        decls: Vec::new(),
                        refs: Vec::new(),
                        comment_span: None,
                    });
                    id
                });
                return Resolution::Symbol(sym);
            }
            s = scope.parent;
        }
        let text = self.ast.atom(name);
        // Configured globals satisfy type and value references alike. Without the TypeScript
        // lib (not typescript-eslint's scope analysis) type references always resolve.
        let found = self.globals.contains(text)
            || match &self.lib {
                Some(lib) if want_type => lib.resolves_type(text),
                Some(lib) => lib.resolves_value(text),
                None => want_type,
            };
        if found {
            Resolution::Global
        } else {
            Resolution::Unresolved
        }
    }

    fn fill_scope_at(&mut self, parents: &ParentMap) {
        // Parents have larger ids than children, so a descending pass sees parents first.
        for i in (1..self.scope_at.len()).rev() {
            if self.scope_at[i] != UNSET {
                continue;
            }
            let Some(id) = NodeId::from_raw(i as u32) else {
                continue;
            };
            self.scope_at[i] = match parents.parent(id) {
                Some(p) => self.scope_at[p.index()],
                None => 0,
            };
            if self.scope_at[i] == UNSET {
                self.scope_at[i] = 0;
            }
        }
    }

    /// The name of a symbol.
    pub fn name(&self, sym: SymbolId) -> &str {
        let a = self.symbols[sym.0 as usize].name;
        if a.0 >= EXTRA_BASE {
            return self
                .extra_names
                .get((a.0 - EXTRA_BASE) as usize)
                .map_or("", |s| s.as_str());
        }
        self.ast.atom(a)
    }
}

const EXTRA_BASE: u32 = u32::MAX / 2;

impl Semantic for LocalSemantic<'_> {
    fn scopes(&self) -> &[Scope] {
        &self.scopes
    }
    fn symbols(&self) -> &[Symbol] {
        &self.symbols
    }
    fn references(&self) -> &[Reference] {
        &self.refs
    }
    fn symbol_of_decl(&self, ident: NodeId) -> Option<SymbolId> {
        self.decl_map.get(&ident).copied()
    }
    fn reference_at(&self, ident: NodeId) -> Option<RefId> {
        self.ref_map.get(&ident).copied()
    }
    fn scope_of_node(&self, node: NodeId) -> Option<ScopeId> {
        self.node_scope.get(&node).copied()
    }
    fn enclosing_scope(&self, node: NodeId) -> ScopeId {
        match self.scope_at.get(node.index()) {
            Some(&s) if s != UNSET => ScopeId(s),
            _ => ScopeId(0),
        }
    }
    fn lookup(&self, scope: ScopeId, name: &str) -> Option<SymbolId> {
        let atom = self.ast.atoms.find(name);
        let mut s = Some(scope);
        while let Some(sc) = s {
            if let Some(a) = atom
                && let Some(&sym) = self.names.get(&(sc, a))
            {
                return Some(sym);
            }
            s = self.scopes[sc.0 as usize].parent;
        }
        None
    }
    fn is_global(&self, name: &str) -> bool {
        self.globals.contains(name) || self.lib.as_ref().is_some_and(|l| l.resolves_value(name))
    }
    fn name(&self, sym: SymbolId) -> &str {
        LocalSemantic::name(self, sym)
    }
}

struct Builder<'a> {
    ast: &'a Ast,
    sem: LocalSemantic<'a>,
    cur: ScopeId,
    depth: u32,
    ambient: bool,
    first_jsx: Option<NodeId>,
    first_fragment: Option<NodeId>,
}

impl<'a> Builder<'a> {
    fn run(&mut self, env: &LintEnv, comment_globals: &[CommentGlobal]) {
        let root = self.ast.root;
        let global = self.push_scope(ScopeKind::Global, root);
        for g in comment_globals.iter().filter(|g| !g.off) {
            let atom = match self.ast.atoms.find(&g.name) {
                Some(a) => a,
                None => {
                    let a = Atom(EXTRA_BASE + self.sem.extra_names.len() as u32);
                    self.sem.extra_names.push(g.name.clone());
                    a
                }
            };
            let sym = self.symbol_for(global, atom);
            let s = &mut self.sem.symbols[sym.0 as usize];
            s.flags.insert(SymbolFlags::COMMENT_GLOBAL);
            s.comment_span = Some(g.span);
            s.decls.push(Decl {
                kind: DeclKind::CommentGlobal,
                name: root,
                node: root,
                parent: root,
            });
        }
        match env.source_type {
            SourceType::Module => {
                self.push_scope(ScopeKind::Module, root);
            }
            SourceType::CommonJs => {
                self.push_scope(ScopeKind::Function, root);
            }
            SourceType::Script if env.global_return => {
                self.push_scope(ScopeKind::Function, root);
            }
            SourceType::Script => {}
        }
        self.sem.node_scope.insert(root, self.cur);
        if let Node::Program { stmts } = *self.ast.node(root) {
            for &s in self.ast.list(stmts) {
                self.visit(s);
            }
        }
        let implicit = [
            (&env.jsx_pragma, self.first_jsx),
            (&env.jsx_fragment_name, self.first_fragment),
        ];
        for (name, node) in implicit {
            if let (Some(name), Some(jsx)) = (name, node)
                && let Some(atom) = self.ast.atoms.find(name)
            {
                let scope = self.sem.scope_at[jsx.index()];
                self.sem.refs.push(Reference {
                    node: jsx,
                    name: atom,
                    scope: ScopeId(if scope == UNSET { 0 } else { scope }),
                    flags: RefFlags::READ | RefFlags::JSX | RefFlags::IMPLICIT,
                    resolution: Resolution::Unresolved,
                    write_value: None,
                });
            }
        }
    }

    // ---- scopes and symbols ----

    fn push_scope(&mut self, kind: ScopeKind, node: NodeId) -> ScopeId {
        let id = ScopeId(self.sem.scopes.len() as u32);
        let parent = if self.sem.scopes.is_empty() {
            None
        } else {
            Some(self.cur)
        };
        let dynamic = kind == ScopeKind::With
            || parent.is_some_and(|p| self.sem.scopes[p.0 as usize].dynamic);
        self.sem.scopes.push(Scope {
            kind,
            node,
            parent,
            symbols: Vec::new(),
            dynamic,
        });
        self.sem.node_scope.entry(node).or_insert(id);
        self.cur = id;
        id
    }

    fn pop_scope(&mut self) {
        if let Some(p) = self.sem.scopes[self.cur.0 as usize].parent {
            self.cur = p;
        }
    }

    fn var_scope(&self) -> ScopeId {
        let mut s = self.cur;
        loop {
            let sc = &self.sem.scopes[s.0 as usize];
            if sc.kind.is_var_scope() {
                return s;
            }
            match sc.parent {
                Some(p) => s = p,
                None => return s,
            }
        }
    }

    fn symbol_for(&mut self, scope: ScopeId, name: Atom) -> SymbolId {
        if let Some(&s) = self.sem.names.get(&(scope, name)) {
            return s;
        }
        let id = SymbolId(self.sem.symbols.len() as u32);
        self.sem.symbols.push(Symbol {
            name,
            scope,
            flags: SymbolFlags::NONE,
            decls: Vec::new(),
            refs: Vec::new(),
            comment_span: None,
        });
        self.sem.scopes[scope.0 as usize].symbols.push(id);
        self.sem.names.insert((scope, name), id);
        id
    }

    fn declare(
        &mut self,
        scope: ScopeId,
        ident: NodeId,
        mut flags: SymbolFlags,
        kind: DeclKind,
        node: NodeId,
        parent: NodeId,
    ) -> Option<SymbolId> {
        let name = match *self.ast.node(ident) {
            Node::Ident { name } => name,
            _ => return None,
        };
        if self.ambient {
            flags.insert(SymbolFlags::AMBIENT);
        }
        let sym = self.symbol_for(scope, name);
        let s = &mut self.sem.symbols[sym.0 as usize];
        s.flags.insert(flags);
        s.decls.push(Decl {
            kind,
            name: ident,
            node,
            parent,
        });
        self.sem.decl_map.insert(ident, sym);
        self.mark(ident);
        Some(sym)
    }

    fn add_ref(&mut self, ident: NodeId, flags: RefFlags, value: Option<NodeId>) {
        let name = match *self.ast.node(ident) {
            Node::Ident { name } => name,
            _ => return,
        };
        self.mark(ident);
        let id = RefId(self.sem.refs.len() as u32);
        self.sem.refs.push(Reference {
            node: ident,
            name,
            scope: self.cur,
            flags,
            resolution: Resolution::Unresolved,
            write_value: value,
        });
        self.sem.ref_map.insert(ident, id);
    }

    #[inline]
    fn mark(&mut self, id: NodeId) {
        if let Some(slot) = self.sem.scope_at.get_mut(id.index()) {
            *slot = self.cur.0;
        }
    }

    // ---- statements and expressions ----

    fn visit_opt(&mut self, id: Option<NodeId>) {
        if let Some(id) = id {
            self.visit(id);
        }
    }

    fn visit_list(&mut self, list: ts_r_ast::NodeList) {
        for &c in self.ast.list(list) {
            self.visit(c);
        }
    }

    fn visit(&mut self, id: NodeId) {
        if self.depth > MAX_DEPTH {
            return;
        }
        self.depth += 1;
        self.visit_inner(id);
        self.depth -= 1;
    }

    fn visit_inner(&mut self, id: NodeId) {
        self.mark(id);
        let ast = self.ast;
        match *ast.node(id) {
            Node::Ident { .. } => self.add_ref(id, RefFlags::READ, None),
            Node::Block { stmts } => {
                self.push_scope(ScopeKind::Block, id);
                self.visit_list(stmts);
                self.pop_scope();
            }
            Node::VarDecl {
                modifiers,
                kind,
                decls,
            } => {
                let was = self.ambient;
                if modifiers.contains(Modifiers::DECLARE) {
                    self.ambient = true;
                }
                for &d in ast.list(decls) {
                    self.visit_declarator(d, id, kind, modifiers, false);
                }
                self.ambient = was;
            }
            Node::FunctionDecl {
                modifiers,
                name,
                body,
                ..
            } => {
                let was = self.ambient;
                if modifiers.contains(Modifiers::DECLARE) {
                    self.ambient = true;
                }
                if let Some(n) = name {
                    let mut f = SymbolFlags::FUNCTION;
                    if modifiers.contains(Modifiers::EXPORT) {
                        f.insert(SymbolFlags::EXPORTED);
                    }
                    if body.is_none() && !self.ambient {
                        // overload signature: still the same symbol
                    }
                    let scope = self.cur;
                    self.declare(scope, n, f, DeclKind::Function, id, id);
                }
                self.visit_function(id);
                self.ambient = was;
            }
            Node::FunctionExpr { name, .. } => {
                if let Some(n) = name {
                    let s = self.push_scope(ScopeKind::FunctionExprName, id);
                    self.declare(
                        s,
                        n,
                        SymbolFlags::FN_EXPR_NAME,
                        DeclKind::FunctionExprName,
                        id,
                        id,
                    );
                    self.visit_function(id);
                    self.pop_scope();
                } else {
                    self.visit_function(id);
                }
            }
            Node::Arrow { .. } => self.visit_function(id),
            Node::ClassDecl {
                decorators,
                modifiers,
                name,
                ..
            } => {
                self.visit_list(decorators);
                let was = self.ambient;
                if modifiers.contains(Modifiers::DECLARE) {
                    self.ambient = true;
                }
                if let Some(n) = name {
                    let mut f = SymbolFlags::CLASS;
                    if modifiers.contains(Modifiers::EXPORT) {
                        f.insert(SymbolFlags::EXPORTED);
                    }
                    let scope = self.cur;
                    self.declare(scope, n, f, DeclKind::Class, id, id);
                }
                self.visit_class(id);
                self.ambient = was;
            }
            Node::ClassExpr {
                decorators, name, ..
            } => {
                self.visit_list(decorators);
                if let Some(n) = name {
                    let s = self.push_scope(ScopeKind::ClassExprName, id);
                    self.declare(
                        s,
                        n,
                        SymbolFlags::CLASS_EXPR_NAME,
                        DeclKind::ClassExprName,
                        id,
                        id,
                    );
                    self.visit_class(id);
                    self.pop_scope();
                } else {
                    self.visit_class(id);
                }
            }
            Node::Assign { left, op, right } => {
                let mut f = RefFlags::WRITE;
                if op != ts_r_ast::AssignOp::Assign {
                    f = f | RefFlags::READ | RefFlags::COMPOUND;
                }
                let value = (op == ts_r_ast::AssignOp::Assign).then_some(right);
                self.visit_target(left, f, value);
                self.visit(right);
            }
            Node::Update { arg, .. } => {
                self.visit_target(
                    arg,
                    RefFlags::READ | RefFlags::WRITE | RefFlags::COMPOUND,
                    None,
                );
            }
            Node::Unary {
                op: UnaryOp::TypeOf,
                arg,
            } => {
                let inner = skip_parens(ast, arg);
                if matches!(ast.node(inner), Node::Ident { .. }) {
                    self.mark(arg);
                    self.add_ref(inner, RefFlags::READ | RefFlags::TYPEOF_EXPR, None);
                } else {
                    self.visit(arg);
                }
            }
            Node::Member { obj, prop, .. } => {
                self.visit(obj);
                self.mark(prop);
            }
            Node::Property { key, value, .. } => {
                self.visit_key(key);
                self.visit(value);
            }
            Node::ShorthandProp { name, init, .. } => {
                self.add_ref(name, RefFlags::READ, None);
                self.visit_opt(init);
            }
            Node::MethodDef {
                decorators,
                key,
                value,
                ..
            } => {
                self.visit_list(decorators);
                self.visit_key(key);
                self.mark(value);
                self.visit_function(value);
            }
            Node::PropertyDef {
                decorators,
                key,
                type_ann,
                value,
                ..
            } => {
                self.visit_list(decorators);
                self.visit_key(key);
                if let Some(t) = type_ann {
                    self.visit_type(t);
                }
                if let Some(v) = value {
                    self.push_scope(ScopeKind::ClassFieldInit, v);
                    self.visit(v);
                    self.pop_scope();
                }
            }
            Node::StaticBlock { body } => {
                self.push_scope(ScopeKind::StaticBlock, id);
                self.visit_list(body);
                self.pop_scope();
            }
            Node::Labeled { label, body } => {
                self.mark(label);
                self.visit(body);
            }
            Node::Break { label } | Node::Continue { label } => {
                if let Some(l) = label {
                    self.mark(l);
                }
            }
            Node::For {
                init,
                test,
                update,
                body,
            } => {
                let scoped = init.is_some_and(
                    |i| matches!(ast.node(i), Node::VarDecl { kind, .. } if *kind != VarKind::Var),
                );
                if scoped {
                    self.push_scope(ScopeKind::For, id);
                }
                self.visit_opt(init);
                self.visit_opt(test);
                self.visit_opt(update);
                self.visit(body);
                if scoped {
                    self.pop_scope();
                }
            }
            Node::ForIn { left, right, body }
            | Node::ForOf {
                left, right, body, ..
            } => {
                let decl = match *ast.node(left) {
                    Node::VarDecl {
                        kind, modifiers, ..
                    } => Some((kind, modifiers)),
                    _ => None,
                };
                let scoped = decl.is_some_and(|(k, _)| k != VarKind::Var);
                if scoped {
                    self.push_scope(ScopeKind::For, id);
                }
                // The iterated expression is evaluated before the bindings exist, but for
                // reference resolution the order does not matter.
                match decl {
                    Some((kind, modifiers)) => {
                        self.mark(left);
                        if let Node::VarDecl { decls, .. } = *ast.node(left) {
                            for &d in ast.list(decls) {
                                self.visit_declarator(d, left, kind, modifiers, true);
                            }
                        }
                    }
                    None => self.visit_target(left, RefFlags::WRITE, None),
                }
                self.visit(right);
                self.visit(body);
                if scoped {
                    self.pop_scope();
                }
            }
            Node::Switch { disc, cases } => {
                self.visit(disc);
                self.push_scope(ScopeKind::Switch, id);
                self.visit_list(cases);
                self.pop_scope();
            }
            Node::CatchClause {
                param,
                type_ann,
                body,
            } => {
                let scope = self.push_scope(ScopeKind::Catch, id);
                if let Some(p) = param {
                    self.declare_pattern(
                        p,
                        scope,
                        SymbolFlags::CATCH_PARAM,
                        DeclKind::CatchParam,
                        id,
                        id,
                        false,
                        None,
                    );
                }
                if let Some(t) = type_ann {
                    self.visit_type(t);
                }
                self.visit(body);
                self.pop_scope();
            }
            Node::With { obj, body } => {
                self.visit(obj);
                self.push_scope(ScopeKind::With, id);
                self.visit(body);
                self.pop_scope();
            }
            Node::ImportDecl {
                type_only,
                specifiers,
                source,
                ..
            } => {
                self.mark(source);
                for &s in ast.list(specifiers) {
                    self.mark(s);
                    let (local, kind, spec_type) = match *ast.node(s) {
                        Node::ImportDefaultSpecifier { local } => {
                            (local, DeclKind::ImportDefault, false)
                        }
                        Node::ImportNamespaceSpecifier { local } => {
                            (local, DeclKind::ImportNamespace, false)
                        }
                        Node::ImportSpecifier {
                            type_only,
                            local,
                            imported,
                        } => {
                            if let Some(i) = imported {
                                self.mark(i);
                            }
                            (local, DeclKind::ImportNamed, type_only)
                        }
                        _ => continue,
                    };
                    let mut f = SymbolFlags::IMPORT;
                    if type_only || spec_type {
                        f.insert(SymbolFlags::TYPE_IMPORT);
                    }
                    let scope = self.cur;
                    self.declare(scope, local, f, kind, s, id);
                }
            }
            Node::ImportEquals {
                modifiers,
                type_only,
                name,
                module_ref,
            } => {
                let mut f = SymbolFlags::IMPORT_EQUALS;
                if modifiers.contains(Modifiers::EXPORT) {
                    f.insert(SymbolFlags::EXPORTED);
                }
                if type_only {
                    f.insert(SymbolFlags::TYPE_IMPORT);
                }
                let scope = self.cur;
                self.declare(scope, name, f, DeclKind::ImportEquals, id, id);
                match *ast.node(module_ref) {
                    Node::ExternalModuleRef { .. } => self.mark(module_ref),
                    _ => {
                        if let Some(root) = qualified_root(ast, module_ref) {
                            self.add_ref(root, RefFlags::READ, None);
                        }
                    }
                }
            }
            Node::ExportNamed {
                specifiers, source, ..
            } => {
                for &s in ast.list(specifiers) {
                    self.mark(s);
                    if let Node::ExportSpecifier {
                        local, exported, ..
                    } = *ast.node(s)
                    {
                        if let Some(e) = exported {
                            self.mark(e);
                        }
                        if source.is_none() {
                            self.add_ref(local, RefFlags::READ | RefFlags::EXPORT, None);
                        } else {
                            self.mark(local);
                        }
                    }
                }
            }
            Node::ExportDefaultExpr { expr } | Node::ExportAssignment { expr } => {
                let inner = skip_parens(ast, expr);
                if matches!(ast.node(inner), Node::Ident { .. }) {
                    self.mark(expr);
                    self.add_ref(inner, RefFlags::READ | RefFlags::EXPORT, None);
                } else {
                    self.visit(expr);
                }
            }
            Node::ExportAll { .. } | Node::NamespaceExport { .. } => {}
            Node::Interface {
                modifiers,
                name,
                type_params,
                extends,
                body,
            } => {
                let mut f = SymbolFlags::INTERFACE;
                if modifiers.contains(Modifiers::EXPORT) {
                    f.insert(SymbolFlags::EXPORTED);
                }
                let was = self.ambient;
                if modifiers.contains(Modifiers::DECLARE) {
                    self.ambient = true;
                }
                let scope = self.cur;
                self.declare(scope, name, f, DeclKind::Interface, id, id);
                self.push_scope(ScopeKind::Type, id);
                self.declare_type_params(type_params);
                for &e in ast.list(extends) {
                    self.visit_type(e);
                }
                for &m in ast.list(body) {
                    self.visit_type(m);
                }
                self.pop_scope();
                self.ambient = was;
            }
            Node::TypeAlias {
                modifiers,
                name,
                type_params,
                ty,
            } => {
                let mut f = SymbolFlags::TYPE_ALIAS;
                if modifiers.contains(Modifiers::EXPORT) {
                    f.insert(SymbolFlags::EXPORTED);
                }
                let scope = self.cur;
                self.declare(scope, name, f, DeclKind::TypeAlias, id, id);
                self.push_scope(ScopeKind::Type, id);
                self.declare_type_params(type_params);
                self.visit_type(ty);
                self.pop_scope();
            }
            Node::Enum {
                modifiers,
                name,
                members,
            } => {
                let mut f = SymbolFlags::ENUM;
                if modifiers.contains(Modifiers::EXPORT) {
                    f.insert(SymbolFlags::EXPORTED);
                }
                let was = self.ambient;
                if modifiers.contains(Modifiers::DECLARE) {
                    self.ambient = true;
                }
                let scope = self.cur;
                self.declare(scope, name, f, DeclKind::Enum, id, id);
                let es = self.push_scope(ScopeKind::TsEnum, id);
                for &m in ast.list(members) {
                    self.mark(m);
                    if let Node::EnumMember { name, .. } = *ast.node(m) {
                        self.declare(
                            es,
                            name,
                            SymbolFlags::ENUM_MEMBER,
                            DeclKind::EnumMember,
                            m,
                            id,
                        );
                        self.mark(name);
                    }
                }
                for &m in ast.list(members) {
                    if let Node::EnumMember { init: Some(i), .. } = *ast.node(m) {
                        self.visit(i);
                    }
                }
                self.pop_scope();
                self.ambient = was;
            }
            Node::Module {
                modifiers,
                kind,
                name,
                body,
            } => {
                let was = self.ambient;
                if modifiers.contains(Modifiers::DECLARE) {
                    self.ambient = true;
                }
                self.mark(name);
                if kind != ModuleKind::Global {
                    let root = match *ast.node(name) {
                        Node::Ident { .. } => Some(name),
                        Node::QualifiedName { .. } => qualified_root(ast, name),
                        _ => None,
                    };
                    if let Some(r) = root {
                        let mut f = SymbolFlags::NAMESPACE;
                        if modifiers.contains(Modifiers::EXPORT) {
                            f.insert(SymbolFlags::EXPORTED);
                        }
                        let scope = self.cur;
                        self.declare(scope, r, f, DeclKind::Namespace, id, id);
                    }
                }
                if let Some(b) = body {
                    self.mark(b);
                    self.push_scope(ScopeKind::TsModule, id);
                    if let Node::ModuleBlock { stmts } = *ast.node(b) {
                        self.visit_list(stmts);
                    }
                    self.pop_scope();
                }
                self.ambient = was;
            }
            Node::JsxElement { .. } | Node::JsxFragment { .. } => {
                if self.first_jsx.is_none() {
                    self.first_jsx = Some(id);
                }
                if self.first_fragment.is_none() && matches!(ast.node(id), Node::JsxFragment { .. })
                {
                    self.first_fragment = Some(id);
                }
                ast.for_each_child(id, |c| self.visit(c));
            }
            Node::JsxOpening {
                name,
                type_args,
                attrs,
                ..
            } => {
                self.visit_jsx_name(name);
                if let Some(t) = type_args {
                    self.visit_type(t);
                }
                self.visit_list(attrs);
            }
            Node::JsxClosing { name } => self.mark(name),
            Node::JsxAttr { name, value } => {
                self.mark(name);
                self.visit_opt(value);
            }
            Node::As { expr, ty } | Node::Satisfies { expr, ty } => {
                self.visit(expr);
                self.visit_type(ty);
            }
            Node::TypeAssertion { ty, expr } => {
                self.visit_type(ty);
                self.visit(expr);
            }
            Node::Call {
                callee,
                type_args,
                args,
                ..
            }
            | Node::New {
                callee,
                type_args,
                args,
                ..
            } => {
                self.visit(callee);
                if let Some(t) = type_args {
                    self.visit_type(t);
                }
                self.visit_list(args);
            }
            Node::TaggedTemplate {
                tag,
                type_args,
                quasi,
            } => {
                self.visit(tag);
                if let Some(t) = type_args {
                    self.visit_type(t);
                }
                self.visit(quasi);
            }
            Node::Instantiation { expr, type_args } => {
                self.visit(expr);
                self.visit_type(type_args);
            }
            Node::MetaProperty { .. } | Node::PrivateName { .. } => {}
            Node::ImportCall { arg, options, .. } => {
                self.visit(arg);
                self.visit_opt(options);
            }
            Node::Decorator { expr } => self.visit(expr),
            _ => {
                if is_type_kind(ast.kind(id)) {
                    self.visit_type(id);
                } else {
                    ast.for_each_child(id, |c| self.visit(c));
                }
            }
        }
    }

    fn visit_key(&mut self, key: NodeId) {
        self.mark(key);
        if let Node::ComputedKey { expr } = *self.ast.node(key) {
            self.visit(expr);
        }
    }

    fn visit_jsx_name(&mut self, name: NodeId) {
        self.mark(name);
        match *self.ast.node(name) {
            Node::Ident { name: n } => {
                let s = self.ast.atom(n);
                let first = s.chars().next().unwrap_or('a');
                if first.is_uppercase() || first == '_' || first == '$' || s.contains('.') {
                    self.add_ref(name, RefFlags::READ | RefFlags::JSX, None);
                }
            }
            Node::JsxMemberExpr { obj, prop } => {
                self.mark(prop);
                match *self.ast.node(obj) {
                    Node::Ident { .. } => self.add_ref(obj, RefFlags::READ | RefFlags::JSX, None),
                    _ => self.visit_jsx_name_member(obj),
                }
            }
            Node::This {} => {}
            _ => {}
        }
    }

    fn visit_jsx_name_member(&mut self, obj: NodeId) {
        self.mark(obj);
        if let Node::JsxMemberExpr { obj: o, prop } = *self.ast.node(obj) {
            self.mark(prop);
            match *self.ast.node(o) {
                Node::Ident { .. } => self.add_ref(o, RefFlags::READ | RefFlags::JSX, None),
                _ => self.visit_jsx_name_member(o),
            }
        }
    }

    fn visit_declarator(
        &mut self,
        d: NodeId,
        decl: NodeId,
        kind: VarKind,
        modifiers: Modifiers,
        loop_binding: bool,
    ) {
        self.mark(d);
        let Node::VarDeclarator {
            name,
            type_ann,
            init,
            ..
        } = *self.ast.node(d)
        else {
            return;
        };
        let (mut f, dk) = match kind {
            VarKind::Var => (SymbolFlags::VAR, DeclKind::Var),
            VarKind::Let => (SymbolFlags::LET, DeclKind::Let),
            VarKind::Const => (SymbolFlags::CONST, DeclKind::Const),
            VarKind::Using | VarKind::AwaitUsing => (SymbolFlags::USING, DeclKind::Using),
        };
        if modifiers.contains(Modifiers::EXPORT) {
            f.insert(SymbolFlags::EXPORTED);
        }
        let scope = if kind == VarKind::Var {
            self.var_scope()
        } else {
            self.cur
        };
        let has_init = init.is_some() || loop_binding;
        self.declare_pattern(name, scope, f, dk, d, decl, has_init, init);
        if let Some(t) = type_ann {
            self.visit_type(t);
        }
        self.visit_opt(init);
    }

    /// Declares every binding in `pat`. With `init`, each binding also gets an initialising
    /// write reference (as eslint-scope does).
    #[allow(clippy::too_many_arguments)]
    fn declare_pattern(
        &mut self,
        pat: NodeId,
        scope: ScopeId,
        flags: SymbolFlags,
        kind: DeclKind,
        node: NodeId,
        parent: NodeId,
        has_init: bool,
        value: Option<NodeId>,
    ) {
        if self.depth > MAX_DEPTH {
            return;
        }
        self.depth += 1;
        self.mark(pat);
        let ast = self.ast;
        match *ast.node(pat) {
            Node::Ident { .. } => {
                self.declare(scope, pat, flags, kind, node, parent);
                if has_init {
                    self.add_ref(pat, RefFlags::WRITE | RefFlags::INIT, value);
                }
            }
            Node::ObjectPattern { props } => {
                let items = ast.list(props);
                let has_rest = items
                    .iter()
                    .any(|&p| matches!(ast.node(p), Node::Rest { .. }));
                for &p in items {
                    self.mark(p);
                    match *ast.node(p) {
                        Node::BindingProperty {
                            key,
                            value: v,
                            init,
                        } => {
                            self.visit_key(key);
                            let target = v.unwrap_or(key);
                            let mut f = flags;
                            if has_rest {
                                f.insert(SymbolFlags::HAS_REST_SIBLING);
                            }
                            self.declare_pattern(
                                target,
                                scope,
                                f,
                                kind,
                                node,
                                parent,
                                has_init || init.is_some(),
                                None,
                            );
                            self.visit_opt(init);
                        }
                        Node::Rest { arg } => {
                            self.declare_pattern(
                                arg, scope, flags, kind, node, parent, has_init, None,
                            );
                        }
                        _ => {}
                    }
                }
            }
            Node::ArrayPattern { elements } => {
                let f = flags | SymbolFlags::ARRAY_DESTRUCTURE;
                for &e in ast.list(elements) {
                    self.declare_pattern(e, scope, f, kind, node, parent, has_init, None);
                }
            }
            Node::AssignPattern { left, right } => {
                self.declare_pattern(left, scope, flags, kind, node, parent, true, None);
                self.visit(right);
            }
            Node::Rest { arg } => {
                self.declare_pattern(arg, scope, flags, kind, node, parent, has_init, None);
            }
            Node::Hole {} | Node::Missing {} => {}
            _ => self.visit(pat),
        }
        self.depth -= 1;
    }

    /// An assignment target: identifiers become writes, member expressions are visited
    /// normally, literals are destructuring patterns.
    fn visit_target(&mut self, target: NodeId, flags: RefFlags, value: Option<NodeId>) {
        if self.depth > MAX_DEPTH {
            return;
        }
        self.depth += 1;
        self.mark(target);
        let ast = self.ast;
        match *ast.node(target) {
            Node::Ident { .. } => self.add_ref(target, flags, value),
            Node::Paren { expr }
            | Node::NonNull { expr }
            | Node::As { expr, .. }
            | Node::Satisfies { expr, .. } => {
                if let Node::As { ty, .. } | Node::Satisfies { ty, .. } = *ast.node(target) {
                    self.visit_type(ty);
                }
                self.visit_target(expr, flags, value);
            }
            Node::TypeAssertion { ty, expr } => {
                self.visit_type(ty);
                self.visit_target(expr, flags, value);
            }
            Node::ObjectLit { props } => {
                let f = (flags | RefFlags::DESTRUCTURE).union(RefFlags::NONE).0
                    & !(RefFlags::READ.0 | RefFlags::COMPOUND.0);
                let f = RefFlags(f);
                for &p in ast.list(props) {
                    self.mark(p);
                    match *ast.node(p) {
                        Node::Property { key, value: v, .. } => {
                            self.visit_key(key);
                            self.visit_target(v, f, None);
                        }
                        Node::ShorthandProp { name, init, .. } => {
                            self.add_ref(name, f, None);
                            self.visit_opt(init);
                        }
                        Node::Spread { arg } => self.visit_target(arg, f, None),
                        _ => self.visit(p),
                    }
                }
            }
            Node::ArrayLit { elements } => {
                let f = RefFlags(
                    (flags | RefFlags::DESTRUCTURE).0 & !(RefFlags::READ.0 | RefFlags::COMPOUND.0),
                );
                for &e in ast.list(elements) {
                    self.visit_target(e, f, None);
                }
            }
            Node::Assign { left, right, .. } => {
                // `[a = 1] = x` default inside a destructuring target.
                self.visit_target(left, flags, None);
                self.visit(right);
            }
            Node::Spread { arg } | Node::Rest { arg } => self.visit_target(arg, flags, None),
            Node::ObjectPattern { .. } | Node::ArrayPattern { .. } => self.visit(target),
            Node::Hole {} => {}
            _ => self.visit(target),
        }
        self.depth -= 1;
    }

    fn visit_function(&mut self, f: NodeId) {
        let ast = self.ast;
        let (type_params, params, return_type, body, is_arrow_expr) = match *ast.node(f) {
            Node::FunctionDecl {
                type_params,
                params,
                return_type,
                body,
                ..
            }
            | Node::FunctionExpr {
                type_params,
                params,
                return_type,
                body,
                ..
            } => (type_params, params, return_type, body, false),
            Node::Arrow {
                type_params,
                params,
                return_type,
                body,
                flags,
            } => (
                type_params,
                params,
                return_type,
                Some(body),
                flags.contains(ts_r_ast::FnFlags::EXPRESSION_BODY),
            ),
            _ => return,
        };
        // Parameter decorators are evaluated in the enclosing scope.
        for &p in ast.list(params) {
            if let Node::Param { decorators, .. } = *ast.node(p) {
                self.visit_list(decorators);
            }
        }
        let scope = self.push_scope(ScopeKind::Function, f);
        self.declare_type_params(type_params);
        let signature = body.is_none() || self.ambient;
        self.declare_params(params, scope, signature);
        if let Some(r) = return_type {
            self.visit_type(r);
        }
        if let Some(b) = body {
            if is_arrow_expr {
                self.visit(b);
            } else {
                self.mark(b);
                self.sem.node_scope.entry(b).or_insert(scope);
                if let Node::Block { stmts } = *ast.node(b) {
                    self.visit_list(stmts);
                } else {
                    self.visit(b);
                }
            }
        }
        self.pop_scope();
    }

    fn declare_params(&mut self, params: ts_r_ast::NodeList, scope: ScopeId, signature: bool) {
        let ast = self.ast;
        for &p in ast.list(params) {
            self.mark(p);
            let Node::Param {
                modifiers,
                pattern,
                type_ann,
                init,
                ..
            } = *ast.node(p)
            else {
                self.visit(p);
                continue;
            };
            let is_this =
                matches!(ast.node(pattern), Node::Ident { name } if ast.atom(*name) == "this");
            if !is_this {
                let mut f = SymbolFlags::PARAM;
                if signature {
                    f.insert(SymbolFlags::SIGNATURE_PARAM);
                }
                if modifiers.intersects(
                    Modifiers::PUBLIC
                        | Modifiers::PRIVATE
                        | Modifiers::PROTECTED
                        | Modifiers::READONLY
                        | Modifiers::OVERRIDE,
                ) {
                    f.insert(SymbolFlags::PARAM_PROPERTY);
                }
                self.declare_pattern(
                    pattern,
                    scope,
                    f,
                    DeclKind::Param,
                    p,
                    p,
                    init.is_some(),
                    init,
                );
            } else {
                self.mark(pattern);
            }
            if let Some(t) = type_ann {
                self.visit_type(t);
            }
            self.visit_opt(init);
        }
    }

    fn visit_class(&mut self, c: NodeId) {
        let ast = self.ast;
        let (type_params, heritage, members) = match *ast.node(c) {
            Node::ClassDecl {
                type_params,
                heritage,
                members,
                ..
            }
            | Node::ClassExpr {
                type_params,
                heritage,
                members,
                ..
            } => (type_params, heritage, members),
            _ => return,
        };
        self.push_scope(ScopeKind::Class, c);
        self.declare_type_params(type_params);
        for &h in ast.list(heritage) {
            self.mark(h);
            if let Node::HeritageClause { kind, types } = *ast.node(h) {
                for &t in ast.list(types) {
                    self.mark(t);
                    if kind == HeritageKind::Extends {
                        if let Node::ExprWithTypeArgs { expr, type_args } = *ast.node(t) {
                            self.visit(expr);
                            if let Some(ta) = type_args {
                                self.visit_type(ta);
                            }
                        } else {
                            self.visit(t);
                        }
                    } else {
                        self.visit_type(t);
                    }
                }
            }
        }
        let was = self.ambient;
        for &m in ast.list(members) {
            if let Node::MethodDef { modifiers, .. } | Node::PropertyDef { modifiers, .. } =
                *ast.node(m)
                && (modifiers.contains(Modifiers::ABSTRACT)
                    || modifiers.contains(Modifiers::DECLARE))
            {
                self.ambient = true;
            }
            self.visit(m);
            self.ambient = was;
        }
        self.pop_scope();
    }

    fn declare_type_params(&mut self, tp: Option<NodeId>) {
        let Some(tp) = tp else { return };
        self.mark(tp);
        let ast = self.ast;
        if let Node::TypeParams { params } = *ast.node(tp) {
            let scope = self.cur;
            for &p in ast.list(params) {
                self.mark(p);
                if let Node::TypeParam { name, .. } = *ast.node(p) {
                    self.declare(
                        scope,
                        name,
                        SymbolFlags::TYPE_PARAM,
                        DeclKind::TypeParam,
                        p,
                        tp,
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
                        self.visit_type(c);
                    }
                    if let Some(d) = default {
                        self.visit_type(d);
                    }
                }
            }
        }
    }

    // ---- types ----

    fn visit_type(&mut self, id: NodeId) {
        if self.depth > MAX_DEPTH {
            return;
        }
        self.depth += 1;
        self.visit_type_inner(id);
        self.depth -= 1;
    }

    fn visit_type_inner(&mut self, id: NodeId) {
        self.mark(id);
        let ast = self.ast;
        match *ast.node(id) {
            Node::TypeRef { name, type_args } => {
                self.mark(name);
                if let Some(root) = qualified_root(ast, name) {
                    self.add_ref(root, RefFlags::READ | RefFlags::TYPE, None);
                }
                if let Some(t) = type_args {
                    self.visit_type(t);
                }
            }
            Node::Ident { .. } => self.add_ref(id, RefFlags::READ | RefFlags::TYPE, None),
            Node::QualifiedName { .. } => {
                if let Some(root) = qualified_root(ast, id) {
                    self.add_ref(root, RefFlags::READ | RefFlags::TYPE, None);
                }
            }
            Node::TypeQuery {
                expr_name,
                type_args,
            } => {
                self.mark(expr_name);
                match *ast.node(expr_name) {
                    Node::ImportType { .. } => self.visit_type(expr_name),
                    _ => {
                        if let Some(root) = qualified_root(ast, expr_name) {
                            self.add_ref(root, RefFlags::READ | RefFlags::TYPEOF_TYPE, None);
                        }
                    }
                }
                if let Some(t) = type_args {
                    self.visit_type(t);
                }
            }
            Node::ExprWithTypeArgs { expr, type_args } => {
                self.mark(expr);
                // `implements A<T>, B` can arrive as an instantiation expression.
                if let Node::Instantiation {
                    expr: inner,
                    type_args: ta,
                } = *ast.node(expr)
                {
                    self.mark(inner);
                    if let Some(root) = expr_root(ast, inner) {
                        self.add_ref(root, RefFlags::READ | RefFlags::TYPE, None);
                    }
                    self.visit_type(ta);
                }
                if let Some(root) = expr_root(ast, expr) {
                    self.add_ref(root, RefFlags::READ | RefFlags::TYPE, None);
                }
                if let Some(t) = type_args {
                    self.visit_type(t);
                }
            }
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
            } => {
                let s = self.push_scope(ScopeKind::Type, id);
                self.declare_type_params(type_params);
                self.declare_params(params, s, true);
                self.visit_type(return_type);
                self.pop_scope();
            }
            Node::CallSignature {
                type_params,
                params,
                return_type,
            }
            | Node::ConstructSignature {
                type_params,
                params,
                return_type,
            } => {
                let s = self.push_scope(ScopeKind::Type, id);
                self.declare_type_params(type_params);
                self.declare_params(params, s, true);
                if let Some(r) = return_type {
                    self.visit_type(r);
                }
                self.pop_scope();
            }
            Node::MethodSignature {
                key,
                type_params,
                params,
                return_type,
                ..
            } => {
                self.visit_key(key);
                let s = self.push_scope(ScopeKind::Type, id);
                self.declare_type_params(type_params);
                self.declare_params(params, s, true);
                if let Some(r) = return_type {
                    self.visit_type(r);
                }
                self.pop_scope();
            }
            Node::PropertySignature { key, type_ann, .. } => {
                self.visit_key(key);
                if let Some(t) = type_ann {
                    self.visit_type(t);
                }
            }
            Node::IndexSignature {
                params, type_ann, ..
            } => {
                for &p in ast.list(params) {
                    self.mark(p);
                    if let Node::Param {
                        pattern,
                        type_ann: Some(t),
                        ..
                    } = *ast.node(p)
                    {
                        self.mark(pattern);
                        self.visit_type(t);
                    }
                }
                if let Some(t) = type_ann {
                    self.visit_type(t);
                }
            }
            Node::MappedType {
                type_param,
                name_type,
                ty,
                ..
            } => {
                let s = self.push_scope(ScopeKind::Type, id);
                self.mark(type_param);
                if let Node::TypeParam {
                    name, constraint, ..
                } = *ast.node(type_param)
                {
                    self.declare(
                        s,
                        name,
                        SymbolFlags::TYPE_PARAM,
                        DeclKind::TypeParam,
                        type_param,
                        id,
                    );
                    if let Some(c) = constraint {
                        self.visit_type(c);
                    }
                }
                if let Some(n) = name_type {
                    self.visit_type(n);
                }
                if let Some(t) = ty {
                    self.visit_type(t);
                }
                self.pop_scope();
            }
            Node::ConditionalType {
                check,
                extends,
                true_ty,
                false_ty,
            } => {
                self.visit_type(check);
                self.push_scope(ScopeKind::Type, id);
                self.visit_type(extends);
                self.visit_type(true_ty);
                self.pop_scope();
                self.visit_type(false_ty);
            }
            Node::InferType { type_param } => {
                self.mark(type_param);
                if let Node::TypeParam {
                    name, constraint, ..
                } = *ast.node(type_param)
                {
                    // `infer X` belongs to the nearest conditional type's `extends` scope,
                    // even inside a nested function type.
                    let mut s = self.cur;
                    let mut sc = Some(self.cur);
                    while let Some(c) = sc {
                        let scope = &self.sem.scopes[c.0 as usize];
                        if matches!(ast.node(scope.node), Node::ConditionalType { .. }) {
                            s = c;
                            break;
                        }
                        sc = scope.parent;
                    }
                    self.declare(
                        s,
                        name,
                        SymbolFlags::TYPE_PARAM | SymbolFlags::INFER,
                        DeclKind::TypeParam,
                        type_param,
                        id,
                    );
                    if let Some(c) = constraint {
                        self.visit_type(c);
                    }
                }
            }
            Node::TypePredicate { param, ty, .. } => {
                self.mark(param);
                // `x is T` / `asserts x` refer to the parameter `x` from a type position.
                self.add_ref(param, RefFlags::READ | RefFlags::TYPEOF_TYPE, None);
                if let Some(t) = ty {
                    self.visit_type(t);
                }
            }
            Node::ImportType { type_args, .. } => {
                if let Some(t) = type_args {
                    self.visit_type(t);
                }
            }
            Node::NamedTupleMember { name, ty, .. } => {
                self.mark(name);
                self.visit_type(ty);
            }
            Node::ComputedKey { expr } => self.visit(expr),
            Node::StringLit { .. }
            | Node::NumberLit { .. }
            | Node::BigIntLit { .. }
            | Node::BoolLit { .. }
            | Node::NullLit {}
            | Node::KeywordType { .. }
            | Node::ThisType {}
            | Node::TemplateElement { .. } => {}
            Node::Unary { .. } => {}
            Node::TypeLiteral { members } => {
                for &m in ast.list(members) {
                    self.visit_type(m);
                }
            }
            _ => {
                if is_type_kind(ast.kind(id)) || ast.kind(id) == NodeKind::TypeArgs {
                    ast.for_each_child(id, |c| self.visit_type(c));
                } else {
                    self.visit(id);
                }
            }
        }
    }
}

/// The leftmost identifier of `A`, `A.B.C` (a `QualifiedName`), or `None`.
pub fn qualified_root(ast: &Ast, mut id: NodeId) -> Option<NodeId> {
    loop {
        match *ast.node(id) {
            Node::Ident { .. } => return Some(id),
            Node::QualifiedName { left, .. } => id = left,
            Node::Member { obj, .. } => id = obj,
            _ => return None,
        }
    }
}

/// The leftmost identifier of an entity-name expression (`A.B.C`).
pub fn expr_root(ast: &Ast, mut id: NodeId) -> Option<NodeId> {
    loop {
        match *ast.node(id) {
            Node::Ident { .. } => return Some(id),
            Node::Member { obj, .. } => id = obj,
            Node::QualifiedName { left, .. } => id = left,
            Node::Paren { expr } => id = expr,
            _ => return None,
        }
    }
}

fn skip_parens(ast: &Ast, mut id: NodeId) -> NodeId {
    while let Node::Paren { expr } = *ast.node(id) {
        id = expr;
    }
    id
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
            | IndexSignature
    )
}
