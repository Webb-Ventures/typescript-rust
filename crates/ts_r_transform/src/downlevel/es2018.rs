//! ES2018: async generators, `for await` and object spread (observed, tsc 7.0.2).
//!
//! - `async function* g(a) { body }` → `function g(a) { return __asyncGenerator(this,
//!   arguments, function* g_1() { body }); }`: parameters split as for async functions; the generator is
//!   named after the function or method (`m_1`), an anonymous one stays anonymous; both
//!   bodies keep the original's one-line layout. Inside, `await x` → `yield __await(x)`,
//!   `yield x` → `yield yield __await(x)`, `yield* x` →
//!   `yield __await(yield* __asyncDelegator(__asyncValues(x)))`, `return x` →
//!   `return yield __await(x)` (`void 0` for a missing operand).
//! - `for await (const x of it) body` →
//!   ```text
//!   try {
//!       for (var _d = true, it_1 = __asyncValues(it), it_1_1; it_1_1 = yield it_1.next(), _a = it_1_1.done, !_a; _d = true) {
//!           _c = it_1_1.value;
//!           _d = false;
//!           const x = _c;
//!           body
//!       }
//!   }
//!   catch (e_1_1) { e_1 = { error: e_1_1 }; }
//!   finally {
//!       try {
//!           if (!_d && !_a && (_b = it_1.return)) yield _b.call(it_1);
//!       }
//!       finally { if (e_1) throw e_1.error; }
//!   }
//!   ```
//!   with `var _a, e_1, _b, _c;` hoisted; the iterator is named after an identifier operand
//!   (`it_1`, `it_1_1`), else temporaries. A label stays on the inner `for`.
//! - `{ a, ...b, c }` → `Object.assign(Object.assign({ a }, b), { c })`; a leading spread
//!   starts from `{}`.

use ts_r_ast::{FnFlags, Modifiers, Node, NodeId, NodeList, VarKind};

use super::lower::{FnCtx, Group, Lower};
use crate::builder::Build;
use crate::helpers::Helper;

impl Lower<'_, '_> {
    // ---- async generators ----

    fn await_call(&mut self, e: NodeId, at: u32) -> NodeId {
        let f = self.tx.helper_ref(Helper::Await, at);
        self.call(f, &[e], at)
    }

    fn yield_of(&mut self, arg: NodeId, at: u32) -> NodeId {
        self.mk(
            Node::Yield {
                delegate: false,
                arg: Some(arg),
            },
            at,
        )
    }

    /// `await x` → `yield __await(x)`.
    pub fn await_in_async_gen(&mut self, id: NodeId) {
        let Node::Await { arg } = self.n(id) else {
            return;
        };
        self.visit(arg);
        let at = self.at(id);
        let c = self.await_call(arg, at);
        self.tx.set(
            id,
            Node::Yield {
                delegate: false,
                arg: Some(c),
            },
        );
    }

    /// `yield x` → `yield yield __await(x)`; `yield* x` →
    /// `yield __await(yield* __asyncDelegator(__asyncValues(x)))`.
    pub fn yield_in_async_gen(&mut self, id: NodeId) {
        let Node::Yield { delegate, arg } = self.n(id) else {
            return;
        };
        if let Some(a) = arg {
            self.visit(a);
        }
        let at = self.at(id);
        let a = match arg {
            Some(a) => a,
            None => self.void0(at),
        };
        let value = if delegate {
            let v = self.tx.helper_ref(Helper::AsyncValues, at);
            let v = self.call(v, &[a], at);
            let d = self.tx.helper_ref(Helper::AsyncDelegator, at);
            let d = self.call(d, &[v], at);
            let ys = self.mk(
                Node::Yield {
                    delegate: true,
                    arg: Some(d),
                },
                at,
            );
            self.await_call(ys, at)
        } else {
            let c = self.await_call(a, at);
            self.yield_of(c, at)
        };
        self.tx.set(
            id,
            Node::Yield {
                delegate: false,
                arg: Some(value),
            },
        );
    }

    /// `return x` → `return yield __await(x)`.
    pub fn return_in_async_gen(&mut self, id: NodeId) {
        let Node::Return { arg } = self.n(id) else {
            return;
        };
        if let Some(a) = arg {
            self.visit(a);
        }
        let at = self.at(id);
        let a = match arg {
            Some(a) => a,
            None => self.void0(at),
        };
        let c = self.await_call(a, at);
        let y = self.yield_of(c, at);
        self.tx.set(id, Node::Return { arg: Some(y) });
    }

