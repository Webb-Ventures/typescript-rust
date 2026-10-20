//! ES2017: async functions (observed, tsc 7.0.2).
//!
//! - `async function f(a) { body }` → `function f(a) { return __awaiter(this, void 0, void 0,
//!   function* () { body }); }`: a new, multi-line outer body; the generator keeps the
//!   original body (and its one-line layout). `await x` → `yield x` (also in a plain generator,
//!   never at the top level).
//! - Parameters with an initializer or a pattern move to the generator (a plain rest
//!   parameter does not): the outer
//!   function keeps the ones before the first initializer or rest, renamed (`a_1`, patterns
//!   `_a`), and passes `arguments`; an arrow passes `[a_1, ...args_1]` instead.
//! - An arrow passes `this` inside a non-arrow function and `void 0` elsewhere, and keeps an
//!   expression body: `() => __awaiter(void 0, ...)`.
//! - `arguments` in the body (and its arrows) is read through `var arguments_1 = arguments;`,
//!   the first statement of the outer body (an arrow gets a block body for it).
//! - `super` in an async method: `super.m` → `_super.m` with
//!   `const _super = Object.create(null, { m: { get: () => super.m } });`, `super[k]` →
//!   `_superIndex(k)` with `const _superIndex = name => super[name];` (before `_super`),
//!   and calls pass `this`: `_super.m.call(this, x)`. If any `super` property is assigned,
//!   every entry gets `set: v => super.m = v` and `_superIndex` caches getter/setter objects
//!   (`_superIndex(k).value`).

use ts_r_ast::{Atom, FnFlags, MethodKind, Modifiers, Node, NodeId, NodeList, UnaryOp, VarKind};

use super::lower::{FnCtx, Group, Lower, SuperUse};
use crate::builder::Build;
use crate::helpers::Helper;
use crate::scope::BindingKind;

impl Lower<'_, '_> {
    // ---- await ----

    /// `await x` → `yield x`.
    pub fn await_to_yield(&mut self, id: NodeId) {
        let Node::Await { arg } = self.n(id) else {
            return;
        };
        self.visit(arg);
        self.tx.set(
            id,
            Node::Yield {
                delegate: false,
                arg: Some(arg),
            },
        );
    }

    /// `await e` in the innermost function, as the lowered code needs it: `yield
    /// __await(e)` in a lowered async generator, `yield e` in a lowered async function, else
    /// `await e`.
    pub fn awaited(&mut self, e: NodeId, at: u32) -> NodeId {
        if self.below(ts_r_config::ScriptTarget::ES2018) && self.in_async_gen() {
            let f = self.tx.helper_ref(Helper::Await, at);
            let c = self.call(f, &[e], at);
            return self.mk(
                Node::Yield {
                    delegate: false,
                    arg: Some(c),
                },
                at,
            );
        }
        if self.below(ts_r_config::ScriptTarget::ES2017) && !self.fns.is_empty() {
            return self.mk(
                Node::Yield {
                    delegate: false,
                    arg: Some(e),
                },
                at,
            );
        }
        self.mk(Node::Await { arg: e }, at)
    }

    /// True if the innermost function is an async generator being lowered.
    pub fn in_async_gen(&self) -> bool {
        self.fns.last().is_some_and(|f| f.async_gen)
    }

    // ---- arguments ----

