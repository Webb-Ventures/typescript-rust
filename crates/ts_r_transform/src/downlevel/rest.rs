//! ES2018 object rest (observed, tsc 7.0.2).
//!
//! - Declarations: `const { a, ...r } = o;` → `const { a } = o, r = __rest(o, ["a"]);`; a
//!   non-simple initializer is captured in the list first (`const _a = f(), { a } = _a, ...`);
//!   a property holding a nested rest is read into a temporary
//!   (`_b = o.d, { e } = _b, r4 = __rest(_b, ["e"]), r5 = __rest(o, ["d"])`), as is an array
//!   element (`const [_a] = o, r = __rest(_a, [])`).
//! - Parameters: `function f({ a, ...r }) {}` → `function f(_a) { var { a } = _a, r = __rest(_a, ["a"]); }`
//!   (an initializer stays on the parameter; an expression-bodied arrow gets a block).
//! - `catch ({ a, ...r })` → `catch (_a) { var { a } = _a, r = __rest(_a, ["a"]); }`.
//! - `for (const { a, ...r } of xs)` → `for (let _a of xs) { const { a } = _a, r = ...; }`.
//! - Assignments: `({ a, ...r } = o)` → `({ a } = o, r = __rest(o, ["a"]))`, a non-simple
//!   value captured in a hoisted temporary, and the value appended when it is used:
//!   `v = ({ a } = o, r = __rest(o, ["a"]), o)`.
//!
//! - Defaults: `{ x: { a, ...b } = d }` reads `_a = o.x`, then `_b = _a === void 0 ? d : _a`
//!   (inline in a one-element pattern when `d` is a name or literal); after an element read
//!   into a temporary, later array elements other than names (or names with literal
//!   defaults) are read into temporaries too and assigned in order.
//!
//! A computed key inside a pattern nested under the rest is not flattened as tsc does.

use ts_r_ast::{Modifiers, Node, NodeId, NodeList, VarKind};

use super::lower::{Group, Lower};
use crate::builder::Build;
use crate::helpers::Helper;

/// One `target = value` produced by flattening a pattern.
type Pair = (NodeId, NodeId);

impl Lower<'_, '_> {
    /// True if the binding pattern `id` holds an object rest at any depth.
    pub fn has_object_rest(&self, id: NodeId) -> bool {
        let mut stack = vec![id];
        while let Some(id) = stack.pop() {
            match self.n(id) {
                Node::ObjectPattern { props } => {
                    for p in self.tx.items(props) {
                        match self.n(p) {
                            Node::Rest { .. } => return true,
                            Node::BindingProperty { value: Some(v), .. } => stack.push(v),
                            _ => {}
                        }
                    }
                }
                Node::ArrayPattern { elements } => stack.extend(self.tx.items(elements)),
                Node::AssignPattern { left, .. } => stack.push(left),
                Node::Rest { arg } => stack.push(arg),
                // Assignment targets (expression form).
                Node::ObjectLit { props } => {
                    for p in self.tx.items(props) {
                        match self.n(p) {
                            Node::Spread { .. } => return true,
                            Node::Property { value, .. } => stack.push(value),
                            _ => {}
                        }
                    }
                }
                Node::ArrayLit { elements } => stack.extend(self.tx.items(elements)),
                Node::Spread { arg } => stack.push(arg),
                Node::Assign { left, .. } => stack.push(left),
                Node::Paren { expr } => stack.push(expr),
                _ => {}
            }
        }
        false
    }

    /// The key text a rest excludes (`"a"`, `"1"`), or `None` for a computed key.
    fn rest_key(&mut self, key: NodeId, at: u32) -> Option<NodeId> {
        let text = match self.n(key) {
            Node::Ident { name } => self.tx.ast.atom(name).to_owned(),
            Node::StringLit { value } => self.tx.ast.atom(value).to_owned(),
            Node::NumberLit { value } => ts_r_codegen::literals::js_number(value.get()),
            _ => return None,
        };
        Some(self.string(&text, at))
    }

