//! `using` and `await using` below ESNext (observed, tsc 7.0.2).
//!
//! A statement list with a `using` declaration becomes
//!
//! ```js
//! const env_1 = { stack: [], error: void 0, hasError: false };
//! try {
//!     const d = __addDisposableResource(env_1, x, false);   // `true` for `await using`
//!     ...the rest of the list...
//! }
//! catch (e_1) {
//!     env_1.error = e_1;
//!     env_1.hasError = true;
//! }
//! finally {
//!     __disposeResources(env_1);   // `const result_1 = ...; if (result_1) await result_1;`
//! }
//! ```
//!
//! At the top level only the statements from the first `using` on move into the `try`:
//! function declarations, imports and export lists are hoisted above it, other declarations
//! become assignments to a hoisted `var` (exported ones to `export let`, exported classes and
//! default exports to `export { C, _default as default }`). `for (using x of xs)` declares
//! `x` in its body from `x_1`; `for (using x = ...;;)` moves into a block.
//!
//! Names (`env_N`, `e_N`, `result_N`) are numbered file-wide in tsc's order: a scope's
//! statements first (an `env` when a list starts, its `e` when the list is done), then the
//! functions found in them, each in turn (the top level's hoisted function declarations
//! first).

use ts_r_ast::{Atom, Modifiers, Node, NodeId, NodeKind, NodeList, VarKind};

use crate::builder::Build;
use crate::helpers::Helper;
use crate::transform::Tx;

struct U<'t, 'a> {
    tx: &'t mut Tx<'a>,
    /// Start of the function body whose statements `list` handles next: statements before
    /// it were moved in from outside (constructor field initializers and parameter
    /// properties).
    body_start: Option<u32>,
}

impl Build for U<'_, '_> {
    fn ast_mut(&mut self) -> &mut ts_r_ast::Ast {
        self.tx.ast
    }
}

/// Lowers every `using` declaration (below ESNext).
pub(crate) fn run(tx: &mut Tx) {
    let mut u = U {
        tx,
        body_start: None,
    };
    let root = u.tx.ast.root;
    let Node::Program { stmts } = u.tx.n(root) else {
        return;
    };
    let stmts = u.scope(stmts, true);
    u.tx.set(root, Node::Program { stmts });
}

/// The first `using` declaration reachable in `ast` that lowering left behind. A `using`
/// directly in a `case` clause is not counted: tsc leaves it as written (an error case).
pub(crate) fn remaining(ast: &ts_r_ast::Ast) -> Option<ts_r_span::Span> {
    let mut stack = vec![ast.root];
    while let Some(id) = stack.pop() {
        let n = *ast.node(id);
        if crate::is_type_kind(n.kind()) {
            continue;
        }
        match n {
            Node::VarDecl {
                kind: VarKind::Using | VarKind::AwaitUsing,
                ..
            } => return Some(ast.span(id)),
            Node::SwitchCase { test, body } => {
                stack.extend(test);
                for &s in ast.list(body) {
                    if let Node::VarDecl {
                        kind: VarKind::Using | VarKind::AwaitUsing,
                        decls,
                        ..
                    } = *ast.node(s)
                    {
                        stack.extend(ast.list(decls).iter().copied());
                    } else {
                        stack.push(s);
                    }
                }
            }
            _ => ast.for_each_child(id, |c| stack.push(c)),
        }
    }
    None
}

impl U<'_, '_> {
    fn n(&self, id: NodeId) -> Node {
        self.tx.n(id)
    }

    /// `env_1`, `e_1`, `result_1`: a unique name (renamed in print order below ES2022).
    fn unique(&mut self, base: &str) -> Atom {
        let a = self.tx.fresh(base, true);
        self.tx.using_names.push(a);
        a
    }

    /// `this.p = p` (a parameter property's assignment).
    fn is_param_prop(&self, s: NodeId) -> bool {
        let Node::ExprStmt { expr } = self.n(s) else {
            return false;
        };
        let Node::Assign { left, right, .. } = self.n(expr) else {
            return false;
        };
        matches!(self.n(left), Node::Member { obj, prop, .. }
            if self.n(obj) == Node::This {} && self.tx.atom_of(prop).is_some()
                && self.tx.atom_of(prop) == self.tx.atom_of(right))
    }

    fn is_using(&self, s: NodeId) -> bool {
        matches!(
            self.n(s),
            Node::VarDecl {
                kind: VarKind::Using | VarKind::AwaitUsing,
                ..
            }
        )
    }

