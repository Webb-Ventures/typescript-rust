//! Scope analysis for JavaScript trees (parser output for `.js`, or `ts_r_transform` output).
//!
//! [`Scopes::analyze`] walks the tree once, creating a [`Scope`] per function, block, class,
//! catch clause, `for` head, `switch` and `with` body, declaring every binding in the right
//! scope (`var` and sloppy-mode block functions hoist to the nearest function scope), and
//! recording every identifier reference with the scope it appears in. References are
//! resolved after the walk, so hoisting and use-before-declaration need no second pass.
//!
//! Results are plain data keyed by ids: [`SymbolId`] and [`ScopeId`] are indices, and
//! [`Scopes::symbol_of`] maps any binding or resolved reference identifier node to its
//! symbol. Analysis is deterministic: the same tree yields the same ids, which the bundler
//! relies on (it re-analyzes a re-parsed file and expects the same symbol numbering).
//!
//! TypeScript-only syntax is skipped (types, `declare`); run the transform first for real
//! TypeScript input.

use rustc_hash::FxHashMap;
use ts_r_ast::{Ast, Atom, FnFlags, Modifiers, Node, NodeId, NodeKind, VarKind};

/// Index of a scope in [`Scopes::scopes`]. The module (program) scope is `ScopeId(0)`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct ScopeId(pub u32);

/// Index of a symbol in [`Scopes::symbols`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct SymbolId(pub u32);

impl ScopeId {
    pub const MODULE: ScopeId = ScopeId(0);
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

impl SymbolId {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ScopeKind {
    /// The program.
    Module,
    /// A function or arrow body with its parameters (`var` target).
    Function,
    /// An arrow function (like `Function`, but no own `arguments`/`this`).
    Arrow,
    /// Holds the name of a named function expression.
    FunctionName,
    /// A class body; holds a class expression's name.
    Class,
    /// `static { }` blocks (a `var` target).
    StaticBlock,
    Block,
    Catch,
    For,
    Switch,
    With,
}

impl ScopeKind {
    /// `var` declarations stop here.
    pub fn is_var_target(self) -> bool {
        matches!(
            self,
            ScopeKind::Module | ScopeKind::Function | ScopeKind::Arrow | ScopeKind::StaticBlock
        )
    }
}

#[derive(Clone, Debug)]
pub struct Scope {
    pub parent: Option<ScopeId>,
    pub kind: ScopeKind,
    /// The node that created the scope (program, function, block, ...).
    pub node: NodeId,
    /// Symbols declared here, in declaration order.
    pub symbols: Vec<SymbolId>,
    /// A direct `eval(...)` appears in this scope or a nested one: names visible here must
    /// keep their meaning.
    pub contains_eval: bool,
    /// Code here runs in strict mode.
    pub strict: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SymbolKind {
    Var,
    Let,
    Const,
    /// `using` / `await using`.
    Using,
    /// A function declaration.
    Function,
    /// A class declaration.
    Class,
    Param,
    CatchParam,
    /// An `import` binding.
    Import,
    /// The name of a named function expression (local to it).
    FunctionExprName,
    /// The name of a named class expression (local to it).
    ClassExprName,
    /// Added by a caller with [`Scopes::add_synthetic`] (no declaring identifier).
    Synthetic,
}

#[derive(Clone, Debug)]
pub struct Symbol {
    pub name: Atom,
    pub kind: SymbolKind,
    pub scope: ScopeId,
    /// First declaring identifier.
    pub decl: Option<NodeId>,
    /// Number of declaring identifiers (`var x; var x;` has 2).
    pub decls: u32,
    /// Number of resolved references (reads and writes).
    pub refs: u32,
    /// Number of references that assign to it.
    pub writes: u32,
}

/// An identifier that refers to a binding (not a declaration, property name or label).
#[derive(Clone, Copy, Debug)]
pub struct Reference {
    pub node: NodeId,
    /// The scope the identifier appears in.
    pub scope: ScopeId,
    /// `None` for globals (and `arguments`).
    pub symbol: Option<SymbolId>,
    pub write: bool,
}

/// What a scope sees from outside itself: a symbol of an enclosing scope, or a global name.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum Through {
    Symbol(SymbolId),
    Global(Atom),
}

/// The result of [`Scopes::analyze`].
#[derive(Clone, Debug, Default)]
pub struct Scopes {
    pub scopes: Vec<Scope>,
    pub symbols: Vec<Symbol>,
    pub refs: Vec<Reference>,
    /// Node index -> symbol index + 1 for declaring identifiers and resolved references.
    node_symbol: Vec<u32>,
    /// Node index -> scope index + 1 for nodes that create a scope.
    node_scope: FxHashMap<u32, u32>,
    /// `(scope, name) -> symbol`.
    names: FxHashMap<(u32, Atom), SymbolId>,
    /// Unresolved names (globals) and how often each is referenced.
    pub globals: FxHashMap<Atom, u32>,
    /// A direct `eval` appears somewhere.
    pub has_direct_eval: bool,
    /// A `with` statement appears somewhere (references inside can't be resolved statically).
    pub has_with: bool,
    /// The tree contains JSX.
    pub has_jsx: bool,
    /// Atoms of unresolved references, by identifier node.
    ref_atoms: FxHashMap<u32, Atom>,
    /// `(declaring scope, symbol)` for declarations that hoist out of the scope they appear
    /// in (`var` in a block, sloppy block functions). The scopes in between see the name.
    hoisted: Vec<(ScopeId, SymbolId)>,
}

impl Scopes {
    /// Analyzes `ast`. `strict` says whether the program is strict code (ES modules, or a
    /// `"use strict"` script; a leading directive is also detected here).
    pub fn analyze(ast: &Ast, strict: bool) -> Scopes {
        let mut b = Builder {
            ast,
            out: Scopes {
                node_symbol: vec![0; ast.nodes.len()],
                ..Scopes::default()
            },
            cur: ScopeId::MODULE,
            pending: Vec::new(),
            depth: 0,
        };
        let root = ast.root;
        let stmts = match *ast.node(root) {
            Node::Program { stmts } => ast.list(stmts),
            _ => &[],
        };
        let strict = strict || has_use_strict(ast, stmts);
        b.out.scopes.push(Scope {
            parent: None,
            kind: ScopeKind::Module,
            node: root,
            symbols: Vec::new(),
            contains_eval: false,
            strict,
        });
        b.out.node_scope.insert(root.raw(), 1);
        for &s in stmts {
            b.stmt(s);
        }
        b.resolve();
        b.out
    }