    /// `typeof t === "symbol" ? t : t + ""`
    fn symbol_or_string(&mut self, t: ts_r_ast::Atom, at: u32) -> NodeId {
        let a = self.ident_atom(t, at);
        let ty = self.unary(ts_r_ast::UnaryOp::TypeOf, a, at);
        let s = self.string("symbol", at);
        let test = self.binary(ty, ts_r_ast::BinaryOp::StrictEq, s, at);
        let a = self.ident_atom(t, at);
        let b = self.ident_atom(t, at);
        let e = self.string("", at);
        let plus = self.binary(b, ts_r_ast::BinaryOp::Add, e, at);
        self.mk(
            Node::Conditional {
                test,
                cons: a,
                alt: plus,
            },
            at,
        )
    }

    fn rest_call(&mut self, value: NodeId, keys: Vec<NodeId>, at: u32) -> NodeId {
        let f = self.tx.helper_ref(Helper::Rest, at);
        let arr = self.array(&keys, at);
        self.call(f, &[value, arr], at)
    }

    /// `value` as a reusable reference: itself if simple, else a temporary assigned first
    /// (declared by the caller's list when `local`, else hoisted).
    fn reusable(&mut self, value: NodeId, local: bool, out: &mut Vec<Pair>) -> NodeId {
        if self.is_simple(value) {
            return value;
        }
        let at = self.at(value);
        let t = if local {
            self.local_temp()
        } else {
            self.temp(Group::Es2018)
        };
        let target = self.ident_atom(t, at);
        out.push((target, value));
        self.ident_atom(t, at)
    }