    /// A function body or the program: its statements, then the functions in them.
    fn scope(&mut self, list: NodeList, top: bool) -> NodeList {
        let mut fns = Vec::new();
        let out = self.list(list, top, &mut fns);
        for f in fns {
            self.function(f);
        }
        out
    }

    fn function(&mut self, f: NodeId) {
        let mut fns = Vec::new();
        match self.n(f) {
            Node::FunctionDecl {
                params,
                body: Some(b),
                ..
            }
            | Node::FunctionExpr {
                params,
                body: Some(b),
                ..
            } => {
                self.body(b);
                for p in self.tx.items(params) {
                    self.collect(p, &mut fns);
                }
            }
            Node::Arrow { params, body, .. } => {
                if self.tx.kind(body) == NodeKind::Block {
                    self.body(body);
                } else {
                    self.collect(body, &mut fns);
                }
                for p in self.tx.items(params) {
                    self.collect(p, &mut fns);
                }
            }
            Node::StaticBlock { body } => {
                let body = self.scope(body, false);
                self.tx.set(f, Node::StaticBlock { body });
            }
            _ => {}
        }
        for g in fns {
            self.function(g);
        }
    }

    fn body(&mut self, b: NodeId) {
        if let Node::Block { stmts } = self.n(b) {
            self.body_start = Some(self.tx.span(b).start);
            let new = self.scope(stmts, false);
            self.body_start = None;
            if new != stmts {
                self.tx.set(b, Node::Block { stmts: new });
                // A body written on one line stays on one line (observed).
                let sp = self.tx.span(b);
                if sp.start < sp.end
                    && self
                        .tx
                        .src
                        .get(sp.start as usize..sp.end as usize)
                        .is_some_and(|t| !t.contains('\n'))
                {
                    self.tx.mark_single_line(b);
                }
            }
        }
    }

    /// The statements of a list (functions found are pushed to `fns`).
    fn list(&mut self, list: NodeList, top: bool, fns: &mut Vec<NodeId>) -> NodeList {
        let items = self.tx.items(list);
        let body_start = self.body_start.take();
        match items.iter().position(|&s| self.is_using(s)) {
            Some(i) if top => self.top_level(&items, i, fns),
            Some(_) => self.wrap(&items, body_start, fns),
            _ => {
                for &s in &items {
                    self.stmt(s, fns);
                }
                list
            }
        }
    }

    /// A statement (in place); nested lists with `using` are lowered.
    fn stmt(&mut self, s: NodeId, fns: &mut Vec<NodeId>) {
        match self.n(s) {
            Node::Block { stmts } => {
                let stmts = self.list(stmts, false, fns);
                self.tx.set(s, Node::Block { stmts });
            }
            Node::If { test, cons, alt } => {
                self.collect(test, fns);
                self.stmt(cons, fns);
                if let Some(a) = alt {
                    self.stmt(a, fns);
                }
            }
            Node::For {
                init: Some(init), ..
            } if self.is_using(init) => self.for_using(s, fns),
            Node::For {
                init,
                test,
                update,
                body,
            } => {
                for e in [init, test, update].into_iter().flatten() {
                    self.collect(e, fns);
                }
                self.stmt(body, fns);
            }
            Node::ForOf { left, .. } if self.is_using(left) => self.for_of_using(s, fns),
            Node::ForOf {
                left, right, body, ..
            }
            | Node::ForIn { left, right, body } => {
                self.collect(left, fns);
                self.collect(right, fns);
                self.stmt(body, fns);
            }
            Node::While { test, body } | Node::DoWhile { body, test } => {
                self.collect(test, fns);
                self.stmt(body, fns);
            }
            Node::Labeled { body, .. } => self.stmt(body, fns),
            Node::With { obj, body } => {
                self.collect(obj, fns);
                self.stmt(body, fns);
            }
            Node::Switch { disc, cases } => {
                self.collect(disc, fns);
                for c in self.tx.items(cases) {
                    if let Node::SwitchCase { test, body } = self.n(c) {
                        if let Some(t) = test {
                            self.collect(t, fns);
                        }
                        // A `using` directly in a `case` clause is left alone (observed).
                        for st in self.tx.items(body) {
                            self.stmt(st, fns);
                        }
                    }
                }
            }
            Node::Try {
                block,
                handler,
                finalizer,
            } => {
                self.stmt(block, fns);
                if let Some(h) = handler
                    && let Node::CatchClause { param, body, .. } = self.n(h)
                {
                    if let Some(p) = param {
                        self.collect(p, fns);
                    }
                    self.stmt(body, fns);
                }
                if let Some(f) = finalizer {
                    self.stmt(f, fns);
                }
            }
            _ => self.collect(s, fns),
        }
    }