    /// Explicit `arguments` bindings must not be hidden by the generated generator's
    /// implicit arguments object. Rename those bindings without adding a capture prologue.
    pub fn rename_arguments_bindings(&mut self) {
        if !self.below(ts_r_config::ScriptTarget::ES2017)
            || !self.tx.sc.bindings.iter().any(|b| b.name == self.arguments)
        {
            return;
        }
        let mut owners = vec![None; self.tx.ast.node_count() + 1];
        let mut lowered = vec![false; self.tx.ast.node_count() + 1];
        let mut ids = Vec::new();
        let mut stack = vec![(self.tx.ast.root, None, false)];
        while let Some((id, mut owner, mut lower)) = stack.pop() {
            match self.n(id) {
                Node::FunctionDecl { flags, .. } | Node::FunctionExpr { flags, .. } => {
                    owner = Some(id);
                    lower = flags.contains(FnFlags::ASYNC);
                }
                Node::Arrow { flags, .. } => lower |= flags.contains(FnFlags::ASYNC),
                _ => {}
            }
            owners[id.index()] = owner;
            lowered[id.index()] = lower;
            ids.push(id);
            let children = self.tx.ast.children(id);
            stack.extend(children.into_iter().rev().map(|c| (c, owner, lower)));
        }
        let mut aliases = vec![None; self.tx.sc.bindings.len()];
        for &id in &ids {
            if !lowered[id.index()] || self.tx.atom_of(id) != Some(self.arguments) {
                continue;
            }
            let Some(b) = self
                .tx
                .sc
                .decl_of
                .get(&id)
                .copied()
                .or_else(|| self.tx.sc.binding_of_ref(id))
            else {
                continue;
            };
            let binding = self.tx.sc.binding(b);
            if matches!(
                binding.kind,
                BindingKind::Param | BindingKind::Let | BindingKind::Const | BindingKind::Catch
            ) && owners[id.index()] == owners[binding.decl.index()]
                && aliases[b as usize].is_none()
            {
                aliases[b as usize] = Some(self.unique("arguments"));
            }
        }
        if aliases.iter().all(Option::is_none) {
            return;
        }
        // Keep shorthand property names while changing their value/binding identifiers.
        for &id in &ids {
            match self.n(id) {
                Node::ShorthandProp {
                    modifiers,
                    name,
                    init,
                } => {
                    if self.tx.sc.binding_of_ref(name).is_some_and(|b| {
                        aliases[b as usize].is_some()
                            && owners[name.index()] == owners[self.tx.sc.binding(b).decl.index()]
                    }) {
                        let at = self.at(id);
                        let key = self.copy(name);
                        let value = init.map_or(name, |i| self.assign(name, i, at));
                        self.tx.set(
                            id,
                            Node::Property {
                                modifiers,
                                key,
                                value,
                            },
                        );
                    }
                }
                Node::BindingProperty {
                    key,
                    value: None,
                    init,
                } => {
                    if self
                        .tx
                        .sc
                        .decl_of
                        .get(&key)
                        .is_some_and(|&b| aliases[b as usize].is_some())
                    {
                        let value = Some(key);
                        let key = self.copy(key);
                        self.tx.set(id, Node::BindingProperty { key, value, init });
                    }
                }
                _ => {}
            }
        }
        for id in ids {
            let Some(b) = self
                .tx
                .sc
                .decl_of
                .get(&id)
                .copied()
                .or_else(|| self.tx.sc.binding_of_ref(id))
            else {
                continue;
            };
            if let Some(name) = aliases[b as usize]
                && owners[id.index()] == owners[self.tx.sc.binding(b).decl.index()]
                && self.tx.atom_of(id) == Some(self.arguments)
            {
                self.tx.set(id, Node::Ident { name });
            }
        }
    }

    /// The name `arguments` is read through in the current function, if a lowered async
    /// function captures it.
    fn arguments_capture(&mut self) -> Option<Atom> {
        // The function `arguments` belongs to captures it if it is lowered; else the
        // outermost lowered async arrow inside it does (observed).
        let owner = self.fns.iter().rposition(|f| !f.arrow);
        // A function that is not lowered shares one name among its lowered arrows, declared
        // only by the first (observed; later arrows read an undeclared name).
        let i = match owner {
            Some(o) if self.fns[o].lower_await => o,
            _ => {
                let from = owner.map_or(0, |o| o + 1);
                let i = from + self.fns[from..].iter().position(|f| f.lower_await)?;
                let shared = match owner {
                    Some(o) => self.fns[o].args_shared,
                    None => self.top_args,
                };
                if self.fns[i].args.is_none()
                    && let Some(a) = shared
                {
                    return Some(a);
                }
                i
            }
        };
        if let Some(a) = self.fns[i].args {
            return Some(a);
        }
        let a = self.unique("arguments");
        self.fns[i].args = Some(a);
        match owner {
            Some(o) if !self.fns[o].lower_await => self.fns[o].args_shared = Some(a),
            None => self.top_args = Some(a),
            _ => {}
        }
        Some(a)
    }