    /// Flattens `pat = value` into pairs without object rests; false if a form is not
    /// supported. `local`: temporaries are declared by the pairs (declarations), else hoisted.
    fn flatten(&mut self, pat: NodeId, value: NodeId, local: bool, out: &mut Vec<Pair>) -> bool {
        // A target with a default: `_b = _a === void 0 ? d : _a`, then the target (observed).
        let defaulted = match self.n(pat) {
            Node::AssignPattern { left, right }
            | Node::Assign {
                op: ts_r_ast::AssignOp::Assign,
                left,
                right,
            } => Some((left, right)),
            _ => None,
        };
        if let Some((left, right)) = defaulted {
            let at = self.at(pat);
            let v = self.reusable(value, local, out);
            let test_v = self.copy(v);
            let void = self.void0(at);
            let test = self.binary(test_v, ts_r_ast::BinaryOp::StrictEq, void, at);
            let alt = self.copy(v);
            let cond = self.mk(
                Node::Conditional {
                    test,
                    cons: right,
                    alt,
                },
                at,
            );
            // A name takes the value as is, as does an object pattern of one element when
            // the default is a name or a literal; other patterns read it from a temporary
            // (observed: `__rest(_a === void 0 ? d : _a, [])`, but `_b = _a === void 0 ? {}
            // : _a, q = __rest(_b, [])`).
            let simple_default = matches!(
                self.n(right),
                Node::Ident { .. }
                    | Node::StringLit { .. }
                    | Node::NumberLit { .. }
                    | Node::BigIntLit { .. }
                    | Node::BoolLit { .. }
                    | Node::NullLit {}
            );
            let single = match self.n(left) {
                Node::ObjectPattern { props } | Node::ObjectLit { props } => {
                    simple_default && self.tx.ast.list(props).len() == 1
                }
                Node::ArrayPattern { .. } | Node::ArrayLit { .. } => false,
                _ => {
                    out.push((left, cond));
                    return true;
                }
            };
            if single {
                return self.flatten(left, cond, local, out);
            }
            let t = if local {
                self.local_temp()
            } else {
                self.temp(Group::Es2018)
            };
            let target = self.ident_atom(t, at);
            out.push((target, cond));
            let r = self.ident_atom(t, at);
            return self.flatten(left, r, local, out);
        }
        if !self.has_object_rest(pat) {
            let empty = match self.n(pat) {
                Node::ObjectPattern { props } | Node::ObjectLit { props } => props.is_empty(),
                Node::ArrayPattern { elements } | Node::ArrayLit { elements } => {
                    elements.is_empty()
                }
                _ => false,
            };
            if !empty || out.is_empty() {
                out.push((pat, value));
            }
            return true;
        }
        let at = self.at(pat);
        match self.n(pat) {
            Node::ObjectPattern { props } | Node::ObjectLit { props } => {
                let decl = matches!(self.n(pat), Node::ObjectPattern { .. });
                let items = self.tx.items(props);
                // A computed key reads the value by index, so the value is always captured
                // (observed: `const _a = o, _b = k, c = _a[_b], r = __rest(_a, [typeof _b
                // === "symbol" ? _b : _b + ""])`).
                let computed = items.iter().any(|&p| match self.n(p) {
                    Node::BindingProperty { key, .. } | Node::Property { key, .. } => {
                        matches!(self.n(key), Node::ComputedKey { .. })
                    }
                    _ => false,
                });
                let value = if computed && self.is_simple(value) {
                    let t = if local {
                        self.local_temp()
                    } else {
                        self.temp(Group::Es2018)
                    };
                    let target = self.ident_atom(t, at);
                    out.push((target, value));
                    self.ident_atom(t, at)
                } else if self.value_uses(&items) > 1 {
                    self.reusable(value, local, out)
                } else {
                    value
                };
                let mut chunk: Vec<NodeId> = Vec::new();
                let mut keys: Vec<NodeId> = Vec::new();
                for p in items {
                    match self.n(p) {
                        Node::BindingProperty {
                            key,
                            value: Some(v),
                            init: None,
                        }
                        | Node::Property { key, value: v, .. }
                            if matches!(self.n(key), Node::ComputedKey { .. }) =>
                        {
                            let Node::ComputedKey { expr } = self.n(key) else {
                                return false;
                            };
                            self.flush_chunk(&mut chunk, decl, value, out, at);
                            let t = if local {
                                self.local_temp()
                            } else {
                                self.temp(Group::Es2018)
                            };
                            let kt = self.ident_atom(t, at);
                            out.push((kt, expr));
                            keys.push(self.symbol_or_string(t, at));
                            let obj = self.copy(value);
                            let k = self.ident_atom(t, at);
                            let read = self.index(obj, k, at);
                            if !self.flatten(v, read, local, out) {
                                return false;
                            }
                        }
                        Node::BindingProperty {
                            key,
                            value: v,
                            init,
                        } => {
                            let Some(k) = self.rest_key(key, at) else {
                                return false;
                            };
                            keys.push(k);
                            match v {
                                Some(v) if self.has_object_rest(v) => {
                                    self.flush_chunk(&mut chunk, decl, value, out, at);
                                    // `{ x: { a, ...b } = d }`: the default applies to the
                                    // value read.
                                    let v = match init {
                                        Some(right) => {
                                            let vat = self.at(v);
                                            self.mk(Node::AssignPattern { left: v, right }, vat)
                                        }
                                        None => v,
                                    };
                                    if !self.flatten_property(key, v, value, local, out) {
                                        return false;
                                    }
                                }
                                _ => chunk.push(p),
                            }
                        }
                        Node::Property { key, value: v, .. } => {
                            let Some(k) = self.rest_key(key, at) else {
                                return false;
                            };
                            keys.push(k);
                            if self.has_object_rest(v) {
                                self.flush_chunk(&mut chunk, decl, value, out, at);
                                if !self.flatten_property(key, v, value, local, out) {
                                    return false;
                                }
                            } else {
                                chunk.push(p);
                            }
                        }
                        Node::ShorthandProp { name, .. } => {
                            let Some(k) = self.rest_key(name, at) else {
                                return false;
                            };
                            keys.push(k);
                            chunk.push(p);
                        }
                        Node::Rest { arg } | Node::Spread { arg } => {
                            self.flush_chunk(&mut chunk, decl, value, out, at);
                            let v = self.copy(value);
                            let call = self.rest_call(v, std::mem::take(&mut keys), at);
                            // A pattern (not parenthesized) is read from a temporary
                            // (observed: `({ ...{} } = o)` → `(_a = __rest(o, []))`).
                            let call = if matches!(
                                self.n(arg),
                                Node::ObjectPattern { .. }
                                    | Node::ObjectLit { .. }
                                    | Node::ArrayPattern { .. }
                                    | Node::ArrayLit { .. }
                            ) {
                                let t = if local {
                                    self.local_temp()
                                } else {
                                    self.temp(Group::Es2018)
                                };
                                let target = self.ident_atom(t, at);
                                out.push((target, call));
                                self.ident_atom(t, at)
                            } else {
                                call
                            };
                            if !self.flatten(arg, call, local, out) {
                                return false;
                            }
                        }
                        _ => return false,
                    }
                }
                self.flush_chunk(&mut chunk, decl, value, out, at);
                true
            }
            Node::ArrayPattern { elements } | Node::ArrayLit { elements } => {
                let decl = matches!(self.n(pat), Node::ArrayPattern { .. });
                let items = self.tx.items(elements);
                let mut later: Vec<(NodeId, NodeId)> = Vec::new();
                let mut new_items = Vec::with_capacity(items.len());
                // After an element read into a temporary, a later element that is not a
                // plain name is too, so it is assigned in order (observed: `let [_f, _g] =
                // [{ x: 1 }], a = __rest(_f, []), b = _g === void 0 ? a : _g`).
                let mut prior = false;
                for e in items {
                    let defaulted_rest = matches!(self.n(e),
                        Node::AssignPattern { left, .. } | Node::Assign { left, .. }
                            if self.has_object_rest(left));
                    let late = prior && !self.simple_element(e);
                    if (self.has_object_rest(e)
                        && matches!(
                            self.n(e),
                            Node::ObjectPattern { .. } | Node::ObjectLit { .. }
                        ))
                        || defaulted_rest
                        || late
                    {
                        prior = true;
                        let t = if local {
                            self.local_temp()
                        } else {
                            self.temp(Group::Es2018)
                        };
                        new_items.push(self.ident_atom(t, at));
                        let r = self.ident_atom(t, at);
                        later.push((e, r));
                    } else if self.has_object_rest(e) {
                        return false;
                    } else {
                        new_items.push(e);
                    }
                }
                let list = self.new_list(&new_items);
                let arr = if decl {
                    self.mk(Node::ArrayPattern { elements: list }, at)
                } else {
                    self.mk(Node::ArrayLit { elements: list }, at)
                };
                out.push((arr, value));
                for (e, r) in later {
                    if !self.flatten(e, r, local, out) {
                        return false;
                    }
                }
                true
            }
            Node::Paren { expr } => self.flatten(expr, value, local, out),
            _ => false,
        }
    }