    /// Finds the functions in `id` (not inside them), in source order.
    fn collect(&mut self, id: NodeId, fns: &mut Vec<NodeId>) {
        match self.n(id) {
            Node::FunctionDecl { .. }
            | Node::FunctionExpr { .. }
            | Node::Arrow { .. }
            | Node::StaticBlock { .. } => fns.push(id),
            Node::MethodDef { key, value, .. } => {
                self.collect(key, fns);
                fns.push(value);
            }
            _ => {
                for c in self.tx.ast.children(id) {
                    self.collect(c, fns);
                }
            }
        }
    }

    // ---- building blocks ----

    /// `__addDisposableResource(env, value, async)`.
    fn add_resource(&mut self, env: Atom, value: NodeId, is_await: bool, at: u32) -> NodeId {
        let f = self.tx.helper_ref(Helper::AddDisposableResource, at);
        let e = self.ident_atom(env, at);
        let b = self.bool_lit(is_await, at);
        self.call(f, &[e, value, b], at)
    }

    /// `const env_1 = { stack: [], error: void 0, hasError: false };`
    fn env_decl(&mut self, env: Atom, at: u32) -> NodeId {
        let stack = self.array(&[], at);
        let p1 = self.prop("stack", stack, at);
        let v = self.void0(at);
        let p2 = self.prop("error", v, at);
        let f = self.bool_lit(false, at);
        let p3 = self.prop("hasError", f, at);
        let o = self.object(&[p1, p2, p3], at);
        let name = self.ident_atom(env, at);
        self.var(VarKind::Const, Modifiers::NONE, name, Some(o), at)
    }

    fn void0(&mut self, at: u32) -> NodeId {
        let zero = self.number(0.0, at);
        self.unary(ts_r_ast::UnaryOp::Void, zero, at)
    }

    /// `try { body } catch (e_1) { ... } finally { ... }` for `env` (its `e` and `result`
    /// names are made now, after the body's).
    fn try_stmt(&mut self, env: Atom, body: &[NodeId], is_await: bool, at: u32) -> NodeId {
        let e = self.unique("e");
        let result = is_await.then(|| self.unique("result"));
        let block = self.block(body, at);
        // catch (e_1) { env_1.error = e_1; env_1.hasError = true; }
        let env_ref = self.ident_atom(env, at);
        let err = self.member(env_ref, "error", at);
        let e_ref = self.ident_atom(e, at);
        let a1 = self.assign(err, e_ref, at);
        let s1 = self.expr_stmt(a1, at);
        let env_ref = self.ident_atom(env, at);
        let has = self.member(env_ref, "hasError", at);
        let t = self.bool_lit(true, at);
        let a2 = self.assign(has, t, at);
        let s2 = self.expr_stmt(a2, at);
        let cbody = self.block(&[s1, s2], at);
        let param = self.ident_atom(e, at);
        let handler = self.mk(
            Node::CatchClause {
                param: Some(param),
                type_ann: None,
                body: cbody,
            },
            at,
        );
        // finally { __disposeResources(env_1); }
        let f = self.tx.helper_ref(Helper::DisposeResources, at);
        let env_ref = self.ident_atom(env, at);
        let call = self.call(f, &[env_ref], at);
        let fin = match result {
            None => {
                let s = self.expr_stmt(call, at);
                self.block(&[s], at)
            }
            Some(r) => {
                let name = self.ident_atom(r, at);
                let d = self.var(VarKind::Const, Modifiers::NONE, name, Some(call), at);
                let test = self.ident_atom(r, at);
                let arg = self.ident_atom(r, at);
                let aw = self.mk(Node::Await { arg }, at);
                let cons = self.expr_stmt(aw, at);
                let iff = self.mk(
                    Node::If {
                        test,
                        cons,
                        alt: None,
                    },
                    at,
                );
                self.block(&[d, iff], at)
            }
        };
        self.mk(
            Node::Try {
                block,
                handler: Some(handler),
                finalizer: Some(fin),
            },
            at,
        )
    }

    /// `using a = x, b = y;` → `const a = __addDisposableResource(env, x, false), ...;`
    /// (in place). True for `await using`.
    fn convert_decl(&mut self, s: NodeId, env: Atom, fns: &mut Vec<NodeId>) -> bool {
        let Node::VarDecl {
            modifiers,
            kind,
            decls,
        } = self.n(s)
        else {
            return false;
        };
        let is_await = kind == VarKind::AwaitUsing;
        for d in self.tx.items(decls) {
            if let Node::VarDeclarator {
                name,
                definite,
                type_ann,
                init: Some(init),
            } = self.n(d)
            {
                self.collect(init, fns);
                let at = self.tx.at(init);
                let v = self.add_resource(env, init, is_await, at);
                self.tx.set(
                    d,
                    Node::VarDeclarator {
                        name,
                        definite,
                        type_ann,
                        init: Some(v),
                    },
                );
            }
        }
        self.tx.set(
            s,
            Node::VarDecl {
                modifiers,
                kind: VarKind::Const,
                decls,
            },
        );
        is_await
    }