    /// `arguments` → `arguments_1` in a lowered async body.
    pub fn arguments_ref(&mut self, id: NodeId) {
        if let Some(a) = self.arguments_capture() {
            self.tx.set(id, Node::Ident { name: a });
        }
    }

    /// `{ arguments }` → `{ arguments: arguments_1 }` in a lowered async body.
    pub fn arguments_shorthand(&mut self, id: NodeId) {
        let Node::ShorthandProp { name, init, .. } = self.n(id) else {
            return;
        };
        if let Some(i) = init {
            self.visit(i);
        }
        if init.is_some() {
            return;
        }
        if let Some(a) = self.arguments_capture() {
            let at = self.at(id);
            let key = self.copy(name);
            let value = self.ident_atom(a, at);
            self.tx.set(
                id,
                Node::Property {
                    modifiers: Modifiers::NONE,
                    key,
                    value,
                },
            );
        }
    }

    // ---- super ----

    /// The index in `fns` of the lowered async method whose `super` the current code sees.
    pub fn super_ctx(&self) -> Option<usize> {
        let i = self.fns.iter().rposition(|f| !f.arrow)?;
        self.fns[i].sup.is_some().then_some(i)
    }

    /// `super.x` or `super[k]` (parentheses skipped) in a lowered async method.
    pub fn is_super_access(&self, callee: NodeId) -> bool {
        let c = self.skip_parens(callee);
        match self.n(c) {
            Node::Member { obj, .. } | Node::Index { obj, .. } => {
                self.n(obj) == Node::Super {} && self.super_ctx().is_some()
            }
            _ => false,
        }
    }

    /// `super.x` → `_super.x`; `super[k]` → `_superIndex(k)` (`.value` when assigning).
    pub fn super_access(&mut self, id: NodeId) {
        let Some(i) = self.super_ctx() else {
            self.children(id);
            return;
        };
        let at = self.at(id);
        match self.n(id) {
            Node::Member { prop, optional, .. } => {
                let Node::Ident { name } = self.n(prop) else {
                    return;
                };
                if let Some(s) = self.fns[i].sup.as_mut()
                    && !s.names.contains(&name)
                {
                    s.names.push(name);
                }
                let obj = self.ident("_super", at);
                self.tx.set(
                    id,
                    Node::Member {
                        obj,
                        optional,
                        prop,
                    },
                );
            }
            Node::Index { index, .. } => {
                self.visit(index);
                let assign = self.fns[i].sup.as_ref().is_some_and(|s| s.assign);
                if let Some(s) = self.fns[i].sup.as_mut() {
                    s.index = true;
                }
                let f = self.ident("_superIndex", at);
                let c = self.call(f, &[index], at);
                let r = if assign {
                    self.member(c, "value", at)
                } else {
                    c
                };
                self.put(id, r);
            }
            _ => {}
        }
    }

    /// `super.m(x)` → `_super.m.call(this, x)`.
    pub fn super_call(&mut self, id: NodeId) {
        let Node::Call {
            callee,
            optional,
            type_args,
            args,
        } = self.n(id)
        else {
            return;
        };
        let at = self.at(id);
        let c = self.skip_parens(callee);
        self.super_access(c);
        self.visit_list(args);
        let callee = self.member(c, "call", at);
        let this = self.this(at);
        let mut items = vec![this];
        items.extend(self.tx.items(args));
        let args = self.new_list(&items);
        self.tx.set(
            id,
            Node::Call {
                callee,
                optional,
                type_args,
                args,
            },
        );
    }