    /// An array element kept in place after a flattened one: a name, a hole or a rest, or a
    /// name whose default is a literal or keyword (observed: `[_e, a = 1] = c`, but `n = m`
    /// is read into a temporary).
    fn simple_element(&self, e: NodeId) -> bool {
        match self.n(e) {
            Node::Ident { .. } | Node::Hole {} | Node::Rest { .. } | Node::Spread { .. } => true,
            Node::AssignPattern { left, right }
            | Node::Assign {
                op: ts_r_ast::AssignOp::Assign,
                left,
                right,
            } => {
                matches!(self.n(left), Node::Ident { .. })
                    && matches!(
                        self.n(right),
                        Node::StringLit { .. }
                            | Node::NumberLit { .. }
                            | Node::BigIntLit { .. }
                            | Node::BoolLit { .. }
                            | Node::NullLit {}
                    )
            }
            _ => false,
        }
    }

    /// How many times flattening the object pattern with properties `items` reads its value:
    /// once per run of plain properties, nested rest, computed key and the rest itself.
    fn value_uses(&self, items: &[NodeId]) -> usize {
        let mut uses = 0;
        let mut in_run = false;
        for &p in items {
            let plain = match self.n(p) {
                Node::BindingProperty { value: Some(v), .. } | Node::Property { value: v, .. } => {
                    !self.has_object_rest(v)
                }
                Node::Rest { .. } | Node::Spread { .. } => false,
                _ => true,
            };
            if plain {
                if !in_run {
                    uses += 1;
                    in_run = true;
                }
            } else {
                uses += 1;
                in_run = false;
            }
        }
        uses
    }