    /// Lowers the async generator function or method value `id` (its body already visited).
    pub fn lower_async_gen(&mut self, id: NodeId, method: Option<NodeId>, ctx: FnCtx) {
        let at = self.at(id);
        let (flags, name, params, body) = match self.n(id) {
            Node::FunctionDecl {
                flags,
                name,
                params,
                body: Some(body),
                ..
            }
            | Node::FunctionExpr {
                flags,
                name,
                params,
                body: Some(body),
                ..
            } => (flags, name, params, body),
            _ => return,
        };
        let Node::Block { stmts } = self.n(body) else {
            return;
        };
        let one_line = self.one_line_body(body) || self.tx.single_line.contains(&body);
        let base = match (name, method.map(|m| self.n(m))) {
            (Some(n), _) => Some(self.tx.name(n).to_owned()),
            (None, Some(Node::MethodDef { key, .. })) => match self.n(key) {
                Node::Ident { .. } | Node::StringLit { .. } => Some(self.tx.name(key).to_owned()),
                _ => None,
            },
            _ => None,
        };
        let gen_name = base.filter(|b| !b.is_empty()).map(|b| {
            let u = self.unique(&b);
            self.ident_atom(u, at)
        });
        let gen_body = self.mk(Node::Block { stmts }, at);
        if one_line {
            self.tx.mark_single_line(gen_body);
        }
        // Parameters split as for async functions (`f(x_1) { ... function* f_1(x, y = z)`).
        let (params, gen_params, _) = self.split_async_params(params, false, at);
        let generator = self.mk(
            Node::FunctionExpr {
                modifiers: Modifiers::NONE,
                flags: FnFlags::GENERATOR,
                name: gen_name,
                type_params: None,
                params: gen_params,
                return_type: None,
                body: Some(gen_body),
            },
            at,
        );
        let f = self.tx.helper_ref(Helper::AsyncGenerator, at);
        let this = self.this(at);
        let args = self.ident("arguments", at);
        let call = self.call(f, &[this, args, generator], at);
        let mut pre = self.prologue_for(&ctx, at);
        let r = self.ret(Some(call), at);
        pre.push(r);
        let stmts = self.new_list(&pre);
        self.tx.set(body, Node::Block { stmts });
        if one_line {
            self.tx.mark_single_line(body);
        }
        let mut flags = flags;
        flags.remove(FnFlags::ASYNC);
        flags.remove(FnFlags::GENERATOR);
        self.set_function(id, flags, params, body);
    }

    // ---- for await ----