    /// True if `body` assigns a `super` property (outside nested non-arrow functions).
    pub fn assigns_super(&self, body: NodeId) -> bool {
        let mut stack = vec![body];
        while let Some(id) = stack.pop() {
            match self.n(id) {
                Node::FunctionDecl { .. }
                | Node::FunctionExpr { .. }
                | Node::ClassDecl { .. }
                | Node::ClassExpr { .. } => continue,
                Node::Assign { left, .. }
                | Node::Update { arg: left, .. }
                | Node::ForIn { left, .. }
                | Node::ForOf { left, .. }
                    if self.has_super_property(left) =>
                {
                    return true;
                }
                _ => {}
            }
            stack.extend(self.tx.ast.children(id));
        }
        false
    }

    fn has_super_property(&self, id: NodeId) -> bool {
        let mut stack = vec![id];
        while let Some(id) = stack.pop() {
            match self.n(id) {
                Node::FunctionDecl { .. }
                | Node::FunctionExpr { .. }
                | Node::ClassDecl { .. }
                | Node::ClassExpr { .. } => continue,
                Node::Member { obj, .. } | Node::Index { obj, .. }
                    if self.n(obj) == Node::Super {} =>
                {
                    return true;
                }
                _ => {}
            }
            stack.extend(self.tx.ast.children(id));
        }
        false
    }

    /// The statements declaring `_superIndex` and `_super` for a lowered method.
    pub fn super_decls(&mut self, sup: &SuperUse, at: u32) -> Vec<NodeId> {
        let mut out = Vec::new();
        if sup.index {
            let value = if sup.assign {
                self.super_index_cached(at)
            } else {
                // name => super[name]
                let n = self.ident("name", at);
                let p = self.param(n, at);
                let s = self.mk(Node::Super {}, at);
                let k = self.ident("name", at);
                let ix = self.index(s, k, at);
                self.arrow_expr(&[p], ix, at)
            };
            let name = self.ident("_superIndex", at);
            out.push(self.var(VarKind::Const, Modifiers::NONE, name, Some(value), at));
        }
        if !sup.names.is_empty() {
            let mut entries = Vec::with_capacity(sup.names.len());
            for &n in &sup.names {
                // get: () => super.n
                let s = self.mk(Node::Super {}, at);
                let p = self.ident_atom(n, at);
                let m = self.mk(
                    Node::Member {
                        obj: s,
                        optional: false,
                        prop: p,
                    },
                    at,
                );
                let get = self.arrow_expr(&[], m, at);
                let gp = self.prop("get", get, at);
                let mut props = vec![gp];
                if sup.assign {
                    // set: v => super.n = v
                    let v = self.ident("v", at);
                    let vp = self.param(v, at);
                    let s = self.mk(Node::Super {}, at);
                    let p = self.ident_atom(n, at);
                    let m = self.mk(
                        Node::Member {
                            obj: s,
                            optional: false,
                            prop: p,
                        },
                        at,
                    );
                    let v = self.ident("v", at);
                    let a = self.assign(m, v, at);
                    let set = self.arrow_expr(&[vp], a, at);
                    props.push(self.prop("set", set, at));
                }
                let desc = self.object(&props, at);
                let key = self.ident_atom(n, at);
                entries.push(self.prop_node(key, desc, at));
            }
            let obj = self.object(&entries, at);
            self.tx.mark_multi_line(obj);
            let create = self.dotted("Object.create", at);
            let null = self.null(at);
            let c = self.call(create, &[null, obj], at);
            let name = self.ident("_super", at);
            out.push(self.var(VarKind::Const, Modifiers::NONE, name, Some(c), at));
        }
        out
    }