    /// `_b = value.key` and the flattened nested pattern.
    fn flatten_property(
        &mut self,
        key: NodeId,
        pat: NodeId,
        value: NodeId,
        local: bool,
        out: &mut Vec<Pair>,
    ) -> bool {
        let at = self.at(pat);
        let obj = self.copy(value);
        let read = match self.n(key) {
            Node::Ident { .. } => {
                let k = self.copy(key);
                self.mk(
                    Node::Member {
                        obj,
                        optional: false,
                        prop: k,
                    },
                    at,
                )
            }
            _ => {
                let k = self.copy(key);
                self.index(obj, k, at)
            }
        };
        let t = if local {
            self.local_temp()
        } else {
            self.temp(Group::Es2018)
        };
        let target = self.ident_atom(t, at);
        out.push((target, read));
        let r = self.ident_atom(t, at);
        self.flatten(pat, r, local, out)
    }

    fn flush_chunk(
        &mut self,
        chunk: &mut Vec<NodeId>,
        decl: bool,
        value: NodeId,
        out: &mut Vec<Pair>,
        at: u32,
    ) {
        if chunk.is_empty() {
            return;
        }
        let props = self.new_list(chunk);
        chunk.clear();
        let p = if decl {
            self.mk(Node::ObjectPattern { props }, at)
        } else {
            self.mk(Node::ObjectLit { props }, at)
        };
        let v = self.copy(value);
        out.push((p, v));
    }

    // ---- the places a pattern appears ----

    /// Rewrites the declarators of `decl` (a `VarDecl`) whose patterns hold object rests.
    pub fn rest_var_decl(&mut self, decl: NodeId) -> bool {
        let Node::VarDecl {
            modifiers,
            kind,
            decls,
        } = self.n(decl)
        else {
            return false;
        };
        let items = self.tx.items(decls);
        let mut out_decls = Vec::with_capacity(items.len());
        let mut changed = false;
        for d in items {
            let Node::VarDeclarator {
                name,
                init: Some(init),
                ..
            } = self.n(d)
            else {
                out_decls.push(d);
                continue;
            };
            if !self.has_object_rest(name) {
                out_decls.push(d);
                continue;
            }
            let mut pairs = Vec::new();
            if !self.flatten(name, init, true, &mut pairs) {
                return false;
            }
            changed = true;
            for (target, value) in pairs {
                let at = self.at(target);
                out_decls.push(self.mk(
                    Node::VarDeclarator {
                        name: target,
                        definite: false,
                        type_ann: None,
                        init: Some(value),
                    },
                    at,
                ));
            }
        }
        if changed {
            let decls = self.new_list(&out_decls);
            self.tx.set(
                decl,
                Node::VarDecl {
                    modifiers,
                    kind,
                    decls,
                },
            );
        }
        changed
    }

