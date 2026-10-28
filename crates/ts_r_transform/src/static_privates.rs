//! Static private members of a class-decorated class at ES2022 and later (observed, tsc
//! 7.0.2; the TC39 decorator transform marks such a class and the class-fields transform
//! then lowers its static private elements even though the target has private names:
//! `estransforms/esdecorator.go`, `hasStaticPrivateClassElements`, and
//! `estransforms/classfields.go`).
//!
//! Instance private members stay native. For the static ones of the class `D`:
//!
//! - a field `static #f = v;` becomes `static { _D_f = { value: v }; }` where it stood
//!   (`{ value: void 0 }` without an initializer); an `accessor`'s storage the same;
//! - methods and accessors leave the class: one `static { _D_m = function _D_m() {},
//!   _D_x_get = function _D_x_get() {}, _D_x_set = function _D_x_set(value) {}; }` after
//!   the `_classThis` and `__setFunctionName` blocks, in member order;
//! - their names are declared by one `var` first in the decorator IIFE, in member order;
//!   an undecorated `accessor #x`'s storage comes after its `_get`/`_set` (the class-fields
//!   transform names them), a decorated one's before (the decorator transform made it a
//!   field first);
//! - uses become helper calls with the class alias as the brand:
//!   `__classPrivateFieldGet(o, _classThis, "f", _D_f)` (`"m"` for a method, `"a"` and
//!   the getter for an accessor), `__classPrivateFieldSet(o, _classThis, v, "f", _D_f)`,
//!   `__classPrivateFieldIn(_classThis, o)`; a call `o.#m(a)` is `Get(..).call(o, a)`.
//!
//! Not done: `++`/`--`, logical assignments and destructuring targets on these names,
//! compound assignments whose receiver is not `this` (tsc caches even an identifier
//! receiver in a function-scoped temporary: `_a = C`), and calls whose receiver is not
//! `this` or an identifier stay as written.

use rustc_hash::FxHashMap;
use ts_r_ast::{AssignOp, Atom, BinaryOp, MethodKind, Modifiers, Node, NodeId, NodeKind};

use crate::builder::{Build, deep_clone, replace};
use crate::helpers::Helper;
use crate::transform::Tx;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Field,
    Method,
    Accessor,
}

#[derive(Clone, Copy)]
struct Member {
    kind: Kind,
    /// A field's storage or a method's function.
    storage: Option<Atom>,
    get: Option<Atom>,
    set: Option<Atom>,
}