    /// `for await (...)` (the `Labeled` around it when `label` is set).
    pub fn for_await(&mut self, id: NodeId, label: Option<NodeId>) {
        let target = label.map_or(id, |l| match self.n(l) {
            Node::Labeled { body, .. } => body,
            _ => id,
        });
        let Node::ForOf {
            is_await: true,
            left,
            right,
            body,
        } = self.n(target)
        else {
            return;
        };
        let at = self.at(target);
        let in_loop = self.loops > 0;
        self.visit(right);
        self.visit(left);
        let rest_stmt = if self.left_has_rest(left) {
            self.rest_for_left(left)
        } else {
            None
        };
        self.loops += 1;
        self.visit(body);
        self.loops -= 1;
        let done = self.temp(Group::Es2018);
        let err = self.unique("e");
        self.hoist(err, Group::Es2018);
        let ret = self.temp(Group::Es2018);
        let value = self.temp(Group::Es2018);
        let flag = self.local_temp();
        let right_core = self.skip_parens(right);
        let (iter, step) = match self.n(right_core) {
            Node::Ident { name } => {
                let text = self.tx.ast.atom(name).to_owned();
                let it = self.unique(&text);
                let step = self.unique_from(it);
                (it, step)
            }
            _ => (self.local_temp(), self.local_temp()),
        };
        // var _d = true, it_1 = __asyncValues(it), it_1_1
        let t = self.bool_lit(true, at);
        let d1 = self.declarator(flag, Some(t), at);
        let av = self.tx.helper_ref(Helper::AsyncValues, at);
        let mut av = self.call(av, &[right], at);
        if in_loop {
            // In a loop, the error from the previous run is cleared first (observed):
            // `it_1 = (e_1 = void 0, __asyncValues(it))`.
            let e = self.ident_atom(err, at);
            let v = self.void0(at);
            let reset = self.assign(e, v, at);
            let s = self.seq(&[reset, av], at);
            av = self.paren(s, at);
        }
        let d2 = self.declarator(iter, Some(av), at);
        let d3 = self.declarator(step, None, at);
        let decls = self.new_list(&[d1, d2, d3]);
        let init = self.mk(
            Node::VarDecl {
                modifiers: Modifiers::NONE,
                kind: VarKind::Var,
                decls,
            },
            at,
        );
        // it_1_1 = await it_1.next(), _a = it_1_1.done, !_a
        let it = self.ident_atom(iter, at);
        let next = self.member(it, "next", at);
        let next = self.call(next, &[], at);
        let next = self.awaited(next, at);
        let s = self.ident_atom(step, at);
        let a1 = self.assign(s, next, at);
        let dn = self.ident_atom(done, at);
        let s = self.ident_atom(step, at);
        let sd = self.member(s, "done", at);
        let a2 = self.assign(dn, sd, at);
        let dn = self.ident_atom(done, at);
        let nd = self.not(dn, at);
        let test = self.seq(&[a1, a2, nd], at);
        // _d = true
        let f = self.ident_atom(flag, at);
        let t = self.bool_lit(true, at);
        let update = self.assign(f, t, at);
        // body: _c = it_1_1.value; _d = false; const x = _c; ...
        let v = self.ident_atom(value, at);
        let s = self.ident_atom(step, at);
        let sv = self.member(s, "value", at);
        let a = self.assign(v, sv, at);
        let mut stmts = vec![self.expr_stmt(a, at)];
        let f = self.ident_atom(flag, at);
        let fl = self.bool_lit(false, at);
        let a = self.assign(f, fl, at);
        stmts.push(self.expr_stmt(a, at));
        stmts.extend(self.for_await_binding(left, value, at));
        stmts.extend(rest_stmt);
        match self.n(body) {
            Node::Block { stmts: b } => stmts.extend(self.tx.items(b)),
            _ => stmts.push(body),
        }
        let loop_body = self.block(&stmts, at);
        let mut loop_stmt = self.mk(
            Node::For {
                init: Some(init),
                test: Some(test),
                update: Some(update),
                body: loop_body,
            },
            at,
        );
        if let Some(l) = label
            && let Node::Labeled { label: name, .. } = self.n(l)
        {
            loop_stmt = self.mk(
                Node::Labeled {
                    label: name,
                    body: loop_stmt,
                },
                at,
            );
        }
        let try_block = self.block(&[loop_stmt], at);
        // catch (e_1_1) { e_1 = { error: e_1_1 }; }
        let caught = self.unique_from(err);
        let cp = self.ident_atom(caught, at);
        let e = self.ident_atom(err, at);
        let cv = self.ident_atom(caught, at);
        let ep = self.prop("error", cv, at);
        let eo = self.object(&[ep], at);
        let a = self.assign(e, eo, at);
        let st = self.expr_stmt(a, at);
        let cbody = self.block(&[st], at);
        self.tx.mark_single_line(cbody);
        let handler = self.mk(
            Node::CatchClause {
                param: Some(cp),
                type_ann: None,
                body: cbody,
            },
            at,
        );
        // if (!_d && !_a && (_b = it_1.return)) await _b.call(it_1);
        let f = self.ident_atom(flag, at);
        let nf = self.not(f, at);
        let dn = self.ident_atom(done, at);
        let nd = self.not(dn, at);
        let and1 = self.binary(nf, ts_r_ast::BinaryOp::LogicalAnd, nd, at);
        let r = self.ident_atom(ret, at);
        let it = self.ident_atom(iter, at);
        let ir = self.member(it, "return", at);
        let ra = self.assign(r, ir, at);
        let ra = self.paren(ra, at);
        let cond = self.binary(and1, ts_r_ast::BinaryOp::LogicalAnd, ra, at);
        let r = self.ident_atom(ret, at);
        let rc = self.member(r, "call", at);
        let it = self.ident_atom(iter, at);
        let rc = self.call(rc, &[it], at);
        let rc = self.awaited(rc, at);
        let rs = self.expr_stmt(rc, at);
        self.tx.mark_single_line(rs);
        let iff = self.mk(
            Node::If {
                test: cond,
                cons: rs,
                alt: None,
            },
            at,
        );
        let inner_try = self.block(&[iff], at);
        // finally { if (e_1) throw e_1.error; }
        let e = self.ident_atom(err, at);
        let e2 = self.ident_atom(err, at);
        let ee = self.member(e2, "error", at);
        let th = self.mk(Node::Throw { arg: ee }, at);
        self.tx.mark_single_line(th);
        let iff = self.mk(
            Node::If {
                test: e,
                cons: th,
                alt: None,
            },
            at,
        );
        let fin = self.block(&[iff], at);
        self.tx.mark_single_line(fin);
        let inner = self.mk(
            Node::Try {
                block: inner_try,
                handler: None,
                finalizer: Some(fin),
            },
            at,
        );
        let outer_fin = self.block(&[inner], at);
        let t = self.mk(
            Node::Try {
                block: try_block,
                handler: Some(handler),
                finalizer: Some(outer_fin),
            },
            at,
        );
        self.put(label.unwrap_or(id), t);
    }