    /// ```text
    /// (function (geti, seti) {
    ///     const cache = Object.create(null);
    ///     return name => cache[name] || (cache[name] = { get value() { return geti(name); }, set value(v) { seti(name, v); } });
    /// })(name => super[name], (name, value) => super[name] = value)
    /// ```
    fn super_index_cached(&mut self, at: u32) -> NodeId {
        // const cache = Object.create(null);
        let create = self.dotted("Object.create", at);
        let null = self.null(at);
        let c = self.call(create, &[null], at);
        let cache = self.ident("cache", at);
        let decl = self.var(VarKind::Const, Modifiers::NONE, cache, Some(c), at);
        // get value() { return geti(name); }
        let geti = self.ident("geti", at);
        let name = self.ident("name", at);
        let call = self.call(geti, &[name], at);
        let r = self.ret(Some(call), at);
        let key = self.ident("value", at);
        let get = self.method(Modifiers::NONE, MethodKind::Get, key, &[], &[r], at);
        self.tx.mark_single_line_body(get);
        // set value(v) { seti(name, v); }
        let seti = self.ident("seti", at);
        let name = self.ident("name", at);
        let v = self.ident("v", at);
        let call = self.call(seti, &[name, v], at);
        let st = self.expr_stmt(call, at);
        let key = self.ident("value", at);
        let v = self.ident("v", at);
        let vp = self.param(v, at);
        let set = self.method(Modifiers::NONE, MethodKind::Set, key, &[vp], &[st], at);
        self.tx.mark_single_line_body(set);
        let obj = self.object(&[get, set], at);
        // name => cache[name] || (cache[name] = obj)
        let cache = self.ident("cache", at);
        let name = self.ident("name", at);
        let read = self.index(cache, name, at);
        let cache = self.ident("cache", at);
        let name = self.ident("name", at);
        let write = self.index(cache, name, at);
        let a = self.assign(write, obj, at);
        let pa = self.paren(a, at);
        let or = self.binary(read, ts_r_ast::BinaryOp::LogicalOr, pa, at);
        let n = self.ident("name", at);
        let np = self.param(n, at);
        let arrow = self.arrow_expr(&[np], or, at);
        let r = self.ret(Some(arrow), at);
        let gi = self.ident("geti", at);
        let gp = self.param(gi, at);
        let si = self.ident("seti", at);
        let sp = self.param(si, at);
        let f = self.function_expr(None, &[gp, sp], &[decl, r], at);
        let pf = self.paren(f, at);
        // name => super[name]
        let n = self.ident("name", at);
        let np = self.param(n, at);
        let s = self.mk(Node::Super {}, at);
        let k = self.ident("name", at);
        let ix = self.index(s, k, at);
        let g = self.arrow_expr(&[np], ix, at);
        // (name, value) => super[name] = value
        let n = self.ident("name", at);
        let np = self.param(n, at);
        let v = self.ident("value", at);
        let vp = self.param(v, at);
        let s = self.mk(Node::Super {}, at);
        let k = self.ident("name", at);
        let ix = self.index(s, k, at);
        let v = self.ident("value", at);
        let a = self.assign(ix, v, at);
        let st = self.arrow_expr(&[np, vp], a, at);
        self.call(pf, &[g, st], at)
    }

    // ---- the function ----

    /// Lowers the async function, method value or arrow `id` (its body already visited).
    pub fn lower_async(&mut self, id: NodeId, method: Option<NodeId>, ctx: FnCtx) {
        let at = self.at(id);
        let (arrow, flags, params, body) = match self.n(id) {
            Node::FunctionDecl {
                flags,
                params,
                body: Some(body),
                ..
            }
            | Node::FunctionExpr {
                flags,
                params,
                body: Some(body),
                ..
            } => (false, flags, params, body),
            Node::Arrow {
                flags,
                params,
                body,
                ..
            } => (true, flags, params, body),
            _ => return,
        };
        let _ = method;
        let (outer_params, gen_params, args) = self.split_async_params(params, arrow, at);
        let this_arg = if !arrow || self.fns.iter().any(|f| !f.arrow) {
            self.this(at)
        } else {
            self.void0(at)
        };
        let gen_body = if arrow && flags.contains(FnFlags::EXPRESSION_BODY) {
            let r = self.ret(Some(body), at);
            let b = self.block(&[r], at);
            self.tx.mark_single_line(b);
            b
        } else {
            body
        };
        let generator = self.mk(
            Node::FunctionExpr {
                modifiers: Modifiers::NONE,
                flags: FnFlags::GENERATOR,
                name: None,
                type_params: None,
                params: gen_params,
                return_type: None,
                body: Some(gen_body),
            },
            at,
        );
        let f = self.tx.helper_ref(Helper::Awaiter, at);
        let void = self.void0(at);
        let call = self.call(f, &[this_arg, args, void, generator], at);
        let mut pre = self.prologue_for(&ctx, at);
        let mut flags = flags;
        flags.remove(FnFlags::ASYNC);
        let new_body = if arrow && pre.is_empty() {
            flags.insert(FnFlags::EXPRESSION_BODY);
            call
        } else {
            flags.remove(FnFlags::EXPRESSION_BODY);
            let r = self.ret(Some(call), at);
            pre.push(r);
            self.block(&pre, at)
        };
        self.set_function(id, flags, outer_params, new_body);
    }