    /// A list with `using` below the top level: everything after the prologue moves into
    /// the `try`.
    fn wrap(
        &mut self,
        items: &[NodeId],
        body_start: Option<u32>,
        fns: &mut Vec<NodeId>,
    ) -> NodeList {
        let pro = items
            .iter()
            .take_while(|&&s| self.tx.kind(s) == NodeKind::Directive)
            .count();
        // A constructor without a leading `super(...)`: the field initializers and parameter
        // properties moved in before the body run outside the `try`; the parameter
        // properties are assigned inside it again (observed).
        let mut out = items[..pro].to_vec();
        let mut inner = Vec::with_capacity(items.len() - pro);
        let mut rest = pro;
        if let Some(start) = body_start {
            for &s in &items[pro..] {
                let sp = self.tx.span(s);
                if sp.end > start || self.is_using(s) {
                    break;
                }
                out.push(s);
                if self.is_param_prop(s) {
                    let copy = crate::builder::deep_clone(self.tx.ast, s);
                    inner.push(copy);
                }
                rest += 1;
            }
        }
        let at = items.get(rest).map_or(0, |&s| self.tx.at(s));
        let env = self.unique("env");
        let mut is_await = false;
        for &s in &items[rest..] {
            if self.is_using(s) {
                is_await |= self.convert_decl(s, env, fns);
            } else {
                self.stmt(s, fns);
            }
            inner.push(s);
        }
        let decl = self.env_decl(env, at);
        let t = self.try_stmt(env, &inner, is_await, at);
        out.push(decl);
        out.push(t);
        self.new_list(&out)
    }

    /// `for (using a = x;;) body` → `{ const env_1 = ...; try { const a = ...; for (;;) body }
    /// ... }`.
    fn for_using(&mut self, s: NodeId, fns: &mut Vec<NodeId>) {
        let Node::For {
            init: Some(init),
            test,
            update,
            body,
        } = self.n(s)
        else {
            return;
        };
        let at = self.tx.at(s);
        let env = self.unique("env");
        let is_await = self.convert_decl(init, env, fns);
        for e in [test, update].into_iter().flatten() {
            self.collect(e, fns);
        }
        self.stmt(body, fns);
        let lp = self.mk(
            Node::For {
                init: None,
                test,
                update,
                body,
            },
            at,
        );
        let decl = self.env_decl(env, at);
        let t = self.try_stmt(env, &[init, lp], is_await, at);
        let block = self.block(&[decl, t], at);
        crate::builder::replace(self.tx.ast, s, block);
    }

    /// `for (using x of xs) body` → `for (const x_1 of xs) { const env_1 = ...; try { const x
    /// = __addDisposableResource(env_1, x_1, false); ...body } ... }`.
    fn for_of_using(&mut self, s: NodeId, fns: &mut Vec<NodeId>) {
        let Node::ForOf {
            is_await: loop_await,
            left,
            right,
            body,
        } = self.n(s)
        else {
            return;
        };
        self.collect(right, fns);
        let Node::VarDecl { kind, decls, .. } = self.n(left) else {
            return;
        };
        let Some(&d) = self.tx.ast.list(decls).first() else {
            return;
        };
        let Node::VarDeclarator { name, .. } = self.n(d) else {
            return;
        };
        let at = self.tx.at(s);
        let base = self
            .tx
            .atom_of(name)
            .map(|a| self.tx.ast.atom(a).to_owned())
            .unwrap_or_else(|| "_".to_owned());
        let fresh = self.tx.fresh(&base, true);
        let env = self.unique("env");
        let is_await = kind == VarKind::AwaitUsing;
        let loop_name = self.ident_atom(fresh, at);
        let loop_decl = self.var(VarKind::Const, Modifiers::NONE, loop_name, None, at);
        let value = self.ident_atom(fresh, at);
        let v = self.add_resource(env, value, is_await, at);
        let inner_name = crate::builder::clone_node(self.tx.ast, name);
        let inner_decl = self.var(VarKind::Const, Modifiers::NONE, inner_name, Some(v), at);
        let mut inner = vec![inner_decl];
        match self.n(body) {
            Node::Block { stmts } => {
                for st in self.tx.items(stmts) {
                    self.stmt(st, fns);
                    inner.push(st);
                }
            }
            _ => {
                self.stmt(body, fns);
                inner.push(body);
            }
        }
        let decl = self.env_decl(env, at);
        let t = self.try_stmt(env, &inner, is_await, at);
        let block = self.block(&[decl, t], at);
        self.tx.set(
            s,
            Node::ForOf {
                is_await: loop_await,
                left: loop_decl,
                right,
                body: block,
            },
        );
    }

