//! Private names below ES2022 (observed, tsc 7.0.2).
//!
//! A class's private members get storage named after the class and the member (`_C_x`,
//! file-wide unique names, hoisted into the ES2022 `var` in member order, after
//! `_C_instances`):
//!
//! - an instance field: a `WeakMap` (`_C_x = new WeakMap()` after the class,
//!   `_C_x.set(this, v)` in the constructor where the field's initializer runs);
//! - a static field: `_C_s = { value: v }` among the static members;
//! - methods and accessors: functions (`_C_m = function _C_m() {}` after the class), with
//!   one `_C_instances = new WeakSet()` (`_C_instances.add(this)` first in the
//!   constructor) as the brand of the instance ones; static members' brand is the class
//!   alias.
//!
//! Uses become helper calls: `__classPrivateFieldGet(o, _C_x, "f")`,
//! `__classPrivateFieldSet(o, _C_x, v, "f")`, `__classPrivateFieldIn(_C_x, o)`; methods
//! (`"m"`) and accessors (`"a"`) pass the brand and the function. Compound assignments and
//! `++`/`--` capture a receiver other than `this` in a temporary; calls keep `this` and
//! identifiers as the `this` argument and capture anything else.
//!
//! [`Lower::class`] (es2022.rs) builds the storage and pushes a [`PrivEnv`] while the class
//! is visited; the rewrites below look names up innermost class first. Uses the lowering does
//! not cover (destructuring targets, optional chains) stay and are refused (TSR2003).

use rustc_hash::{FxHashMap, FxHashSet};
use ts_r_ast::{AssignOp, Atom, BinaryOp, Node, NodeId};
use ts_r_config::ScriptTarget;

use super::lower::{Group, Lower};
use crate::builder::Build;
use crate::helpers::Helper;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PrivKind {
    Field,
    Method,
    Accessor,
}

/// How one private name is stored.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PrivMember {
    pub kind: PrivKind,
    pub is_static: bool,
    /// A field's storage or a method's function.
    pub storage: Option<Atom>,
    /// An accessor's functions.
    pub get: Option<Atom>,
    pub set: Option<Atom>,
}

/// The private names of one class being lowered.
#[derive(Default, Debug)]
pub(crate) struct PrivEnv {
    pub members: FxHashMap<Atom, PrivMember>,
    /// `_C_instances`, the brand of instance methods and accessors.
    pub instances: Option<Atom>,
    /// The class alias, the brand of static members.
    pub brand: Option<Atom>,
}

/// A private member reference resolved against the classes around it.
#[derive(Clone, Copy)]
struct Resolved {
    m: PrivMember,
    instances: Option<Atom>,
    brand: Option<Atom>,
}

impl Lower<'_, '_> {
    fn resolve_private(&self, name: Atom) -> Option<Resolved> {
        self.privates.iter().rev().find_map(|env| {
            env.members.get(&name).map(|&m| Resolved {
                m,
                instances: env.instances,
                brand: env.brand,
            })
        })
    }

    /// The private member `obj.#x` (`id`) refers to, if lowered.
    fn private_member(&self, id: NodeId) -> Option<(NodeId, Resolved)> {
        let Node::Member {
            obj,
            prop,
            optional: false,
        } = self.n(id)
        else {
            return None;
        };
        let Node::PrivateName { name } = self.n(prop) else {
            return None;
        };
        self.resolve_private(name).map(|r| (obj, r))
    }

    /// True if `id` is a use of a lowered private name this module rewrites in place.
    pub(super) fn is_private_use(&self, id: NodeId) -> bool {
        if self.privates.is_empty() {
            return false;
        }
        match self.n(id) {
            Node::Member { .. } => self.private_member(id).is_some(),
            // A logical assignment is a compound one too (`set(o, s, get(o, s) ?? v)`, which
            // always writes: observed, tsc 7.0.2).
            Node::Assign { left, .. } => self.private_member(self.skip_parens(left)).is_some(),
            Node::Update { arg, .. } => self.private_member(self.skip_parens(arg)).is_some(),
            Node::Call {
                callee,
                optional: false,
                ..
            } => self.private_member(self.skip_parens(callee)).is_some(),
            Node::TaggedTemplate { tag, .. } => {
                self.private_member(self.skip_parens(tag)).is_some()
            }
            Node::Binary {
                left,
                op: BinaryOp::In,
                ..
            } => {
                matches!(self.n(left), Node::PrivateName { name } if self.resolve_private(name).is_some())
            }
            _ => false,
        }
    }