    /// `kind P = value;` lowered.
    pub fn rest_decl_stmt(&mut self, kind: VarKind, pat: NodeId, value: NodeId, at: u32) -> NodeId {
        let v = self.var(kind, Modifiers::NONE, pat, Some(value), at);
        self.rest_var_decl(v);
        v
    }

    /// Function parameters whose patterns hold object rests become temporaries, declared
    /// from in the body. Returns the statements to put first in the body.
    pub fn rest_params(&mut self, params: NodeList) -> Vec<NodeId> {
        let mut stmts = Vec::new();
        for p in self.tx.items(params) {
            let Node::Param {
                decorators,
                modifiers,
                rest,
                pattern,
                optional,
                type_ann,
                init,
            } = self.n(p)
            else {
                continue;
            };
            if !self.has_object_rest(pattern) {
                continue;
            }
            let at = self.at(p);
            let t = self.local_temp();
            let name = self.ident_atom(t, at);
            let r = self.ident_atom(t, at);
            stmts.push(self.rest_decl_stmt(VarKind::Var, pattern, r, at));
            self.tx.set(
                p,
                Node::Param {
                    decorators,
                    modifiers,
                    rest,
                    pattern: name,
                    optional,
                    type_ann,
                    init,
                },
            );
        }
        stmts
    }

    /// Parameter initializers and patterns as body statements: `x = 1` →
    /// `if (x === void 0) { x = 1; }`, `{ d } = a` → `_a` and `var { d } = _a === void 0 ? a : _a;`.
    pub fn move_param_inits(&mut self, params: NodeList) -> Vec<NodeId> {
        let mut stmts = Vec::new();
        for p in self.tx.items(params) {
            let Node::Param {
                decorators,
                modifiers,
                rest: false,
                pattern,
                optional,
                type_ann,
                init,
            } = self.n(p)
            else {
                continue;
            };
            let at = self.at(p);
            let new_pattern = match (self.n(pattern), init) {
                (Node::Ident { name }, Some(i)) => {
                    // if (x === void 0) { x = init; }
                    let x = self.ident_atom(name, at);
                    let v = self.void0(at);
                    let test = self.binary(x, ts_r_ast::BinaryOp::StrictEq, v, at);
                    let x = self.ident_atom(name, at);
                    let a = self.assign(x, i, at);
                    let s = self.expr_stmt(a, at);
                    let block = self.block(&[s], at);
                    self.tx.mark_single_line(block);
                    stmts.push(self.mk(
                        Node::If {
                            test,
                            cons: block,
                            alt: None,
                        },
                        at,
                    ));
                    pattern
                }
                (Node::Ident { .. }, None) => continue,
                _ => {
                    let t = self.local_temp();
                    let value = match init {
                        Some(i) => {
                            let r = self.ident_atom(t, at);
                            let v = self.void0(at);
                            let test = self.binary(r, ts_r_ast::BinaryOp::StrictEq, v, at);
                            let r = self.ident_atom(t, at);
                            self.mk(
                                Node::Conditional {
                                    test,
                                    cons: i,
                                    alt: r,
                                },
                                at,
                            )
                        }
                        None => self.ident_atom(t, at),
                    };
                    stmts.push(self.var(VarKind::Var, Modifiers::NONE, pattern, Some(value), at));
                    self.ident_atom(t, at)
                }
            };
            self.tx.set(
                p,
                Node::Param {
                    decorators,
                    modifiers,
                    rest: false,
                    pattern: new_pattern,
                    optional,
                    type_ann,
                    init: None,
                },
            );
        }
        stmts
    }