    /// The symbol an identifier node declares or refers to.
    #[inline]
    pub fn symbol_of(&self, id: NodeId) -> Option<SymbolId> {
        match self.node_symbol.get(id.index()) {
            Some(&v) if v != 0 => Some(SymbolId(v - 1)),
            _ => None,
        }
    }

    /// The scope a node creates (program, functions, blocks, ...).
    pub fn scope_of_node(&self, id: NodeId) -> Option<ScopeId> {
        self.node_scope.get(&id.raw()).map(|&v| ScopeId(v - 1))
    }

    pub fn scope(&self, id: ScopeId) -> &Scope {
        &self.scopes[id.index()]
    }

    pub fn symbol(&self, id: SymbolId) -> &Symbol {
        &self.symbols[id.index()]
    }

    /// The symbol named `name` declared directly in `scope`.
    pub fn lookup_in(&self, scope: ScopeId, name: Atom) -> Option<SymbolId> {
        self.names.get(&(scope.0, name)).copied()
    }

    /// The symbol `name` resolves to from `scope`.
    pub fn resolve_name(&self, mut scope: ScopeId, name: Atom) -> Option<SymbolId> {
        loop {
            if let Some(s) = self.lookup_in(scope, name) {
                return Some(s);
            }
            scope = self.scopes[scope.index()].parent?;
        }
    }

    /// Symbols declared in the module scope, in declaration order.
    pub fn module_symbols(&self) -> &[SymbolId] {
        &self.scopes[0].symbols
    }

    /// True when some binding anywhere is named `name`.
    pub fn declares_name(&self, name: Atom) -> bool {
        self.symbols.iter().any(|s| s.name == name)
    }

    /// Adds a symbol with no declaring identifier (for example the bundler's name for an
    /// anonymous `export default` value). Returns its id.
    pub fn add_synthetic(&mut self, scope: ScopeId, name: Atom) -> SymbolId {
        let id = SymbolId(self.symbols.len() as u32);
        self.symbols.push(Symbol {
            name,
            kind: SymbolKind::Synthetic,
            scope,
            decl: None,
            decls: 0,
            refs: 0,
            writes: 0,
        });
        self.scopes[scope.index()].symbols.push(id);
        id
    }

    /// Binds every unresolved reference named `name` to `sym` (for example a module
    /// wrapper's `exports` parameter that the bundler adds around the code). Returns how
    /// many references moved.
    pub fn adopt_global(&mut self, name: Atom, sym: SymbolId) -> u32 {
        let mut moved = 0;
        let mut writes = 0;
        for r in &mut self.refs {
            if r.symbol.is_none() && self.ref_atoms.get(&r.node.raw()) == Some(&name) {
                r.symbol = Some(sym);
                self.node_symbol[r.node.index()] = sym.0 + 1;
                moved += 1;
                writes += u32::from(r.write);
            }
        }
        self.globals.remove(&name);
        let s = &mut self.symbols[sym.index()];
        s.refs += moved;
        s.writes += writes;
        moved
    }

    /// Binds a new identifier node (made after analysis) to `sym`.
    pub fn bind_node(&mut self, node: NodeId, sym: SymbolId) {
        let i = node.index();
        if i >= self.node_symbol.len() {
            self.node_symbol.resize(i + 1, 0);
        }
        self.node_symbol[i] = sym.0 + 1;
    }