    // ---- the top level ----

    /// The top level from the first `using` (at `first`) on.
    fn top_level(&mut self, items: &[NodeId], first: usize, fns: &mut Vec<NodeId>) -> NodeList {
        let at = self.tx.at(items[first]);
        let env = self.unique("env");
        let mut out: Vec<NodeId> = Vec::with_capacity(items.len() + 6);
        // An empty `export {}` goes last (tsc adds its own where a module needs one).
        let mut empty_export = None;
        for &s in &items[..first] {
            if let Node::ExportNamed {
                specifiers,
                source: None,
                ..
            } = self.n(s)
                && specifiers.is_empty()
            {
                empty_export = Some(s);
                continue;
            }
            self.stmt(s, fns);
            out.push(s);
        }
        let mut hoisted: Vec<NodeId> = Vec::new();
        let mut exports_hoisted: Vec<NodeId> = Vec::new();
        let mut specs: Vec<(Atom, bool)> = Vec::new();
        let mut vars: Vec<Atom> = Vec::new();
        let mut export_lets: Vec<Atom> = Vec::new();
        let mut body: Vec<NodeId> = Vec::new();
        let mut body_fns: Vec<NodeId> = Vec::new();
        let mut is_await = false;
        // CommonJS (already transformed): exports that tsc's module transform would see as
        // the synthesized `export { C, _default as default }` and as `export let w`.
        let cjs = self.tx.opts.is_cjs();
        let mut cjs_specs: Vec<String> = Vec::new();
        let mut cjs_lets: Vec<String> = Vec::new();
        // `module.exports = _default;` after the `try`.
        let mut tail: Vec<NodeId> = Vec::new();
        let rest = &items[first..];
        let mut j = 0;
        while j < rest.len() {
            let s = rest[j];
            j += 1;
            match self.n(s) {
                Node::FunctionDecl { .. } => {
                    hoisted.push(s);
                    fns.push(s);
                }
                Node::ExportNamed {
                    specifiers,
                    source: None,
                    ..
                } if specifiers.is_empty() => empty_export = Some(s),
                Node::ImportDecl { .. }
                | Node::ImportEquals { .. }
                | Node::ExportNamed { .. }
                | Node::ExportAll { .. } => exports_hoisted.push(s),
                Node::VarDecl {
                    modifiers,
                    kind,
                    decls,
                } => {
                    let using = matches!(kind, VarKind::Using | VarKind::AwaitUsing);
                    is_await |= kind == VarKind::AwaitUsing;
                    let exported = modifiers.contains(Modifiers::EXPORT);
                    let mut exprs = Vec::new();
                    let ds = self.tx.items(decls);
                    for &d in &ds {
                        let Node::VarDeclarator { name, init, .. } = self.n(d) else {
                            continue;
                        };
                        let names = self.bound_names(name);
                        if exported {
                            export_lets.extend(names);
                        } else {
                            vars.extend(names);
                        }
                        let Some(init) = init else {
                            continue;
                        };
                        self.collect(init, &mut body_fns);
                        let dat = self.tx.at(d);
                        let value = if using {
                            self.add_resource(env, init, kind == VarKind::AwaitUsing, dat)
                        } else {
                            init
                        };
                        let mut e = self.assign(name, value, dat);
                        // `let C = ...; export default C;` → `_default = C = ...` and
                        // `let C = ...; exports.C = C;` → `exports.C = C = ...` (observed).
                        if !using && ds.len() == 1 && self.tx.kind(name) == NodeKind::Ident {
                            let local = self.tx.atom_of(name);
                            // A decorated class (already its IIFE) is still a class to tsc's
                            // `using` transform; a plain `export default y` is a snapshot
                            // (`_default = y`) and is not merged.
                            let class_like = self.is_iife(init);
                            let source_exported = class_like && self.source_exported(s);
                            while let Some(&next) = rest.get(j) {
                                let copy = self.export_copy(next, local);
                                let default =
                                    matches!(copy, Some(Copy::Default | Copy::Cjs(_, true)));
                                if default && !class_like {
                                    break;
                                }
                                if cjs && source_exported {
                                    if default {
                                        cjs_specs.push("default".to_owned());
                                    } else if let Some(Copy::Cjs(t, _)) = copy
                                        && let Node::Member { prop, .. } = self.n(t)
                                        && let Some(a) = self.tx.atom_of(prop)
                                    {
                                        cjs_specs.push(self.tx.ast.atom(a).to_owned());
                                    }
                                }
                                match copy {
                                    Some(Copy::Default) => {
                                        let dn = self.tx.fresh("_default", false);
                                        vars.push(dn);
                                        specs.push((dn, true));
                                        let t = self.ident_atom(dn, dat);
                                        e = self.assign(t, e, dat);
                                    }
                                    Some(Copy::Cjs(target, default)) => {
                                        if default {
                                            let dn = self.tx.fresh("_default", false);
                                            vars.push(dn);
                                            let t = self.ident_atom(dn, dat);
                                            e = self.assign(t, e, dat);
                                        }
                                        e = self.assign(target, e, dat);
                                    }
                                    None => break,
                                }
                                j += 1;
                            }
                        }
                        exprs.push(e);
                    }
                    if !exprs.is_empty() {
                        let sat = self.tx.at(s);
                        let e = self.seq(&exprs, sat);
                        body.push(self.expr_stmt(e, sat));
                    }
                }
                Node::ClassDecl { modifiers, .. } => {
                    self.collect(s, &mut body_fns);
                    let sat = self.tx.at(s);
                    let Some(mut p) = crate::class::parts(self.tx, s) else {
                        continue;
                    };
                    let name = p.name.and_then(|n| self.tx.atom_of(n));
                    let default = modifiers.contains(Modifiers::DEFAULT);
                    let exported = modifiers.contains(Modifiers::EXPORT);
                    p.modifiers = Modifiers::NONE;
                    p.is_expr = true;
                    if default && name.is_none() {
                        self.name_default_class(&mut p, sat);
                    }
                    crate::class::set_parts(self.tx, s, p);
                    let mut e = s;
                    if let Some(n) = name {
                        vars.push(n);
                        if exported && !default {
                            specs.push((n, false));
                        }
                        let t = self.ident_atom(n, sat);
                        e = self.assign(t, e, sat);
                    }
                    if default {
                        let dn = self.tx.fresh("_default", false);
                        vars.push(dn);
                        specs.push((dn, true));
                        let t = self.ident_atom(dn, sat);
                        e = self.assign(t, e, sat);
                    }
                    // CommonJS: `class K {} exports.K = K;` → `exports.K = K = class K {}`.
                    if cjs && name.is_some() {
                        let source_exported = self.source_exported(s);
                        while let Some(&next) = rest.get(j) {
                            let Some(Copy::Cjs(target, is_default)) = self.export_copy(next, name)
                            else {
                                break;
                            };
                            if is_default {
                                let dn = self.tx.fresh("_default", false);
                                vars.push(dn);
                                let t = self.ident_atom(dn, sat);
                                e = self.assign(t, e, sat);
                            }
                            if source_exported {
                                if is_default {
                                    cjs_specs.push("default".to_owned());
                                } else if let Node::Member { prop, .. } = self.n(target)
                                    && let Some(a) = self.tx.atom_of(prop)
                                {
                                    cjs_specs.push(self.tx.ast.atom(a).to_owned());
                                }
                            }
                            e = self.assign(target, e, sat);
                            j += 1;
                        }
                    }
                    let st = self.expr_stmt(e, sat);
                    body.push(st);
                }
                Node::ExportDefaultExpr { expr } => {
                    self.collect(expr, &mut body_fns);
                    let sat = self.tx.at(s);
                    let dn = self.tx.fresh("_default", false);
                    vars.push(dn);
                    specs.push((dn, true));
                    let t = self.ident_atom(dn, sat);
                    let e = self.assign(t, expr, sat);
                    body.push(self.expr_stmt(e, sat));
                }
                Node::ExprStmt { expr } if cjs && self.exports_assign(expr).is_some() => {
                    self.collect(expr, &mut body_fns);
                    let (prop, value) = self.exports_assign(expr).unwrap_or((String::new(), expr));
                    if prop == "default" {
                        // `export default x` → `exports.default = _default = x` (observed).
                        let sat = self.tx.at(s);
                        let dn = self.tx.fresh("_default", false);
                        vars.push(dn);
                        cjs_specs.push(prop);
                        let t = self.ident_atom(dn, sat);
                        let inner = self.assign(t, value, sat);
                        if let Node::Assign { op, left, .. } = self.n(expr) {
                            self.tx.set(
                                expr,
                                Node::Assign {
                                    op,
                                    left,
                                    right: inner,
                                },
                            );
                        }
                    } else if self.tx.atom_of(value).map(|a| self.tx.ast.atom(a)) != Some(&prop) {
                        cjs_lets.push(prop);
                    }
                    body.push(s);
                }
                Node::ExprStmt { expr } if cjs && self.tx.cjs.export_assignments.contains(&s) => {
                    // `export = x` → `_default = x;` in the `try`, then
                    // `module.exports = _default;` after it (observed).
                    self.collect(expr, &mut body_fns);
                    if let Node::Assign { op, left, right } = self.n(expr) {
                        let sat = self.tx.at(s);
                        let dn = self.tx.fresh("_default", false);
                        vars.push(dn);
                        let t = self.ident_atom(dn, sat);
                        self.tx.set(expr, Node::Assign { op, left: t, right });
                        let v = self.ident_atom(dn, sat);
                        let e = self.assign(left, v, sat);
                        tail.push(self.expr_stmt(e, sat));
                    }
                    body.push(s);
                }
                _ => {
                    self.stmt(s, &mut body_fns);
                    body.push(s);
                }
            }
        }
        if !cjs_specs.is_empty() {
            self.reorder_void0(&mut out, &cjs_specs, &cjs_lets);
        }
        fns.extend(body_fns);
        out.extend(hoisted);
        out.extend(exports_hoisted);
        if !specs.is_empty() {
            let mut list = Vec::with_capacity(specs.len());
            for (n, default) in specs {
                let local = self.ident_atom(n, at);
                let exported = default.then(|| self.ident("default", at));
                list.push(self.mk(
                    Node::ExportSpecifier {
                        type_only: false,
                        local,
                        exported,
                    },
                    at,
                ));
            }
            let specifiers = self.new_list(&list);
            out.push(self.mk(
                Node::ExportNamed {
                    type_only: false,
                    specifiers,
                    source: None,
                    attributes: None,
                },
                at,
            ));
        }
        if !vars.is_empty() {
            out.push(self.names_decl(&vars, VarKind::Var, Modifiers::NONE, at));
        }
        if !export_lets.is_empty() {
            out.push(self.names_decl(&export_lets, VarKind::Let, Modifiers::EXPORT, at));
        }
        out.push(self.env_decl(env, at));
        out.push(self.try_stmt(env, &body, is_await, at));
        out.extend(tail);
        out.extend(empty_export);
        self.new_list(&out)
    }