    /// The statements binding the loop variable of a lowered `for await` to `value`.
    fn for_await_binding(&mut self, left: NodeId, value: ts_r_ast::Atom, at: u32) -> Vec<NodeId> {
        match self.n(left) {
            Node::VarDecl {
                modifiers,
                kind,
                decls,
            } => {
                let items = self.tx.items(decls);
                let Some(&d) = items.first() else {
                    return Vec::new();
                };
                let Node::VarDeclarator {
                    name,
                    definite,
                    type_ann,
                    ..
                } = self.n(d)
                else {
                    return Vec::new();
                };
                let v = self.ident_atom(value, at);
                let nd = self.mk(
                    Node::VarDeclarator {
                        name,
                        definite,
                        type_ann,
                        init: Some(v),
                    },
                    at,
                );
                let decls = self.new_list(&[nd]);
                let decl = self.mk(
                    Node::VarDecl {
                        modifiers,
                        kind,
                        decls,
                    },
                    at,
                );
                vec![decl]
            }
            _ => {
                let v = self.ident_atom(value, at);
                let a = self.assign(left, v, at);
                vec![self.expr_stmt(a, at)]
            }
        }
    }

    pub fn declarator(&mut self, name: ts_r_ast::Atom, init: Option<NodeId>, at: u32) -> NodeId {
        let name = self.ident_atom(name, at);
        self.mk(
            Node::VarDeclarator {
                name,
                definite: false,
                type_ann: None,
                init,
            },
            at,
        )
    }

    // ---- object spread ----

    pub fn has_spread(&self, props: NodeList) -> bool {
        self.tx
            .ast
            .list(props)
            .iter()
            .any(|&p| matches!(self.n(p), Node::Spread { .. }))
    }

    /// An object-rest destructuring assignment in the properties, outside functions and
    /// nested object literals: the literal still becomes `Object.assign({ .. })` (observed,
    /// also for a field's `Object.defineProperty` descriptor).
    pub fn holds_rest_assign(&self, props: NodeList) -> bool {
        if !self.dots {
            return false;
        }
        let mut stack: Vec<NodeId> = self.tx.ast.list(props).to_vec();
        while let Some(id) = stack.pop() {
            match self.n(id) {
                Node::ObjectLit { .. }
                | Node::FunctionExpr { .. }
                | Node::FunctionDecl { .. }
                | Node::Arrow { .. }
                | Node::ClassExpr { .. }
                | Node::ClassDecl { .. }
                | Node::MethodDef { .. } => {}
                Node::Assign {
                    op: ts_r_ast::AssignOp::Assign,
                    left,
                    right,
                } if matches!(self.n(left), Node::ObjectLit { .. } | Node::ArrayLit { .. }) => {
                    if self.has_object_rest(left) {
                        return true;
                    }
                    stack.push(right);
                }
                _ => stack.extend(self.tx.ast.children(id)),
            }
        }
        false
    }

    /// `{ a, ...b, c }` → `Object.assign(Object.assign({ a }, b), { c })`.
    pub fn object_spread(&mut self, id: NodeId) {
        let Node::ObjectLit { props } = self.n(id) else {
            return;
        };
        self.children(id);
        let at = self.at(id);
        // Runs of properties become object literals, spreads their operands; `{}` goes first
        // unless the first part is an object literal; then `Object.assign` folds them
        // pairwise (one part: `Object.assign(part)`; observed).
        let mut parts: Vec<NodeId> = Vec::new();
        let mut cur: Vec<NodeId> = Vec::new();
        for p in self.tx.items(props) {
            let Node::Spread { arg } = self.n(p) else {
                cur.push(p);
                continue;
            };
            if !cur.is_empty() {
                let o = self.object(&cur, at);
                parts.push(o);
                cur.clear();
            }
            parts.push(arg);
        }
        if !cur.is_empty() {
            let o = self.object(&cur, at);
            parts.push(o);
        }
        let first_is_object = parts
            .first()
            .is_some_and(|&p| matches!(self.n(p), Node::ObjectLit { .. }));
        if !first_is_object {
            let o = self.object(&[], at);
            parts.insert(0, o);
        }
        let f = self.dotted("Object.assign", at);
        let mut r = match parts.get(1) {
            Some(&second) => self.call(f, &[parts[0], second], at),
            None => self.call(f, &[parts[0]], at),
        };
        for &p in parts.iter().skip(2) {
            r = self.object_assign(r, p, at);
        }
        self.put(id, r);
    }

    fn object_assign(&mut self, a: NodeId, b: NodeId, at: u32) -> NodeId {
        let f = self.dotted("Object.assign", at);
        self.call(f, &[a, b], at)
    }
}