    /// For every scope, what its subtree references from outside it: symbols of enclosing
    /// scopes and global names. Sorted and deduplicated per scope.
    ///
    /// A `var` declared in a nested block counts as seen by the scopes between the block and
    /// the function it hoists to, even when nothing references it: a `let` of the block taking
    /// its name would be a redeclaration (`{ let a; var a; }` is a SyntaxError).
    pub fn through(&self) -> Vec<Vec<Through>> {
        let mut out: Vec<Vec<Through>> = vec![Vec::new(); self.scopes.len()];
        for &(site, sym) in &self.hoisted {
            let stop = self.symbols[sym.index()].scope;
            let mut s = Some(site);
            while let Some(cur) = s {
                if cur == stop {
                    break;
                }
                out[cur.index()].push(Through::Symbol(sym));
                s = self.scopes[cur.index()].parent;
            }
        }
        for r in &self.refs {
            let (stop, item) = match r.symbol {
                Some(s) => (Some(self.symbols[s.index()].scope), Through::Symbol(s)),
                None => match self.ident_name(r) {
                    Some(a) => (None, Through::Global(a)),
                    None => continue,
                },
            };
            let mut s = Some(r.scope);
            while let Some(cur) = s {
                if Some(cur) == stop {
                    break;
                }
                out[cur.index()].push(item);
                s = self.scopes[cur.index()].parent;
            }
        }
        for v in &mut out {
            v.sort_unstable();
            v.dedup();
        }
        out
    }

    fn ident_name(&self, r: &Reference) -> Option<Atom> {
        self.ref_atoms.get(&r.node.raw()).copied()
    }
}

fn has_use_strict(ast: &Ast, stmts: &[NodeId]) -> bool {
    for &s in stmts {
        match *ast.node(s) {
            Node::Directive { value } => {
                if ast.atom(value) == "use strict" {
                    return true;
                }
            }
            _ => return false,
        }
    }
    false
}

/// Maximum nesting the walker follows; deeper subtrees are treated as opaque (their
/// identifiers stay unresolved, which only makes renamers more conservative).
const MAX_DEPTH: u32 = 20_000;

struct Builder<'a> {
    ast: &'a Ast,
    out: Scopes,
    cur: ScopeId,
    /// `(identifier, scope, name, write)` waiting for resolution.
    pending: Vec<(NodeId, ScopeId, Atom, bool)>,
    depth: u32,
}

impl Builder<'_> {
    fn push_scope(&mut self, kind: ScopeKind, node: NodeId, strict: bool) -> ScopeId {
        let id = ScopeId(self.out.scopes.len() as u32);
        let strict = strict || self.out.scopes[self.cur.index()].strict;
        self.out.scopes.push(Scope {
            parent: Some(self.cur),
            kind,
            node,
            symbols: Vec::new(),
            contains_eval: false,
            strict,
        });
        self.out.node_scope.insert(node.raw(), id.0 + 1);
        let prev = self.cur;
        self.cur = id;
        prev
    }

    fn pop_scope(&mut self, prev: ScopeId) {
        self.cur = prev;
    }

    fn var_target(&self) -> ScopeId {
        let mut s = self.cur;
        loop {
            let sc = &self.out.scopes[s.index()];
            if sc.kind.is_var_target() {
                return s;
            }
            match sc.parent {
                Some(p) => s = p,
                None => return s,
            }
        }
    }

    fn declare(&mut self, ident: NodeId, kind: SymbolKind, scope: ScopeId) {
        let name = match *self.ast.node(ident) {
            Node::Ident { name } => name,
            _ => return,
        };
        if self.ast.atom(name) == "this" {
            return;
        }
        if kind == SymbolKind::Var && scope != self.cur {
            self.check_catch_var(name, scope);
        }
        let sym = match self.out.names.get(&(scope.0, name)) {
            Some(&s) => {
                let sy = &mut self.out.symbols[s.index()];
                sy.decls += 1;
                // A function declaration wins over `var` for the symbol's kind, and a
                // lexical declaration over a sloppy block function (Annex B does not make
                // that function a `var` when the names clash).
                if kind == SymbolKind::Function && sy.kind == SymbolKind::Var
                    || matches!(
                        kind,
                        SymbolKind::Let | SymbolKind::Const | SymbolKind::Using | SymbolKind::Class
                    ) && sy.kind == SymbolKind::Function
                {
                    sy.kind = kind;
                }
                s
            }
            None => {
                let s = SymbolId(self.out.symbols.len() as u32);
                self.out.symbols.push(Symbol {
                    name,
                    kind,
                    scope,
                    decl: Some(ident),
                    decls: 1,
                    refs: 0,
                    writes: 0,
                });
                self.out.scopes[scope.index()].symbols.push(s);
                self.out.names.insert((scope.0, name), s);
                s
            }
        };
        self.out.node_symbol[ident.index()] = sym.0 + 1;
        if scope != self.cur && self.is_ancestor(scope, self.cur) {
            self.out.hoisted.push((self.cur, sym));
        }
    }

    /// `anc` is a strict ancestor of `s`.
    fn is_ancestor(&self, anc: ScopeId, mut s: ScopeId) -> bool {
        while let Some(p) = self.out.scopes[s.index()].parent {
            if p == anc {
                return true;
            }
            s = p;
        }
        false
    }

    fn reference(&mut self, ident: NodeId, write: bool) {
        if let Node::Ident { name } = *self.ast.node(ident) {
            self.pending.push((ident, self.cur, name, write));
        }
    }