    /// `var a, b;`
    fn names_decl(
        &mut self,
        names: &[Atom],
        kind: VarKind,
        modifiers: Modifiers,
        at: u32,
    ) -> NodeId {
        let decls: Vec<NodeId> = names
            .iter()
            .map(|&n| {
                let name = self.ident_atom(n, at);
                self.mk(
                    Node::VarDeclarator {
                        name,
                        definite: false,
                        type_ann: None,
                        init: None,
                    },
                    at,
                )
            })
            .collect();
        let decls = self.new_list(&decls);
        self.mk(
            Node::VarDecl {
                modifiers,
                kind,
                decls,
            },
            at,
        )
    }

    /// The names a binding pattern declares, in order.
    fn bound_names(&self, pattern: NodeId) -> Vec<Atom> {
        let mut out = Vec::new();
        let mut stack = vec![pattern];
        while let Some(p) = stack.pop() {
            match self.n(p) {
                Node::Ident { name } => out.push(name),
                Node::ObjectPattern { props } => {
                    for q in self.tx.items(props).into_iter().rev() {
                        stack.push(q);
                    }
                }
                Node::ArrayPattern { elements } => {
                    for q in self.tx.items(elements).into_iter().rev() {
                        stack.push(q);
                    }
                }
                Node::BindingProperty { key, value, .. } => stack.push(value.unwrap_or(key)),
                Node::AssignPattern { left, .. } => stack.push(left),
                Node::Rest { arg } => stack.push(arg),
                _ => {}
            }
        }
        out
    }