/// Lowers the static private members of `members` (the final class body; the first `lead`
/// items are the `_classThis` / `__setFunctionName` blocks). Returns the names to declare.
pub(crate) fn lower(
    tx: &mut Tx,
    members: &mut Vec<NodeId>,
    lead: usize,
    class: &str,
    brand: Atom,
    decorated: &[Atom],
) -> Vec<Atom> {
    let mut map: FxHashMap<Atom, Member> = FxHashMap::default();
    let mut vars: Vec<Atom> = Vec::new();
    // Undecorated `accessor` storages wait for their accessor's functions.
    let mut pending: Vec<(String, Atom)> = Vec::new();
    for &m in members.iter() {
        match tx.n(m) {
            Node::PropertyDef { modifiers, key, .. } if modifiers.contains(Modifiers::STATIC) => {
                let Node::PrivateName { name } = tx.n(key) else {
                    continue;
                };
                let text = tx.ast.atom(name).to_owned();
                let storage = tx.fresh(&format!("_{class}_{text}"), false);
                map.insert(
                    name,
                    Member {
                        kind: Kind::Field,
                        storage: Some(storage),
                        get: None,
                        set: None,
                    },
                );
                match text.strip_suffix("_accessor_storage") {
                    Some(base)
                        if !decorated.iter().any(|&d| tx.ast.atom(d) == base)
                            && has_private_accessor(tx, members, base) =>
                    {
                        pending.push((base.to_owned(), storage));
                    }
                    _ => vars.push(storage),
                }
            }
            Node::MethodDef {
                modifiers,
                key,
                kind,
                ..
            } if modifiers.contains(Modifiers::STATIC) => {
                let Node::PrivateName { name } = tx.n(key) else {
                    continue;
                };
                if map.contains_key(&name) {
                    continue;
                }
                let text = tx.ast.atom(name).to_owned();
                if kind == MethodKind::Method {
                    let f = tx.fresh(&format!("_{class}_{text}"), false);
                    vars.push(f);
                    map.insert(
                        name,
                        Member {
                            kind: Kind::Method,
                            storage: Some(f),
                            get: None,
                            set: None,
                        },
                    );
                    continue;
                }
                let (has_get, has_set) = accessor_kinds(tx, members, name);
                let get = has_get.then(|| tx.fresh(&format!("_{class}_{text}_get"), false));
                let set = has_set.then(|| tx.fresh(&format!("_{class}_{text}_set"), false));
                vars.extend(get.into_iter().chain(set));
                map.insert(
                    name,
                    Member {
                        kind: Kind::Accessor,
                        storage: None,
                        get,
                        set,
                    },
                );
                if let Some(i) = pending.iter().position(|(b, _)| *b == text) {
                    vars.push(pending.remove(i).1);
                }
            }
            _ => {}
        }
    }
    vars.extend(pending.into_iter().map(|(_, s)| s));
    if map.is_empty() {
        return vars;
    }

    // The members: fields become static blocks, functions move into one block.
    let mut out = Vec::with_capacity(members.len() + 1);
    let mut functions: Vec<NodeId> = Vec::new();
    for &m in members.iter() {
        let at = tx.at(m);
        match tx.n(m) {
            Node::PropertyDef { key, value, .. } if is_ours(tx, key, &map) => {
                let Some(storage) = private_of(tx, key).and_then(|n| map[&n].storage) else {
                    out.push(m);
                    continue;
                };
                let v = match value {
                    Some(v) => v,
                    None => tx.void0(at),
                };
                let p = tx.prop("value", v, at);
                let obj = tx.object(&[p], at);
                let t = tx.ident_atom(storage, at);
                let a = tx.assign(t, obj, at);
                let s = tx.expr_stmt(a, at);
                let body = tx.new_list(&[s]);
                let b = tx.mk(Node::StaticBlock { body }, at);
                tx.ast.set_span(b, tx.span(m));
                out.push(b);
            }
            Node::MethodDef {
                key, kind, value, ..
            } if is_ours(tx, key, &map) => {
                let Some(name) = private_of(tx, key) else {
                    out.push(m);
                    continue;
                };
                let entry = map[&name];
                let f = match (entry.kind, kind) {
                    (Kind::Method, _) => entry.storage,
                    (_, MethodKind::Get) => entry.get,
                    (_, MethodKind::Set) => entry.set,
                    _ => None,
                };
                let Some(f) = f else {
                    out.push(m);
                    continue;
                };
                if let Node::FunctionExpr {
                    modifiers,
                    flags,
                    type_params,
                    params,
                    return_type,
                    body,
                    ..
                } = tx.n(value)
                {
                    let n = tx.ident_atom(f, at);
                    tx.set(
                        value,
                        Node::FunctionExpr {
                            modifiers,
                            flags,
                            name: Some(n),
                            type_params,
                            params,
                            return_type,
                            body,
                        },
                    );
                }
                let t = tx.ident_atom(f, at);
                functions.push(tx.assign(t, value, at));
            }
            _ => out.push(m),
        }
    }
    if !functions.is_empty() {
        let at = tx.at(functions[0]);
        let e = if functions.len() == 1 {
            functions[0]
        } else {
            tx.seq(&functions, at)
        };
        let s = tx.expr_stmt(e, at);
        let body = tx.new_list(&[s]);
        let b = tx.mk(Node::StaticBlock { body }, at);
        out.insert(lead.min(out.len()), b);
    }
    for &m in &out {
        rewrite(tx, m, &map, brand);
    }
    *members = out;
    vars
}

fn private_of(tx: &Tx, key: NodeId) -> Option<Atom> {
    match tx.n(key) {
        Node::PrivateName { name } => Some(name),
        _ => None,
    }
}

fn is_ours(tx: &Tx, key: NodeId, map: &FxHashMap<Atom, Member>) -> bool {
    private_of(tx, key).is_some_and(|n| map.contains_key(&n))
}

fn has_private_accessor(tx: &Tx, members: &[NodeId], base: &str) -> bool {
    members.iter().any(|&m| {
        matches!(tx.n(m), Node::MethodDef { kind: MethodKind::Get | MethodKind::Set, key, modifiers, .. }
            if modifiers.contains(Modifiers::STATIC)
                && private_of(tx, key).is_some_and(|n| tx.ast.atom(n) == base))
    })
}

fn accessor_kinds(tx: &Tx, members: &[NodeId], name: Atom) -> (bool, bool) {
    let (mut get, mut set) = (false, false);
    for &m in members {
        if let Node::MethodDef {
            kind,
            key,
            modifiers,
            ..
        } = tx.n(m)
            && modifiers.contains(Modifiers::STATIC)
            && private_of(tx, key) == Some(name)
        {
            match kind {
                MethodKind::Get => get = true,
                MethodKind::Set => set = true,
                _ => {}
            }
        }
    }
    (get, set)
}

/// The private member a `o.#x` names, if it is one of ours.
fn member_of(tx: &Tx, e: NodeId, map: &FxHashMap<Atom, Member>) -> Option<(NodeId, Member)> {
    match tx.n(e) {
        Node::Member {
            obj,
            optional: false,
            prop,
        } => {
            let n = private_of(tx, prop)?;
            map.get(&n).map(|&m| (obj, m))
        }
        _ => None,
    }
}

fn is_simple(tx: &Tx, e: NodeId) -> bool {
    matches!(tx.kind(e), NodeKind::This | NodeKind::Ident)
}