    fn resolve(&mut self) {
        let pending = std::mem::take(&mut self.pending);
        self.out.refs.reserve(pending.len());
        for (node, scope, name, write) in pending {
            let mut s = Some(scope);
            let mut found = None;
            while let Some(cur) = s {
                if let Some(&sym) = self.out.names.get(&(cur.0, name)) {
                    found = Some(sym);
                    break;
                }
                s = self.out.scopes[cur.index()].parent;
            }
            if let Some(sym) = found {
                let sy = &mut self.out.symbols[sym.index()];
                sy.refs += 1;
                if write {
                    sy.writes += 1;
                }
                self.out.node_symbol[node.index()] = sym.0 + 1;
            } else {
                *self.out.globals.entry(name).or_insert(0) += 1;
                self.out.ref_atoms.insert(node.raw(), name);
            }
            self.out.refs.push(Reference {
                node,
                scope,
                symbol: found,
                write,
            });
        }
    }

    fn strict(&self) -> bool {
        self.out.scopes[self.cur.index()].strict
    }

    // ---- statements ----

    fn stmts(&mut self, list: &[NodeId]) {
        for &s in list {
            self.stmt(s);
        }
    }

    fn stmt(&mut self, id: NodeId) {
        if self.depth > MAX_DEPTH {
            return;
        }
        self.depth += 1;
        self.stmt_inner(id);
        self.depth -= 1;
    }

    fn stmt_inner(&mut self, id: NodeId) {
        let ast = self.ast;
        match *ast.node(id) {
            Node::Block { stmts } => {
                let prev = self.push_scope(ScopeKind::Block, id, false);
                self.stmts(ast.list(stmts));
                self.pop_scope(prev);
            }
            Node::VarDecl {
                modifiers,
                kind,
                decls,
            } => {
                if modifiers.contains(Modifiers::DECLARE) {
                    return;
                }
                self.var_decl(kind, decls);
            }
            Node::FunctionDecl {
                modifiers,
                flags,
                name,
                params,
                body,
                ..
            } => {
                if modifiers.contains(Modifiers::DECLARE) || body.is_none() {
                    return;
                }
                if let Some(n) = name {
                    let target = if self.strict()
                        || self.out.scopes[self.cur.index()].kind.is_var_target()
                    {
                        self.cur
                    } else {
                        // Sloppy-mode block function (Annex B): also a `var` binding.
                        self.var_target()
                    };
                    self.declare(n, SymbolKind::Function, target);
                }
                self.function(id, flags, params, body, false);
            }
            Node::ClassDecl {
                decorators,
                modifiers,
                name,
                heritage,
                members,
                ..
            } => {
                if modifiers.contains(Modifiers::DECLARE) {
                    return;
                }
                if let Some(n) = name {
                    self.declare(n, SymbolKind::Class, self.cur);
                }
                self.class(id, decorators, None, heritage, members);
            }
            Node::If { test, cons, alt } => {
                self.expr(test);
                self.stmt(cons);
                if let Some(a) = alt {
                    self.stmt(a);
                }
            }
            Node::For {
                init,
                test,
                update,
                body,
            } => {
                let prev = self.push_scope(ScopeKind::For, id, false);
                if let Some(i) = init {
                    if matches!(ast.node(i), Node::VarDecl { .. }) {
                        self.stmt(i);
                    } else {
                        self.expr(i);
                    }
                }
                if let Some(t) = test {
                    self.expr(t);
                }
                if let Some(u) = update {
                    self.expr(u);
                }
                self.stmt(body);
                self.pop_scope(prev);
            }
            Node::ForIn { left, right, body }
            | Node::ForOf {
                left, right, body, ..
            } => {
                let prev = self.push_scope(ScopeKind::For, id, false);
                if matches!(ast.node(left), Node::VarDecl { .. }) {
                    self.stmt(left);
                } else {
                    self.target(left);
                }
                self.expr(right);
                self.stmt(body);
                self.pop_scope(prev);
            }
            Node::While { test, body } => {
                self.expr(test);
                self.stmt(body);
            }
            Node::DoWhile { body, test } => {
                self.stmt(body);
                self.expr(test);
            }
            Node::Return { arg } => {
                if let Some(a) = arg {
                    self.expr(a);
                }
            }
            Node::Throw { arg } | Node::ExprStmt { expr: arg } => self.expr(arg),
            Node::Labeled { body, .. } => self.stmt(body),
            Node::Break { .. } | Node::Continue { .. } => {}
            Node::Switch { disc, cases } => {
                self.expr(disc);
                let prev = self.push_scope(ScopeKind::Switch, id, false);
                for &c in ast.list(cases) {
                    if let Node::SwitchCase { test, body } = *ast.node(c) {
                        if let Some(t) = test {
                            self.expr(t);
                        }
                        self.stmts(ast.list(body));
                    }
                }
                self.pop_scope(prev);
            }
            Node::Try {
                block,
                handler,
                finalizer,
            } => {
                self.stmt(block);
                if let Some(h) = handler
                    && let Node::CatchClause { param, body, .. } = *ast.node(h)
                {
                    let prev = self.push_scope(ScopeKind::Catch, h, false);
                    if let Some(p) = param {
                        self.pattern(p, SymbolKind::CatchParam, self.cur);
                    }
                    if let Node::Block { stmts } = *ast.node(body) {
                        // The catch body shares the clause's scope.
                        self.stmts(ast.list(stmts));
                    } else {
                        self.stmt(body);
                    }
                    self.pop_scope(prev);
                }
                if let Some(f) = finalizer {
                    self.stmt(f);
                }
            }
            Node::With { obj, body } => {
                self.expr(obj);
                self.out.has_with = true;
                let prev = self.push_scope(ScopeKind::With, id, false);
                self.stmt(body);
                self.pop_scope(prev);
            }
            Node::ImportDecl {
                type_only,
                specifiers,
                ..
            } => {
                if type_only {
                    return;
                }
                for &sp in ast.list(specifiers) {
                    match *ast.node(sp) {
                        Node::ImportDefaultSpecifier { local }
                        | Node::ImportNamespaceSpecifier { local } => {
                            self.declare(local, SymbolKind::Import, ScopeId::MODULE)
                        }
                        Node::ImportSpecifier {
                            type_only, local, ..
                        } => {
                            if !type_only {
                                self.declare(local, SymbolKind::Import, ScopeId::MODULE)
                            }
                        }
                        _ => {}
                    }
                }
            }
            Node::ImportEquals {
                type_only,
                name,
                module_ref,
                ..
            } => {
                if !type_only {
                    self.declare(name, SymbolKind::Var, self.var_target());
                    if let Node::ExternalModuleRef { .. } = ast.node(module_ref) {
                    } else {
                        self.expr(module_ref);
                    }
                }
            }
            Node::ExportNamed {
                type_only,
                specifiers,
                source,
                ..
            } => {
                if type_only || source.is_some() {
                    return;
                }
                for &sp in ast.list(specifiers) {
                    if let Node::ExportSpecifier {
                        type_only: false,
                        local,
                        ..
                    } = *ast.node(sp)
                    {
                        self.reference(local, false);
                    }
                }
            }
            Node::ExportDefaultExpr { expr } | Node::ExportAssignment { expr } => self.expr(expr),
            Node::ExportAll { .. }
            | Node::NamespaceExport { .. }
            | Node::Empty {}
            | Node::Debugger {}
            | Node::Directive { .. }
            | Node::Interface { .. }
            | Node::TypeAlias { .. }
            | Node::Module { .. }
            | Node::Missing {} => {}
            Node::Enum { .. } => {
                // TypeScript enums are lowered by the transform; nothing to bind here.
            }
            _ => {
                if ast.node(id).is_statement() {
                    self.children(id);
                } else {
                    self.expr(id);
                }
            }
        }
    }