    /// `var arguments_1 = arguments;` and the `super` declarations a lowered body needs.
    pub fn prologue_for(&mut self, ctx: &FnCtx, at: u32) -> Vec<NodeId> {
        let mut pre = Vec::new();
        if let Some(a) = ctx.args {
            let name = self.ident_atom(a, at);
            let v = self.ident("arguments", at);
            pre.push(self.var(VarKind::Var, Modifiers::NONE, name, Some(v), at));
        }
        if let Some(sup) = &ctx.sup {
            pre.extend(self.super_decls(sup, at));
        }
        pre
    }

    /// Replaces the flags, parameters and body of the function `id`.
    pub fn set_function(&mut self, id: NodeId, flags: FnFlags, params: NodeList, body: NodeId) {
        match self.n(id) {
            Node::FunctionDecl {
                modifiers,
                name,
                type_params,
                return_type,
                ..
            } => self.tx.set(
                id,
                Node::FunctionDecl {
                    modifiers,
                    flags,
                    name,
                    type_params,
                    params,
                    return_type,
                    body: Some(body),
                },
            ),
            Node::FunctionExpr {
                modifiers,
                name,
                type_params,
                return_type,
                ..
            } => self.tx.set(
                id,
                Node::FunctionExpr {
                    modifiers,
                    flags,
                    name,
                    type_params,
                    params,
                    return_type,
                    body: Some(body),
                },
            ),
            Node::Arrow {
                type_params,
                return_type,
                ..
            } => self.tx.set(
                id,
                Node::Arrow {
                    flags,
                    type_params,
                    params,
                    return_type,
                    body,
                },
            ),
            _ => {}
        }
    }

    /// The outer parameters, the generator's parameters and the `__awaiter` arguments
    /// expression for an async function's parameter list.
    pub fn split_async_params(
        &mut self,
        params: NodeList,
        arrow: bool,
        at: u32,
    ) -> (NodeList, NodeList, NodeId) {
        let items = self.tx.items(params);
        let complex = items.iter().any(|&p| match self.n(p) {
            Node::Param { pattern, init, .. } => {
                init.is_some() || !matches!(self.n(pattern), Node::Ident { .. })
            }
            _ => true,
        });
        if !complex {
            let v = self.void0(at);
            return (params, NodeList::EMPTY, v);
        }
        let mut outer = Vec::new();
        let mut refs = Vec::new();
        let mut tail = false;
        for &p in &items {
            let Node::Param {
                rest,
                pattern,
                init,
                ..
            } = self.n(p)
            else {
                continue;
            };
            if rest || init.is_some() {
                tail = true;
                break;
            }
            let name = match self.n(pattern) {
                Node::Ident { name } if self.unique_base(name).is_some() => self.unique_from(name),
                Node::Ident { name } => {
                    let text = self.tx.ast.atom(name).to_owned();
                    self.unique_local(&text)
                }
                _ => self.local_temp(),
            };
            let pat = self.ident_atom(name, at);
            outer.push(self.param(pat, at));
            refs.push(self.ident_atom(name, at));
        }
        let args = if arrow {
            if tail {
                let a = self.unique_local("args");
                let pat = self.ident_atom(a, at);
                let p = self.mk(
                    Node::Param {
                        decorators: NodeList::EMPTY,
                        modifiers: Modifiers::NONE,
                        rest: true,
                        pattern: pat,
                        optional: false,
                        type_ann: None,
                        init: None,
                    },
                    at,
                );
                outer.push(p);
                let r = self.ident_atom(a, at);
                refs.push(self.spread(r, at));
            }
            self.array(&refs, at)
        } else {
            self.ident("arguments", at)
        };
        let outer = self.new_list(&outer);
        (outer, params, args)
    }