    /// Puts `stmts` first in the body of function `id` (after its directives); an
    /// expression-bodied arrow gets a block, on one line when `one_line_arrow`.
    pub fn prepend_body(&mut self, id: NodeId, stmts: Vec<NodeId>, one_line_arrow: bool) {
        let (body, arrow_expr) = match self.n(id) {
            Node::FunctionDecl { body: Some(b), .. } | Node::FunctionExpr { body: Some(b), .. } => {
                (b, false)
            }
            Node::Arrow { body, flags, .. } => {
                (body, flags.contains(ts_r_ast::FnFlags::EXPRESSION_BODY))
            }
            _ => return,
        };
        if arrow_expr {
            let at = self.at(body);
            let moved = crate::builder::clone_node(self.tx.ast, body);
            let r = self.ret(Some(moved), at);
            let mut all = stmts;
            all.push(r);
            let block = self.block(&all, at);
            if one_line_arrow {
                self.tx.mark_single_line(block);
            }
            if let Node::Arrow {
                mut flags,
                type_params,
                params,
                return_type,
                ..
            } = self.n(id)
            {
                flags.remove(ts_r_ast::FnFlags::EXPRESSION_BODY);
                self.tx.set(
                    id,
                    Node::Arrow {
                        flags,
                        type_params,
                        params,
                        return_type,
                        body: block,
                    },
                );
            }
            return;
        }
        let Node::Block { stmts: list } = self.n(body) else {
            return;
        };
        let one_line = self.one_line_body(body);
        let items = self.tx.items(list);
        let pro = items
            .iter()
            .take_while(|&&s| matches!(self.n(s), Node::Directive { .. }))
            .count();
        let mut all = items[..pro].to_vec();
        all.extend(stmts);
        all.extend_from_slice(&items[pro..]);
        let list = self.new_list(&all);
        self.tx.set(body, Node::Block { stmts: list });
        if one_line {
            self.tx.mark_single_line(body);
        }
    }

    /// `catch ({ a, ...r })` → `catch (_a) { var { a } = _a, r = __rest(_a, ["a"]); ... }`.
    pub fn rest_catch(&mut self, id: NodeId) {
        let Node::CatchClause {
            param: Some(param),
            type_ann,
            body,
        } = self.n(id)
        else {
            return;
        };
        self.visit(param);
        self.visit(body);
        let at = self.at(param);
        let t = self.local_temp();
        let r = self.ident_atom(t, at);
        let decl = self.rest_decl_stmt(VarKind::Var, param, r, at);
        let name = self.ident_atom(t, at);
        let Node::Block { stmts } = self.n(body) else {
            return;
        };
        let mut all = vec![decl];
        all.extend(self.tx.items(stmts));
        let stmts = self.new_list(&all);
        self.tx.set(body, Node::Block { stmts });
        self.tx.set(
            id,
            Node::CatchClause {
                param: Some(name),
                type_ann,
                body,
            },
        );
    }

    /// The binding statement for a `for`-`of`/`in` whose left side holds an object rest:
    /// the left side becomes `let _a` (`var _a`) and the statement binds the pattern from it.
    pub fn rest_for_left(&mut self, left: NodeId) -> Option<NodeId> {
        let at = self.at(left);
        match self.n(left) {
            Node::VarDecl { kind, decls, .. } => {
                let &[d] = &self.tx.items(decls)[..] else {
                    return None;
                };
                let Node::VarDeclarator { name, .. } = self.n(d) else {
                    return None;
                };
                if !self.has_object_rest(name) {
                    return None;
                }
                let t = self.local_temp();
                let r = self.ident_atom(t, at);
                let stmt = self.rest_decl_stmt(kind, name, r, at);
                let new_kind = if kind == VarKind::Var {
                    VarKind::Var
                } else {
                    VarKind::Let
                };
                let n = self.ident_atom(t, at);
                let v = self.var(new_kind, Modifiers::NONE, n, None, at);
                self.put(left, v);
                Some(stmt)
            }
            Node::ObjectLit { .. } | Node::ArrayLit { .. } if self.has_object_rest(left) => {
                let t = self.local_temp();
                let r = self.ident_atom(t, at);
                let mut pairs = Vec::new();
                if !self.flatten(left, r, false, &mut pairs) {
                    return None;
                }
                let e = self.pairs_expr(pairs, None, at);
                let p = self.paren(e, at);
                let stmt = self.expr_stmt(p, at);
                let n = self.ident_atom(t, at);
                let v = self.var(VarKind::Let, Modifiers::NONE, n, None, at);
                self.put(left, v);
                Some(stmt)
            }
            _ => None,
        }
    }