    fn var_decl(&mut self, kind: VarKind, decls: ts_r_ast::NodeList) {
        let ast = self.ast;
        let (sk, scope) = match kind {
            VarKind::Var => (SymbolKind::Var, self.var_target()),
            VarKind::Let => (SymbolKind::Let, self.cur),
            VarKind::Const => (SymbolKind::Const, self.cur),
            VarKind::Using | VarKind::AwaitUsing => (SymbolKind::Using, self.cur),
        };
        for &d in ast.list(decls) {
            if let Node::VarDeclarator { name, init, .. } = *ast.node(d) {
                self.pattern(name, sk, scope);
                if let Some(i) = init {
                    self.expr(i);
                }
            }
        }
    }

    /// Declares the identifiers of a binding pattern; visits defaults and computed keys.
    fn pattern(&mut self, id: NodeId, kind: SymbolKind, scope: ScopeId) {
        let ast = self.ast;
        match *ast.node(id) {
            Node::Ident { .. } => self.declare(id, kind, scope),
            Node::ObjectPattern { props } => {
                for &p in ast.list(props) {
                    match *ast.node(p) {
                        Node::BindingProperty { key, value, init } => {
                            if let Node::ComputedKey { expr } = *ast.node(key) {
                                self.expr(expr);
                            }
                            match value {
                                Some(v) => self.pattern(v, kind, scope),
                                None => self.pattern(key, kind, scope),
                            }
                            if let Some(i) = init {
                                self.expr(i);
                            }
                        }
                        Node::Rest { arg } => self.pattern(arg, kind, scope),
                        _ => {}
                    }
                }
            }
            Node::ArrayPattern { elements } => {
                for &e in ast.list(elements) {
                    self.pattern(e, kind, scope);
                }
            }
            Node::AssignPattern { left, right } => {
                self.pattern(left, kind, scope);
                self.expr(right);
            }
            Node::Rest { arg } => self.pattern(arg, kind, scope),
            Node::Hole {} | Node::Missing {} => {}
            _ => self.target(id),
        }
    }