    /// `export default class { }`: the class is named `default` with a static block
    /// (`__setFunctionName(this, "default")`, lowered with the class's statics).
    fn name_default_class(&mut self, p: &mut crate::class::ClassParts, at: u32) {
        let f = self.tx.helper_ref(Helper::SetFunctionName, at);
        let this = self.this(at);
        let s = self.string("default", at);
        let call = self.call(f, &[this, s], at);
        let st = self.expr_stmt(call, at);
        let body = self.new_list(&[st]);
        let block = self.mk(Node::StaticBlock { body }, at);
        self.tx.mark_single_line(block);
        self.tx.inline_static.push(block);
        let mut members = vec![block];
        members.extend(self.tx.items(p.members));
        p.members = self.new_list(&members);
    }

    /// `(() => { ... })()`: a decorated class already lowered to its IIFE.
    fn is_iife(&self, e: NodeId) -> bool {
        let Node::Call { callee, args, .. } = self.n(e) else {
            return false;
        };
        self.tx.items(args).is_empty()
            && match self.n(callee) {
                Node::Arrow { .. } => true,
                Node::Paren { expr } => self.tx.kind(expr) == NodeKind::Arrow,
                _ => false,
            }
    }

    /// The source of declaration `s` has `export` before its `class` keyword (CommonJS has
    /// dropped the modifier).
    fn source_exported(&self, s: NodeId) -> bool {
        let sp = self.tx.span(s);
        let Some(text) = self.tx.src.get(sp.start as usize..sp.end as usize) else {
            return false;
        };
        let head = text.find("class").map_or(text, |i| &text[..i]);
        head.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '$'))
            .any(|w| w == "export")
    }

    /// `exports.name = value` (CommonJS): the name and the value.
    fn exports_assign(&self, e: NodeId) -> Option<(String, NodeId)> {
        let Node::Assign {
            op: ts_r_ast::AssignOp::Assign,
            left,
            right,
        } = self.n(e)
        else {
            return None;
        };
        let Node::Member { obj, prop, .. } = self.n(left) else {
            return None;
        };
        if self
            .tx
            .atom_of(obj)
            .is_none_or(|a| self.tx.ast.atom(a) != "exports")
        {
            return None;
        }
        let prop = self.tx.atom_of(prop)?;
        Some((self.tx.ast.atom(prop).to_owned(), right))
    }

    /// CommonJS: `exports.a = ... = void 0;` in the order tsc's module transform sees the
    /// lowered module: earlier exports, then `specs` (the synthesized export list), then
    /// `lets` (the hoisted `export let`) (observed).
    fn reorder_void0(&mut self, out: &mut Vec<NodeId>, specs: &[String], lets: &[String]) {
        let written = self.tx.cjs.void0_written.clone();
        let mut names: Vec<String> = written
            .iter()
            .filter(|n| !specs.contains(n) && !lets.contains(n))
            .cloned()
            .collect();
        for n in specs {
            if !names.contains(n) {
                names.push(n.clone());
            }
        }
        names.extend(
            written
                .iter()
                .filter(|n| lets.contains(n) && !specs.contains(n))
                .cloned(),
        );
        if names.len() > 50 || names == written {
            return;
        }
        let fresh = crate::cjs::void0_stmt(self.tx, &names);
        if let Some(old) = self.tx.cjs.void0_stmt {
            let n = self.n(fresh);
            self.tx.set(old, n);
        } else {
            let at = self
                .tx
                .cjs
                .esmodule_stmt
                .and_then(|e| out.iter().position(|&s| s == e))
                .map_or(0, |i| i + 1);
            out.insert(at, fresh);
            self.tx.cjs.void0_stmt = Some(fresh);
        }
        self.tx.cjs.void0_written = names;
    }

    /// `export default x;` (ESM) or `exports.y = x;` (CommonJS) right after `x`'s declaration.
    fn export_copy(&mut self, s: NodeId, local: Option<Atom>) -> Option<Copy> {
        let local = local?;
        match self.n(s) {
            Node::ExportDefaultExpr { expr } if self.tx.atom_of(expr) == Some(local) => {
                Some(Copy::Default)
            }
            Node::ExprStmt { expr } => {
                let Node::Assign {
                    op: ts_r_ast::AssignOp::Assign,
                    left,
                    right,
                } = self.n(expr)
                else {
                    return None;
                };
                if self.tx.atom_of(right) != Some(local) {
                    return None;
                }
                let Node::Member { obj, prop, .. } = self.n(left) else {
                    return None;
                };
                if self
                    .tx
                    .atom_of(obj)
                    .is_none_or(|a| self.tx.ast.atom(a) != "exports")
                {
                    return None;
                }
                let default = self
                    .tx
                    .atom_of(prop)
                    .is_some_and(|a| self.tx.ast.atom(a) == "default");
                Some(Copy::Cjs(left, default))
            }
            _ => None,
        }
    }
}

/// How a declaration is exported right after it.
enum Copy {
    /// `export default x;`
    Default,
    /// `exports.y = x;` (the target; `exports.default`).
    Cjs(NodeId, bool),
}