    // ---- `var`s colliding with parameters ----

    /// In a lowered async body, a `var` statement declaring a name that is also a parameter
    /// name becomes assignments, its names hoisted into the ES2017 `var` statement (observed:
    /// `var a = 1;` → `var a;` ... `a = 1;`; a `for` initializer → `for (_a = true, c_1 =
    /// ...; ...)`). Generated names compare by their base (`c_1` collides with `c`).
    pub fn hoist_colliding_vars(&mut self, params: NodeList, body: NodeId) {
        let mut names: Vec<String> = Vec::new();
        for p in self.tx.items(params) {
            if let Node::Param { pattern, .. } = self.n(p) {
                self.binding_texts(pattern, &mut names);
            }
        }
        if names.is_empty() || !matches!(self.n(body), Node::Block { .. }) {
            return;
        }
        // The `var` statements of the newer transforms come first in the body, so a
        // colliding one (`var _a, e_1, ...` with a parameter `e`) moves first.
        for g in [Group::Es2018, Group::Chain, Group::Coalesce, Group::Es2021] {
            let group = self.frame_group(g);
            if group
                .iter()
                .any(|&a| self.decl_text(a).is_some_and(|t| names.contains(&t)))
            {
                let moved = std::mem::take(self.frame_group_mut(g));
                self.frame_group_mut(Group::Es2017).extend(moved);
            }
        }
        self.collide_stmt(body, &names);
    }

    fn binding_texts(&self, id: NodeId, out: &mut Vec<String>) {
        let mut stack = vec![id];
        while let Some(id) = stack.pop() {
            match self.n(id) {
                Node::Ident { name } => out.push(self.tx.ast.atom(name).to_owned()),
                Node::ObjectPattern { props } => stack.extend(self.tx.items(props)),
                Node::ArrayPattern { elements } => stack.extend(self.tx.items(elements)),
                Node::BindingProperty { key, value, .. } => stack.push(value.unwrap_or(key)),
                Node::AssignPattern { left, .. } => stack.push(left),
                Node::Rest { arg } => stack.push(arg),
                _ => {}
            }
        }
    }

    /// The name a declared identifier compares by: a generated name's base.
    fn decl_text(&self, name: Atom) -> Option<String> {
        if self.is_temp(name) {
            return None;
        }
        if let Some(b) = self.unique_base(name) {
            return Some(b.to_owned());
        }
        Some(self.tx.ast.atom(name).to_owned())
    }

    /// The declarators of a `var` list that collides, if every one binds an identifier.
    fn colliding(&self, decl: NodeId, names: &[String]) -> Option<Vec<NodeId>> {
        let Node::VarDecl {
            kind: VarKind::Var,
            decls,
            ..
        } = self.n(decl)
        else {
            return None;
        };
        let items = self.tx.items(decls);
        let mut hit = false;
        for &d in &items {
            let Node::VarDeclarator { name, .. } = self.n(d) else {
                return None;
            };
            let Node::Ident { name } = self.n(name) else {
                return None;
            };
            if self.decl_text(name).is_some_and(|t| names.contains(&t)) {
                hit = true;
            }
        }
        hit.then_some(items)
    }