    fn function(
        &mut self,
        id: NodeId,
        flags: FnFlags,
        params: ts_r_ast::NodeList,
        body: Option<NodeId>,
        arrow: bool,
    ) {
        let ast = self.ast;
        let body_stmts: &[NodeId] = match body.map(|b| ast.node(b)) {
            Some(Node::Block { stmts }) => ast.list(*stmts),
            _ => &[],
        };
        let strict = has_use_strict(ast, body_stmts);
        let kind = if arrow {
            ScopeKind::Arrow
        } else {
            ScopeKind::Function
        };
        let prev = self.push_scope(kind, id, strict);
        for &p in ast.list(params) {
            if let Node::Param {
                decorators,
                pattern,
                init,
                ..
            } = *ast.node(p)
            {
                for &d in ast.list(decorators) {
                    self.expr(d);
                }
                self.pattern(pattern, SymbolKind::Param, self.cur);
                if let Some(i) = init {
                    self.expr(i);
                }
            } else {
                self.pattern(p, SymbolKind::Param, self.cur);
            }
        }
        match body {
            Some(b) if flags.contains(FnFlags::EXPRESSION_BODY) => self.expr(b),
            Some(b) => {
                if let Node::Block { .. } = ast.node(b) {
                    self.out.node_scope.insert(b.raw(), self.cur.0 + 1);
                    self.stmts(body_stmts);
                } else {
                    self.stmt(b);
                }
            }
            None => {}
        }
        self.pop_scope(prev);
    }

    fn class(
        &mut self,
        id: NodeId,
        decorators: ts_r_ast::NodeList,
        expr_name: Option<NodeId>,
        heritage: ts_r_ast::NodeList,
        members: ts_r_ast::NodeList,
    ) {
        let ast = self.ast;
        for &d in ast.list(decorators) {
            self.expr(d);
        }
        let prev = self.push_scope(ScopeKind::Class, id, true);
        if let Some(n) = expr_name {
            self.declare(n, SymbolKind::ClassExprName, self.cur);
        }
        for &h in ast.list(heritage) {
            if let Node::HeritageClause {
                kind: ts_r_ast::HeritageKind::Extends,
                types,
            } = *ast.node(h)
            {
                for &t in ast.list(types) {
                    match *ast.node(t) {
                        Node::ExprWithTypeArgs { expr, .. } => self.expr(expr),
                        _ => self.expr(t),
                    }
                }
            }
        }
        for &m in ast.list(members) {
            self.class_member(m);
        }
        self.pop_scope(prev);
    }

    fn class_member(&mut self, m: NodeId) {
        let ast = self.ast;
        match *ast.node(m) {
            Node::MethodDef {
                decorators,
                key,
                value,
                ..
            } => {
                for &d in ast.list(decorators) {
                    self.expr(d);
                }
                self.key(key);
                self.expr(value);
            }
            Node::PropertyDef {
                decorators,
                modifiers,
                key,
                value,
                ..
            } => {
                if modifiers.contains(Modifiers::DECLARE) {
                    return;
                }
                for &d in ast.list(decorators) {
                    self.expr(d);
                }
                self.key(key);
                if let Some(v) = value {
                    // Field initializers behave like methods: their own `arguments`/`var`.
                    let prev = self.push_scope(ScopeKind::StaticBlock, m, true);
                    self.expr(v);
                    self.pop_scope(prev);
                }
            }
            Node::StaticBlock { body } => {
                let prev = self.push_scope(ScopeKind::StaticBlock, m, true);
                self.stmts(ast.list(body));
                self.pop_scope(prev);
            }
            Node::IndexSignature { .. } | Node::Empty {} => {}
            _ => {}
        }
    }

    fn key(&mut self, key: NodeId) {
        if let Node::ComputedKey { expr } = *self.ast.node(key) {
            self.expr(expr);
        }
    }

    // ---- expressions ----

    fn children(&mut self, id: NodeId) {
        let ast = self.ast;
        let mut kids: smallvec_lite::Kids = smallvec_lite::Kids::new();
        ast.for_each_child(id, |c| kids.push(c));
        for c in kids.iter() {
            if ast.node(c).is_statement() {
                self.stmt(c);
            } else {
                self.expr(c);
            }
        }
    }

    fn expr(&mut self, id: NodeId) {
        if self.depth > MAX_DEPTH {
            return;
        }
        self.depth += 1;
        self.expr_inner(id);
        self.depth -= 1;
    }