    /// Private names no class declares, or standing where they cannot be lowered (object
    /// literal keys, operands other than `#x in o`), print as nothing below ES2022:
    /// `this.`, `get ()`, `: 1` (observed; all are errors).
    pub(super) fn blank_privates(&mut self) {
        let mut declared: FxHashSet<Atom> = std::mem::take(&mut self.declared);
        let mut always = Vec::new();
        let mut undeclared = Vec::new();
        let mut stack = vec![self.tx.ast.root];
        while let Some(id) = stack.pop() {
            match self.n(id) {
                Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. } => {
                    for m in self.tx.items(members) {
                        if let Node::PropertyDef { key, .. } | Node::MethodDef { key, .. } =
                            self.n(m)
                            && let Node::PrivateName { name } = self.n(key)
                        {
                            declared.insert(name);
                        }
                    }
                }
                Node::ObjectLit { props } => {
                    for p in self.tx.items(props) {
                        if let Node::Property { key, .. } | Node::MethodDef { key, .. } = self.n(p)
                            && matches!(self.n(key), Node::PrivateName { .. })
                        {
                            always.push(key);
                        }
                    }
                }
                Node::Member { prop, .. } => {
                    if let Node::PrivateName { name } = self.n(prop) {
                        undeclared.push((prop, name));
                    }
                }
                Node::Binary { left, op, right } => {
                    if op != BinaryOp::In && matches!(self.n(left), Node::PrivateName { .. }) {
                        always.push(left);
                    }
                    if matches!(self.n(right), Node::PrivateName { .. }) {
                        always.push(right);
                    }
                }
                _ => {}
            }
            stack.extend(self.tx.ast.children(id).iter());
        }
        let empty = self.tx.ast.intern("");
        for id in always {
            self.tx.set(id, Node::Ident { name: empty });
        }
        for (id, name) in undeclared {
            if !declared.contains(&name) {
                self.tx.set(id, Node::Ident { name: empty });
            }
        }
    }

    /// A private member `id` as a destructuring target becomes
    /// `({ set value(_a) { __classPrivateFieldSet(R, state, _a, kind); } }).value`, the
    /// receiver captured in a temporary unless it is an identifier (observed).
    pub(super) fn private_target(&mut self, id: NodeId) {
        let Some((obj, r)) = self.private_member(id) else {
            if let Node::Member { obj, .. } = self.n(id) {
                self.visit(obj);
            }
            return;
        };
        self.visit(obj);
        let at = self.at(id);
        let recv = if matches!(self.n(obj), Node::Ident { .. }) {
            obj
        } else {
            let t = self.temp(Group::Es2022);
            let a = self.ident_atom(t, at);
            let cap = self.assign(a, obj, at);
            if let Some(caps) = self.priv_caps.as_mut() {
                caps.push(cap);
            }
            self.ident_atom(t, at)
        };
        let p = self.setter_param();
        let v = self.ident_atom(p, at);
        let set = self.private_set(recv, r, v, at);
        let stmt = self.expr_stmt(set, at);
        let key = self.ident("value", at);
        let pname = self.ident_atom(p, at);
        let param = self.param(pname, at);
        let setter = self.method(
            ts_r_ast::Modifiers::NONE,
            ts_r_ast::MethodKind::Set,
            key,
            &[param],
            &[stmt],
            at,
        );
        self.tx.mark_single_line_body(setter);
        if let Node::MethodDef { value, .. } = self.n(setter) {
            self.transparent.insert(value);
        }
        let o = self.object(&[setter], at);
        let par = self.paren(o, at);
        let m = self.member(par, "value", at);
        self.put(id, m);
    }

    /// `x++;` on a private member as a statement (the old value is not kept).
    pub(super) fn is_private_update_stmt(&self, expr: NodeId) -> bool {
        !self.privates.is_empty()
            && matches!(self.n(self.skip_parens(expr)), Node::Update { arg, .. }
                if self.private_member(self.skip_parens(arg)).is_some())
    }

    /// Rewrites a use [`Lower::is_private_use`] accepted; `stmt` for an update whose value
    /// is unused.
    pub(super) fn private_use(&mut self, id: NodeId, stmt: bool) {
        let at = self.at(id);
        match self.n(id) {
            Node::Member { .. } => {
                let Some((obj, r)) = self.private_member(id) else {
                    return;
                };
                self.visit(obj);
                let g = self.private_get(obj, r, at);
                self.put(id, g);
            }
            Node::Assign { op, left, right } => {
                let Some((obj, r)) = self.private_member(self.skip_parens(left)) else {
                    return;
                };
                self.visit(obj);
                self.visit(right);
                let value = if op == AssignOp::Assign {
                    let s = self.private_set(obj, r, right, at);
                    self.put(id, s);
                    return;
                } else {
                    right
                };
                let (r1, r2) = self.private_receiver(obj, at);
                let read = self.private_get(r2, r, at);
                // The operator is lowered here too when the target lacks it (the walk does
                // not revisit what it builds).
                let v = match compound_op(op) {
                    BinaryOp::Coalesce if self.below(ScriptTarget::ES2020) => {
                        self.build_coalesce(id, read, value, false)
                    }
                    BinaryOp::Exp if self.below(ScriptTarget::ES2016) => {
                        self.math_pow(read, value, at)
                    }
                    bop => self.binary(read, bop, value, at),
                };
                let s = self.private_set(r1, r, v, at);
                self.put(id, s);
            }
            Node::Update { op, prefix, arg } => {
                let Some((obj, r)) = self.private_member(self.skip_parens(arg)) else {
                    return;
                };
                self.visit(obj);
                let (r1, r2) = self.private_receiver(obj, at);
                let read = self.private_get(r2, r, at);
                if prefix || stmt {
                    // `(_a = get, _a++, _a)` / `(_a = get, ++_a)`
                    let t = self.temp(Group::Es2022);
                    let a = self.ident_atom(t, at);
                    let first = self.assign(a, read, at);
                    let b = self.ident_atom(t, at);
                    let upd = self.mk(Node::Update { op, prefix, arg: b }, at);
                    let mut items = vec![first, upd];
                    if !prefix {
                        items.push(self.ident_atom(t, at));
                    }
                    let seq = self.seq(&items, at);
                    let v = self.paren(seq, at);
                    let s = self.private_set(r1, r, v, at);
                    self.put(id, s);
                    return;
                }
                // `(set(o, s, (_b = get, _a = _b++, _b), "f"), _a)`
                let result = self.temp(Group::Es2022);
                let t = self.temp(Group::Es2022);
                let a = self.ident_atom(t, at);
                let first = self.assign(a, read, at);
                let b = self.ident_atom(t, at);
                let upd = self.mk(
                    Node::Update {
                        op,
                        prefix: false,
                        arg: b,
                    },
                    at,
                );
                let res = self.ident_atom(result, at);
                let second = self.assign(res, upd, at);
                let c = self.ident_atom(t, at);
                let seq = self.seq(&[first, second, c], at);
                let v = self.paren(seq, at);
                let s = self.private_set(r1, r, v, at);
                let res2 = self.ident_atom(result, at);
                let outer = self.seq(&[s, res2], at);
                let p = self.paren(outer, at);
                self.put(id, p);
            }
            Node::Call { callee, args, .. } => {
                let Some((obj, r)) = self.private_member(self.skip_parens(callee)) else {
                    return;
                };
                self.visit(obj);
                for a in self.tx.items(args) {
                    self.visit(a);
                }
                let (recv, this_arg) = self.call_receiver(obj, at);
                let f = self.private_get(recv, r, at);
                let call = self.member(f, "call", at);
                let mut list = vec![this_arg];
                list.extend(self.tx.items(args));
                let c = self.call(call, &list, at);
                self.put(id, c);
            }
            Node::TaggedTemplate {
                tag,
                type_args,
                quasi,
            } => {
                let Some((obj, r)) = self.private_member(self.skip_parens(tag)) else {
                    return;
                };
                self.visit(obj);
                self.visit(quasi);
                let (recv, this_arg) = self.call_receiver(obj, at);
                let f = self.private_get(recv, r, at);
                let bind = self.member(f, "bind", at);
                let bound = self.call(bind, &[this_arg], at);
                self.tx.set(
                    id,
                    Node::TaggedTemplate {
                        tag: bound,
                        type_args,
                        quasi,
                    },
                );
            }
            Node::Binary { left, right, .. } => {
                let Node::PrivateName { name } = self.n(left) else {
                    return;
                };
                let Some(r) = self.resolve_private(name) else {
                    return;
                };
                self.visit(right);
                let state = self.private_state(r, at);
                let f = self.tx.helper_ref(Helper::ClassPrivateFieldIn, at);
                let c = self.call(f, &[state, right], at);
                self.put(id, c);
            }
            _ => {}
        }
    }

    /// The receiver's first and second use: `this` twice, else `_a = o` then `_a`.
    fn private_receiver(&mut self, obj: NodeId, at: u32) -> (NodeId, NodeId) {
        if matches!(self.n(obj), Node::This {}) {
            let again = self.this(at);
            return (obj, again);
        }
        let t = self.temp(Group::Es2022);
        let a = self.ident_atom(t, at);
        let first = self.assign(a, obj, at);
        (first, self.ident_atom(t, at))
    }

    /// A call's receiver and `this` argument: `this` and identifiers as they are, anything
    /// else `(_a = o)` then `_a`.
    fn call_receiver(&mut self, obj: NodeId, at: u32) -> (NodeId, NodeId) {
        match self.n(obj) {
            Node::This {} => {
                let again = self.this(at);
                (obj, again)
            }
            Node::Ident { name } => {
                let again = self.ident_atom(name, at);
                (obj, again)
            }
            _ => {
                let t = self.temp(Group::Es2022);
                let a = self.ident_atom(t, at);
                let first = self.assign(a, obj, at);
                let p = self.paren(first, at);
                (p, self.ident_atom(t, at))
            }
        }
    }

    /// The brand or storage a helper checks the receiver against.
    fn private_state(&mut self, r: Resolved, at: u32) -> NodeId {
        let a = if r.m.is_static {
            r.brand
        } else if r.m.kind == PrivKind::Field {
            r.m.storage
        } else {
            r.instances
        };
        match a {
            Some(a) => self.ident_atom(a, at),
            None => self.void0(at),
        }
    }

    fn kind_str(&mut self, r: Resolved, at: u32) -> NodeId {
        let k = match r.m.kind {
            PrivKind::Field => "f",
            PrivKind::Method => "m",
            PrivKind::Accessor => "a",
        };
        self.string(k, at)
    }

    /// `__classPrivateFieldGet(recv, state, kind[, f])`.
    fn private_get(&mut self, recv: NodeId, r: Resolved, at: u32) -> NodeId {
        let f = self.tx.helper_ref(Helper::ClassPrivateFieldGet, at);
        let state = self.private_state(r, at);
        let kind = self.kind_str(r, at);
        let mut args = vec![recv, state, kind];
        let extra = match r.m.kind {
            PrivKind::Field if r.m.is_static => r.m.storage,
            PrivKind::Field => None,
            PrivKind::Method => r.m.storage,
            PrivKind::Accessor => r.m.get,
        };
        if let Some(e) = extra {
            args.push(self.ident_atom(e, at));
        }
        self.call(f, &args, at)
    }

    /// `__classPrivateFieldSet(recv, state, value, kind[, f])`.
    fn private_set(&mut self, recv: NodeId, r: Resolved, value: NodeId, at: u32) -> NodeId {
        let f = self.tx.helper_ref(Helper::ClassPrivateFieldSet, at);
        let state = self.private_state(r, at);
        let kind = self.kind_str(r, at);
        let mut args = vec![recv, state, value, kind];
        let extra = match r.m.kind {
            PrivKind::Field if r.m.is_static => r.m.storage,
            PrivKind::Field | PrivKind::Method => None,
            PrivKind::Accessor => r.m.set,
        };
        if let Some(e) = extra {
            args.push(self.ident_atom(e, at));
        }
        self.call(f, &args, at)
    }
}

/// The binary operator of a compound assignment.
pub(super) fn compound_op(op: AssignOp) -> BinaryOp {
    match op {
        AssignOp::AddAssign => BinaryOp::Add,
        AssignOp::SubAssign => BinaryOp::Sub,
        AssignOp::MulAssign => BinaryOp::Mul,
        AssignOp::DivAssign => BinaryOp::Div,
        AssignOp::ModAssign => BinaryOp::Mod,
        AssignOp::ExpAssign => BinaryOp::Exp,
        AssignOp::ShlAssign => BinaryOp::Shl,
        AssignOp::ShrAssign => BinaryOp::Shr,
        AssignOp::UShrAssign => BinaryOp::UShr,
        AssignOp::BitOrAssign => BinaryOp::BitOr,
        AssignOp::BitXorAssign => BinaryOp::BitXor,
        AssignOp::BitAndAssign => BinaryOp::BitAnd,
        AssignOp::OrAssign => BinaryOp::LogicalOr,
        AssignOp::AndAssign => BinaryOp::LogicalAnd,
        AssignOp::CoalesceAssign => BinaryOp::Coalesce,
        AssignOp::Assign => BinaryOp::Add,
    }
}