fn get(tx: &mut Tx, recv: NodeId, m: Member, brand: Atom, at: u32) -> NodeId {
    let f = tx.helper_ref(Helper::ClassPrivateFieldGet, at);
    let b = tx.ident_atom(brand, at);
    let (kind, func) = match m.kind {
        Kind::Field => ("f", m.storage),
        Kind::Method => ("m", m.storage),
        Kind::Accessor => ("a", m.get),
    };
    let k = tx.string(kind, at);
    let mut args = vec![recv, b, k];
    if let Some(func) = func {
        args.push(tx.ident_atom(func, at));
    }
    tx.call(f, &args, at)
}

fn set(tx: &mut Tx, recv: NodeId, m: Member, value: NodeId, brand: Atom, at: u32) -> NodeId {
    let f = tx.helper_ref(Helper::ClassPrivateFieldSet, at);
    let b = tx.ident_atom(brand, at);
    let (kind, func) = match m.kind {
        Kind::Field => ("f", m.storage),
        Kind::Method => ("m", m.storage),
        Kind::Accessor => ("a", m.set),
    };
    let k = tx.string(kind, at);
    let mut args = vec![recv, b, value, k];
    if let Some(func) = func {
        args.push(tx.ident_atom(func, at));
    }
    tx.call(f, &args, at)
}

fn compound(op: AssignOp) -> Option<BinaryOp> {
    Some(match op {
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
        _ => return None,
    })
}

/// True if the class declares one of `map`'s private names (it shadows ours inside).
fn shadows(tx: &Tx, class: NodeId, map: &FxHashMap<Atom, Member>) -> bool {
    let members = match tx.n(class) {
        Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. } => members,
        _ => return false,
    };
    tx.items(members).iter().any(|&m| match tx.n(m) {
        Node::PropertyDef { key, .. } | Node::MethodDef { key, .. } => is_ours(tx, key, map),
        _ => false,
    })
}

/// Rewrites the uses of `map`'s names under `root` in source order, so helpers are asked
/// for in that order (explicit stack: no recursion).
fn rewrite(tx: &mut Tx, root: NodeId, map: &FxHashMap<Atom, Member>, brand: Atom) {
    let mut stack = vec![root];
    let children = |tx: &Tx, n: NodeId, stack: &mut Vec<NodeId>| {
        let from = stack.len();
        tx.ast.for_each_child(n, |c| stack.push(c));
        stack[from..].reverse();
    };
    while let Some(n) = stack.pop() {
        let at = tx.at(n);
        match tx.n(n) {
            Node::ClassDecl { .. } | Node::ClassExpr { .. } if shadows(tx, n, map) => {}
            Node::Binary {
                left,
                op: BinaryOp::In,
                right,
            } if is_ours(tx, left, map) => {
                let f = tx.helper_ref(Helper::ClassPrivateFieldIn, at);
                let b = tx.ident_atom(brand, at);
                let c = tx.call(f, &[b, right], at);
                replace(tx.ast, n, c);
                stack.push(right);
            }
            Node::Assign { left, op, right } if member_of(tx, left, map).is_some() => {
                let Some((obj, m)) = member_of(tx, left, map) else {
                    continue;
                };
                let value = if op == AssignOp::Assign {
                    Some(right)
                } else if let Some(bop) = compound(op)
                    && tx.kind(obj) == NodeKind::This
                {
                    let o2 = deep_clone(tx.ast, obj);
                    let g = get(tx, o2, m, brand, at);
                    Some(tx.binary(g, bop, right, at))
                } else {
                    None
                };
                match value {
                    Some(v) => {
                        let s = set(tx, obj, m, v, brand, at);
                        replace(tx.ast, n, s);
                        stack.push(right);
                        stack.push(obj);
                    }
                    None => children(tx, n, &mut stack),
                }
            }
            Node::Call {
                callee,
                optional: false,
                args,
                ..
            } if member_of(tx, callee, map).is_some_and(|(o, _)| is_simple(tx, o)) => {
                let Some((obj, m)) = member_of(tx, callee, map) else {
                    continue;
                };
                let o2 = deep_clone(tx.ast, obj);
                let g = get(tx, obj, m, brand, at);
                let f = tx.member(g, "call", at);
                let mut all = vec![o2];
                all.extend(tx.items(args));
                let c = tx.call(f, &all, at);
                replace(tx.ast, n, c);
                stack.extend(tx.items(args).into_iter().rev());
            }
            Node::Update { arg, .. } if member_of(tx, arg, map).is_some() => {}
            Node::Member { .. } if member_of(tx, n, map).is_some() => {
                let Some((obj, m)) = member_of(tx, n, map) else {
                    continue;
                };
                let g = get(tx, obj, m, brand, at);
                replace(tx.ast, n, g);
                stack.push(obj);
            }
            _ => children(tx, n, &mut stack),
        }
    }
}