    fn expr_inner(&mut self, id: NodeId) {
        let ast = self.ast;
        let node = *ast.node(id);
        if is_type_kind(node.kind()) {
            return;
        }
        match node {
            Node::Ident { .. } => self.reference(id, false),
            Node::Member { obj, .. } => self.expr(obj),
            Node::Property { key, value, .. } => {
                self.key(key);
                self.expr(value);
            }
            Node::ShorthandProp { name, init, .. } => {
                self.reference(name, false);
                if let Some(i) = init {
                    self.expr(i);
                }
            }
            Node::MethodDef { .. } | Node::PropertyDef { .. } => self.class_member(id),
            Node::FunctionExpr {
                flags,
                name,
                params,
                body,
                ..
            } => {
                if let Some(n) = name {
                    let prev = self.push_scope(ScopeKind::FunctionName, id, false);
                    self.declare(n, SymbolKind::FunctionExprName, self.cur);
                    self.function(id, flags, params, body, false);
                    self.pop_scope(prev);
                } else {
                    self.function(id, flags, params, body, false);
                }
            }
            Node::Arrow {
                flags,
                params,
                body,
                ..
            } => self.function(id, flags, params, Some(body), true),
            Node::ClassExpr {
                decorators,
                name,
                heritage,
                members,
                ..
            } => self.class(id, decorators, name, heritage, members),
            Node::Assign { left, right, .. } => {
                self.target(left);
                self.expr(right);
            }
            Node::Update { arg, .. } => self.target(arg),
            Node::Call {
                callee,
                args,
                optional,
                ..
            } => {
                if !optional
                    && let Node::Ident { name } = *ast.node(callee)
                    && ast.atom(name) == "eval"
                {
                    self.mark_eval();
                }
                self.expr(callee);
                for &a in ast.list(args) {
                    self.expr(a);
                }
            }
            Node::New { callee, args, .. } => {
                self.expr(callee);
                for &a in ast.list(args) {
                    self.expr(a);
                }
            }
            Node::TaggedTemplate { tag, quasi, .. } => {
                self.expr(tag);
                self.expr(quasi);
            }
            Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::TypeAssertion { expr, .. }
            | Node::NonNull { expr }
            | Node::Instantiation { expr, .. } => self.expr(expr),
            Node::JsxOpening { name, attrs, .. } => {
                self.out.has_jsx = true;
                self.jsx_name(name);
                for &a in ast.list(attrs) {
                    self.expr(a);
                }
            }
            Node::JsxClosing { name } => self.jsx_name(name),
            Node::JsxAttr { value, .. } => {
                if let Some(v) = value {
                    self.expr(v);
                }
            }
            Node::JsxElement { .. } | Node::JsxFragment { .. } => {
                self.out.has_jsx = true;
                self.children(id);
            }
            Node::JsxText {} | Node::JsxNamespacedName { .. } => {}
            Node::ObjectPattern { .. } | Node::ArrayPattern { .. } => self.target(id),
            Node::ImportCall { arg, options, .. } => {
                self.expr(arg);
                if let Some(o) = options {
                    self.expr(o);
                }
            }
            Node::Decorator { expr } => self.expr(expr),
            Node::ComputedKey { expr } => self.expr(expr),
            Node::This {}
            | Node::Super {}
            | Node::NullLit {}
            | Node::BoolLit { .. }
            | Node::NumberLit { .. }
            | Node::BigIntLit { .. }
            | Node::StringLit { .. }
            | Node::RegexLit { .. }
            | Node::TemplateElement { .. }
            | Node::MetaProperty { .. }
            | Node::Hole {}
            | Node::PrivateName { .. }
            | Node::Missing {} => {}
            _ => {
                if node.is_statement() {
                    self.stmt(id);
                } else {
                    self.children(id);
                }
            }
        }
    }

    fn jsx_name(&mut self, name: NodeId) {
        let ast = self.ast;
        match *ast.node(name) {
            Node::Ident { name: a } => {
                let s = ast.atom(a);
                let intrinsic =
                    s.chars().next().is_some_and(|c| c.is_ascii_lowercase()) || s.contains('-');
                if !intrinsic && s != "this" {
                    self.reference(name, false);
                }
            }
            Node::JsxMemberExpr { obj, .. } => self.jsx_name(obj),
            Node::This {} => {}
            _ => {}
        }
    }

    /// An assignment target (expression form): identifiers are writes.
    fn target(&mut self, id: NodeId) {
        let ast = self.ast;
        match *ast.node(id) {
            Node::Ident { .. } => self.reference(id, true),
            Node::Paren { expr }
            | Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::NonNull { expr }
            | Node::TypeAssertion { expr, .. } => self.target(expr),
            Node::ArrayLit { elements } => {
                for &e in ast.list(elements) {
                    self.target(e);
                }
            }
            Node::ObjectLit { props } => {
                for &p in ast.list(props) {
                    match *ast.node(p) {
                        Node::Property { key, value, .. } => {
                            self.key(key);
                            self.target(value);
                        }
                        Node::ShorthandProp { name, init, .. } => {
                            self.reference(name, true);
                            if let Some(i) = init {
                                self.expr(i);
                            }
                        }
                        Node::Spread { arg } => self.target(arg),
                        _ => self.expr(p),
                    }
                }
            }
            Node::Spread { arg } | Node::Rest { arg } => self.target(arg),
            Node::Assign { left, right, .. } | Node::AssignPattern { left, right } => {
                self.target(left);
                self.expr(right);
            }
            Node::ObjectPattern { props } => {
                for &p in ast.list(props) {
                    match *ast.node(p) {
                        Node::BindingProperty { key, value, init } => {
                            self.key(key);
                            match value {
                                Some(v) => self.target(v),
                                None => self.reference(key, true),
                            }
                            if let Some(i) = init {
                                self.expr(i);
                            }
                        }
                        Node::Rest { arg } => self.target(arg),
                        _ => {}
                    }
                }
            }
            Node::ArrayPattern { elements } => {
                for &e in ast.list(elements) {
                    self.target(e);
                }
            }
            Node::Hole {} => {}
            _ => self.expr(id),
        }
    }