    /// `for (const { a, ...r } of xs) body` → `for (let _a of xs) { const { a } = _a, ...; body }`.
    pub fn rest_for(&mut self, id: NodeId) {
        let (left, right, body) = match self.n(id) {
            Node::ForOf {
                left, right, body, ..
            }
            | Node::ForIn { left, right, body } => (left, right, body),
            _ => return,
        };
        self.visit(right);
        match self.n(left) {
            Node::ObjectLit { .. } | Node::ArrayLit { .. } => self.visit_target(left),
            _ => self.visit(left),
        }
        self.loops += 1;
        self.visit(body);
        self.loops -= 1;
        let Some(stmt) = self.rest_for_left(left) else {
            return;
        };
        self.prepend_stmt(body, stmt);
    }

    /// Puts `stmt` first in the loop body `body` (a block, or made one).
    pub fn prepend_stmt(&mut self, body: NodeId, stmt: NodeId) {
        let at = self.at(body);
        let mut all = vec![stmt];
        match self.n(body) {
            Node::Block { stmts } => all.extend(self.tx.items(stmts)),
            _ => {
                let moved = crate::builder::clone_node(self.tx.ast, body);
                all.push(moved);
            }
        }
        let stmts = self.new_list(&all);
        let _ = at;
        self.tx.set(body, Node::Block { stmts });
    }

    pub fn left_has_rest(&self, left: NodeId) -> bool {
        match self.n(left) {
            Node::VarDecl { decls, .. } => self.decls_have_rest(decls),
            Node::ObjectLit { .. } | Node::ArrayLit { .. } => self.has_object_rest(left),
            _ => false,
        }
    }

    pub fn decls_have_rest(&self, decls: NodeList) -> bool {
        self.tx.ast.list(decls).iter().any(|&d| {
            matches!(self.n(d), Node::VarDeclarator { name, .. } if self.has_object_rest(name))
        })
    }

    /// `({ a, ...r } = o)` as a statement's expression (parentheses skipped).
    pub fn is_rest_assign(&self, expr: NodeId) -> bool {
        let e = self.skip_parens(expr);
        matches!(
            self.n(e),
            Node::Assign { op: ts_r_ast::AssignOp::Assign, left, .. }
                if matches!(self.n(left), Node::ObjectLit { .. } | Node::ArrayLit { .. })
                    && self.has_object_rest(left)
        )
    }

    fn pairs_expr(&mut self, pairs: Vec<Pair>, tail: Option<NodeId>, at: u32) -> NodeId {
        let mut exprs: Vec<NodeId> = pairs
            .into_iter()
            .map(|(t, v)| self.assign(t, v, at))
            .collect();
        exprs.extend(tail);
        if exprs.len() == 1 {
            exprs[0]
        } else {
            self.seq(&exprs, at)
        }
    }

    /// `({ a, ...r } = o)` → `({ a } = o, r = __rest(o, ["a"]))` (with `, o` when `used`).
    pub fn rest_assign(&mut self, id: NodeId, used: bool) {
        let Node::Assign { left, right, .. } = self.n(id) else {
            return;
        };
        self.visit_target(left);
        self.visit(right);
        let at = self.at(id);
        let mut pairs = Vec::new();
        let value = if used {
            self.reusable(right, false, &mut pairs)
        } else {
            right
        };
        if !self.flatten(left, value, false, &mut pairs) {
            return;
        }
        let tail = used.then(|| self.copy(value));
        let e = self.pairs_expr(pairs, tail, at);
        self.put(id, e);
    }
}