    /// Hoists the names of `decls` and returns their initializations as one expression.
    fn hoist_decls(&mut self, decls: &[NodeId], at: u32) -> Option<NodeId> {
        let mut inits = Vec::new();
        for &d in decls {
            let Node::VarDeclarator { name, init, .. } = self.n(d) else {
                continue;
            };
            let Node::Ident { name: atom } = self.n(name) else {
                continue;
            };
            self.hoist(atom, Group::Es2017);
            if let Some(i) = init {
                let t = self.ident_atom(atom, self.at(name));
                inits.push(self.assign(t, i, at));
            }
        }
        match inits.len() {
            0 => None,
            1 => Some(inits[0]),
            _ => Some(self.seq(&inits, at)),
        }
    }

    /// Rewrites colliding `var`s in statement `id`; false if the statement goes away.
    fn collide_stmt(&mut self, id: NodeId, names: &[String]) -> bool {
        let at = self.at(id);
        match self.n(id) {
            Node::FunctionDecl { .. } | Node::ClassDecl { .. } => true,
            Node::VarDecl { .. } => {
                let Some(decls) = self.colliding(id, names) else {
                    return true;
                };
                match self.hoist_decls(&decls, at) {
                    Some(e) => {
                        self.tx.set(id, Node::ExprStmt { expr: e });
                        true
                    }
                    None => false,
                }
            }
            Node::Block { stmts } => {
                let items = self.tx.items(stmts);
                let mut kept = Vec::with_capacity(items.len());
                for s in &items {
                    if self.collide_stmt(*s, names) {
                        kept.push(*s);
                    }
                }
                if kept.len() != items.len() {
                    let stmts = self.new_list(&kept);
                    self.tx.set(id, Node::Block { stmts });
                }
                true
            }
            Node::If { cons, alt, .. } => {
                self.collide_embedded(cons, names);
                if let Some(a) = alt {
                    self.collide_embedded(a, names);
                }
                true
            }
            Node::For {
                init,
                test,
                update,
                body,
            } => {
                if let Some(i) = init
                    && let Some(decls) = self.colliding(i, names)
                {
                    let e = self.hoist_decls(&decls, at);
                    self.tx.set(
                        id,
                        Node::For {
                            init: e,
                            test,
                            update,
                            body,
                        },
                    );
                }
                self.collide_embedded(body, names);
                true
            }
            Node::ForIn { left, body, .. } | Node::ForOf { left, body, .. } => {
                if let Some(decls) = self.colliding(left, names)
                    && let [d] = decls[..]
                    && let Node::VarDeclarator {
                        name, init: None, ..
                    } = self.n(d)
                    && let Node::Ident { name: atom } = self.n(name)
                {
                    self.hoist(atom, Group::Es2017);
                    self.put(left, name);
                }
                self.collide_embedded(body, names);
                true
            }
            Node::While { body, .. }
            | Node::DoWhile { body, .. }
            | Node::Labeled { body, .. }
            | Node::With { body, .. } => {
                self.collide_embedded(body, names);
                true
            }
            Node::Try {
                block,
                handler,
                finalizer,
            } => {
                self.collide_stmt(block, names);
                if let Some(h) = handler
                    && let Node::CatchClause { body, .. } = self.n(h)
                {
                    self.collide_stmt(body, names);
                }
                if let Some(f) = finalizer {
                    self.collide_stmt(f, names);
                }
                true
            }
            Node::Switch { cases, .. } => {
                for c in self.tx.items(cases) {
                    let Node::SwitchCase { test, body } = self.n(c) else {
                        continue;
                    };
                    let items = self.tx.items(body);
                    let mut kept = Vec::with_capacity(items.len());
                    for s in &items {
                        if self.collide_stmt(*s, names) {
                            kept.push(*s);
                        }
                    }
                    if kept.len() != items.len() {
                        let body = self.new_list(&kept);
                        self.tx.set(c, Node::SwitchCase { test, body });
                    }
                }
                true
            }
            _ => true,
        }
    }

    fn collide_embedded(&mut self, id: NodeId, names: &[String]) {
        if !self.collide_stmt(id, names) {
            self.tx.set(id, Node::Empty {});
        }
    }

    /// `!x`
    pub fn not(&mut self, x: NodeId, at: u32) -> NodeId {
        self.unary(UnaryOp::Not, x, at)
    }
}