    /// `catch (e) { var e = 1; }` (Annex B): the `var` hoists, but its initializer assigns the
    /// catch parameter. One identifier then names two bindings, which renaming can't express,
    /// so freeze the names from here outwards, as direct `eval` does.
    fn check_catch_var(&mut self, name: Atom, target: ScopeId) {
        let mut s = self.cur;
        while s != target {
            let sc = &self.out.scopes[s.index()];
            if sc.kind == ScopeKind::Catch
                && self.out.names.get(&(s.0, name)).is_some_and(|&sym| {
                    self.out.symbols[sym.index()].kind == SymbolKind::CatchParam
                })
            {
                let mut f = Some(self.cur);
                while let Some(cur) = f {
                    self.out.scopes[cur.index()].contains_eval = true;
                    f = self.out.scopes[cur.index()].parent;
                }
                return;
            }
            match sc.parent {
                Some(p) => s = p,
                None => return,
            }
        }
    }

    fn mark_eval(&mut self) {
        self.out.has_direct_eval = true;
        let mut s = Some(self.cur);
        while let Some(cur) = s {
            self.out.scopes[cur.index()].contains_eval = true;
            s = self.out.scopes[cur.index()].parent;
        }
    }
}

/// Type-only node kinds (never contain runtime bindings).
pub fn is_type_kind(k: NodeKind) -> bool {
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

/// A tiny inline vector for child lists (most nodes have at most 6 children).
mod smallvec_lite {
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
            if self.len < 8 && self.spill.is_empty() {
                self.inline[self.len] = Some(id);
                self.len += 1;
            } else {
                if self.spill.is_empty() {
                    self.spill.extend(self.inline.iter().flatten().copied());
                }
                self.spill.push(id);
            }
        }

        pub fn iter(&self) -> impl Iterator<Item = NodeId> + '_ {
            let spilled = !self.spill.is_empty();
            let a = self.inline[..if spilled { 0 } else { self.len }]
                .iter()
                .flatten()
                .copied();
            a.chain(self.spill.iter().copied())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ts_r_ast::SourceKind;

    fn analyze(src: &str) -> (Ast, Scopes) {
        let ast = ts_r_parser::parse(src, SourceKind::Js).ast;
        let s = Scopes::analyze(&ast, true);
        (ast, s)
    }

    fn names(ast: &Ast, s: &Scopes, scope: ScopeId) -> Vec<String> {
        s.scope(scope)
            .symbols
            .iter()
            .map(|&x| ast.atom(s.symbol(x).name).to_owned())
            .collect()
    }

    #[test]
    fn hoisting_and_blocks() {
        let (ast, s) =
            analyze("f(); function f() { var a; { let b; var c; } } let x = 1; { const y = x; }");
        assert_eq!(names(&ast, &s, ScopeId::MODULE), ["f", "x"]);
        // f's scope holds a and c (var hoists out of the block); the block holds b.
        let f_scope = ScopeId(1);
        assert_eq!(names(&ast, &s, f_scope), ["a", "c"]);
        assert_eq!(names(&ast, &s, ScopeId(2)), ["b"]);
        let f = s.module_symbols()[0];
        assert_eq!(s.symbol(f).refs, 1);
        let x = s.module_symbols()[1];
        assert_eq!(s.symbol(x).refs, 1);
    }

    #[test]
    fn globals_properties_and_labels() {
        let (ast, s) = analyze("a.b = c; o = { d: e, f }; lbl: for (;;) break lbl;");
        let mut g: Vec<_> = s.globals.keys().map(|&k| ast.atom(k).to_owned()).collect();
        g.sort();
        assert_eq!(g, ["a", "c", "e", "f", "o"]);
    }

    #[test]
    fn named_function_and_class_expressions_are_local() {
        let (ast, s) = analyze(
            "const g = function h() { return h; }; const C = class D { m() { return D; } };",
        );
        assert_eq!(names(&ast, &s, ScopeId::MODULE), ["g", "C"]);
        assert!(s.globals.is_empty());
    }

    #[test]
    fn through_sets() {
        let (_ast, s) = analyze("let a; function f() { return () => a + b; }");
        let t = s.through();
        // The arrow (scope 2) and f (scope 1) both see `a` and the global `b`.
        assert_eq!(t[1].len(), 2);
        assert_eq!(t[2].len(), 2);
        assert!(t[0].iter().all(|x| matches!(x, Through::Global(_))));
    }

    #[test]
    fn eval_marks_scopes() {
        let (_ast, s) = analyze("function f() { eval('x'); } function g() {}");
        assert!(s.has_direct_eval);
        assert!(s.scope(ScopeId(1)).contains_eval);
        assert!(!s.scope(ScopeId(2)).contains_eval);
    }

    #[test]
    fn destructuring_and_writes() {
        let (ast, s) =
            analyze("let a, b; [a, { b }] = [1, { b: 2 }]; const { c, d: [e = a] } = x;");
        assert_eq!(names(&ast, &s, ScopeId::MODULE), ["a", "b", "c", "e"]);
        let a = s.module_symbols()[0];
        assert_eq!(s.symbol(a).writes, 1);
        assert_eq!(s.symbol(a).refs, 2);
    }

    #[test]
    fn sloppy_block_functions_hoist() {
        let ast = ts_r_parser::parse("if (x) { function f() {} } f();", SourceKind::Js).ast;
        let s = Scopes::analyze(&ast, false);
        assert_eq!(names(&ast, &s, ScopeId::MODULE), ["f"]);
        let strict = Scopes::analyze(&ast, true);
        assert!(strict.module_symbols().is_empty());
    }
}
