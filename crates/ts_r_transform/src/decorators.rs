//! Legacy decorators (`experimentalDecorators`) and `emitDecoratorMetadata`.
//!
//! tsc 7 shapes:
//! ```js
//! let C = C_1 = class C { static make() { return new C_1(); } };   // class decorated
//! __decorate([dec, __param(0, p), __metadata("design:type", Function), ...],
//!     C.prototype, "m", null);                                       // instance members first
//! __decorate([dec, __metadata("design:type", Number)], C, "s", void 0); // then statics
//! C = C_1 = __decorate([dec, __metadata("design:paramtypes", [Number])], C);
//! export { C };
//! ```
//! A class with only member decorators stays a declaration.

use ts_r_ast::{
    Atom, KeywordTypeKind, MethodKind, Modifiers, Node, NodeId, NodeKind, NodeList,
    TypeOperatorKind, VarKind,
};

use crate::MetadataType;
use crate::builder::Build;
use crate::helpers::Helper;
use crate::scope::BindingKind;
use crate::transform::Tx;

#[derive(Clone, Copy, PartialEq, Eq)]
enum MemberKind {
    Field,
    Method,
    Get,
    Set,
}

struct Member {
    is_static: bool,
    key: NodeId,
    kind: MemberKind,
    decorators: Vec<NodeId>,
    /// `(index, decorator expression)`
    params: Vec<(usize, NodeId)>,
    /// Field type / accessor type.
    ty: Option<NodeId>,
    param_types: Vec<ParamType>,
    return_type: Option<NodeId>,
    is_async: bool,
    at: u32,
}

#[derive(Clone, Copy)]
struct ParamType {
    ty: Option<NodeId>,
    rest: bool,
}

pub(crate) struct Info {
    class_decorators: Vec<NodeId>,
    ctor_params: Vec<(usize, NodeId)>,
    ctor_types: Option<Vec<ParamType>>,
    members: Vec<Member>,
}

fn decorator_exprs(tx: &Tx, list: NodeList) -> Vec<NodeId> {
    tx.ast
        .list(list)
        .iter()
        .filter_map(|&d| match tx.n(d) {
            Node::Decorator { expr } => Some(expr),
            _ => None,
        })
        .collect()
}

fn param_info(tx: &Tx, value: NodeId) -> (Vec<(usize, NodeId)>, Vec<ParamType>) {
    let mut decs = Vec::new();
    let mut types = Vec::new();
    if let Node::FunctionExpr { params, .. } = tx.n(value) {
        let mut i = 0usize;
        for &p in tx.ast.list(params) {
            if let Node::Param {
                decorators,
                pattern,
                type_ann,
                rest,
                ..
            } = tx.n(p)
            {
                if matches!(tx.n(pattern), Node::Ident { name } if tx.ast.atom(name) == "this") {
                    continue;
                }
                for e in decorator_exprs(tx, decorators) {
                    decs.push((i, e));
                }
                types.push(ParamType { ty: type_ann, rest });
                i += 1;
            }
        }
    }
    (decs, types)
}

/// Reads decorators and the types metadata needs, before the class body is stripped.
pub(crate) fn collect(tx: &mut Tx, id: NodeId) -> Info {
    let p = crate::class::parts(tx, id).expect("class");
    let mut info = Info {
        class_decorators: decorator_exprs(tx, p.decorators),
        ctor_params: Vec::new(),
        ctor_types: None,
        members: Vec::new(),
    };
    for m in tx.items(p.members) {
        match tx.n(m) {
            Node::MethodDef {
                decorators,
                modifiers,
                kind,
                key,
                value,
                ..
            } => {
                let has_body = matches!(tx.n(value), Node::FunctionExpr { body: Some(_), .. });
                if kind == MethodKind::Constructor {
                    if has_body {
                        let (decs, types) = param_info(tx, value);
                        info.ctor_params = decs;
                        info.ctor_types = Some(types);
                    }
                    continue;
                }
                // A private member is never decorated (an error; observed).
                if !has_body || matches!(tx.n(key), Node::PrivateName { .. }) {
                    continue;
                }
                let (params, param_types) = param_info(tx, value);
                let decorators = decorator_exprs(tx, decorators);
                if decorators.is_empty() && params.is_empty() {
                    continue;
                }
                let (return_type, is_async) = match tx.n(value) {
                    Node::FunctionExpr {
                        return_type, flags, ..
                    } => (return_type, flags.contains(ts_r_ast::FnFlags::ASYNC)),
                    _ => (None, false),
                };
                let kind = match kind {
                    MethodKind::Get => MemberKind::Get,
                    MethodKind::Set => MemberKind::Set,
                    _ => MemberKind::Method,
                };
                // Only the first decorated accessor of a pair is decorated (observed).
                let is_static = modifiers.contains(Modifiers::STATIC);
                if matches!(kind, MemberKind::Get | MemberKind::Set)
                    && let Some(text) = accessor_key(tx, key)
                    && info.members.iter().any(|o| {
                        matches!(o.kind, MemberKind::Get | MemberKind::Set)
                            && o.is_static == is_static
                            && accessor_key(tx, o.key).as_deref() == Some(text.as_str())
                    })
                {
                    continue;
                }
                let ty = match kind {
                    MemberKind::Get => return_type,
                    MemberKind::Set => param_types.first().and_then(|p| p.ty),
                    _ => None,
                };
                info.members.push(Member {
                    is_static: modifiers.contains(Modifiers::STATIC),
                    key,
                    kind,
                    decorators,
                    params,
                    ty,
                    param_types,
                    return_type,
                    is_async,
                    at: tx.at(m),
                });
            }
            Node::PropertyDef {
                decorators,
                modifiers,
                key,
                type_ann,
                ..
            } => {
                let decorators = decorator_exprs(tx, decorators);
                // A decorated `declare` field is decorated too (observed).
                if decorators.is_empty() || matches!(tx.n(key), Node::PrivateName { .. }) {
                    continue;
                }
                let accessor = modifiers.contains(Modifiers::ACCESSOR);
                info.members.push(Member {
                    is_static: modifiers.contains(Modifiers::STATIC),
                    key,
                    kind: if accessor {
                        MemberKind::Get
                    } else {
                        MemberKind::Field
                    },
                    decorators,
                    params: Vec::new(),
                    ty: type_ann,
                    param_types: Vec::new(),
                    return_type: None,
                    is_async: false,
                    at: tx.at(m),
                });
            }
            _ => {}
        }
    }
    // An accessor's metadata describes the pair: with a setter, its parameter's type (none
    // reads as `Object`, even when the getter has a return type), else the getter's return
    // type; and the setter's parameters (tsc: tsc/internal/transformers/tstransforms/
    // typeserializer.go, the accessor type node).
    let members = tx.items(p.members);
    for m in info
        .members
        .iter_mut()
        .filter(|m| matches!(m.kind, MemberKind::Get | MemberKind::Set))
    {
        let Some(text) = accessor_key(tx, m.key) else {
            continue;
        };
        let mut found = false;
        let mut get_ret = None;
        let mut setter = None;
        for &c in &members {
            let Node::MethodDef {
                modifiers,
                kind,
                key,
                value,
                ..
            } = tx.n(c)
            else {
                continue;
            };
            if modifiers.contains(Modifiers::STATIC) != m.is_static
                || accessor_key(tx, key).as_deref() != Some(text.as_str())
            {
                continue;
            }
            match kind {
                MethodKind::Get => {
                    found = true;
                    if let Node::FunctionExpr { return_type, .. } = tx.n(value) {
                        get_ret = return_type;
                    }
                }
                MethodKind::Set => {
                    found = true;
                    setter = Some(value);
                }
                _ => {}
            }
        }
        if !found {
            continue;
        }
        let set_types = setter.map(|v| param_info(tx, v).1);
        m.ty = match &set_types {
            Some(t) => t.first().and_then(|t| t.ty),
            None => get_ret,
        };
        if let Some(t) = set_types {
            m.param_types = t;
        }
    }
    // Instance members first, then static members (each in source order).
    info.members.sort_by_key(|m| m.is_static);
    info
}

/// An accessor's name for pairing a getter with its setter (`None` for computed keys).
fn accessor_key(tx: &Tx, key: NodeId) -> Option<String> {
    match tx.n(key) {
        Node::Ident { name } | Node::PrivateName { name } | Node::StringLit { value: name } => {
            Some(tx.ast.atom(name).to_owned())
        }
        _ => None,
    }
}

/// Emits a decorated class declaration (already stripped by `class_body`).
pub(crate) fn emit_decl(tx: &mut Tx, id: NodeId, info: Info, out: &mut Vec<NodeId>) {
    let mut p = crate::class::parts(tx, id).expect("class");
    let at = tx.at(id);
    let class_decorated = !info.class_decorators.is_empty() || !info.ctor_params.is_empty();
    let exported = p.modifiers.contains(Modifiers::EXPORT);
    let is_default = p.modifiers.contains(Modifiers::DEFAULT);
    let source_name = p.name;
    // A name is needed to refer to the class.
    let cjs_top = tx.opts.is_cjs() && tx.ns_stack.is_empty() && tx.list_depth == 0;
    let name_atom = match p.name {
        Some(n) => tx.atom_of(n).unwrap_or(Atom::EMPTY),
        None => {
            // CommonJS named the anonymous default already (`default_1`, its hoisting
            // prepass): the decorated class takes that name rather than a second one.
            let a = match crate::cjs::default_name(tx) {
                Some(a) if cjs_top => a,
                _ => tx.fresh("default", true),
            };
            let n = tx.ident_atom(a, at);
            p.name = Some(n);
            a
        }
    };
    let name = tx.ast.atom(name_atom).to_owned();
    // Self-references inside a decorated class go through an alias (`C_1`), since the
    // decorator may replace the class.
    let mut alias: Option<Atom> = None;
    if class_decorated && let Some(n) = p.name {
        let refs = self_refs(tx, n, p.members);
        if !refs.is_empty() {
            let a = tx.fresh(&name, true);
            if let Some(t) = tx.temps.last_mut() {
                t.push(a);
            }
            for r in refs {
                tx.set(r, Node::Ident { name: a });
            }
            alias = Some(a);
        }
    }
    crate::class::clear_decorators(tx, id);
    p = crate::class::parts(tx, id).expect("class");
    if p.name.is_none() {
        p.name = Some(tx.ident_atom(name_atom, at));
    }

    let mut after: Vec<NodeId> = Vec::new();
    for m in &info.members {
        let s = member_decorate(tx, m, &name);
        after.push(s);
    }
    // Member decorations that use a private name run in a static block at the end of the
    // class, where the name is in scope (observed).
    let uses_private = info.members.iter().any(|m| {
        m.decorators
            .iter()
            .chain(m.params.iter().map(|(_, d)| d))
            .any(|&d| has_private_name(tx, d))
    });
    if uses_private && !after.is_empty() {
        let body = tx.new_list(&after);
        let block = tx.mk(Node::StaticBlock { body }, at);
        let mut members = tx.items(p.members);
        members.push(block);
        p.members = tx.new_list(&members);
        after.clear();
    }

    if class_decorated {
        // `let C = class C { ... };` (an anonymous default class stays anonymous: `let
        // default_1 = class { ... };`, observed).
        let class_name = source_name.and(p.name);
        p.modifiers = Modifiers::NONE;
        p.is_expr = true;
        p.name = class_name;
        crate::class::set_parts(tx, id, p);
        tx.decl_like.push(id);
        let mut init = id;
        if let Some(a) = alias {
            let t = tx.ident_atom(a, at);
            init = tx.assign(t, id, at);
        }
        let local = tx.ident_atom(name_atom, at);
        let decl = tx.var(VarKind::Let, Modifiers::NONE, local, Some(init), at);
        tx.ast.set_span(decl, tx.span(id));
        out.push(decl);
        // CommonJS: `exports.C = C;` right after the class, and the decorated class is
        // exported again (`exports.C = C = __decorate([...], C);`), as tsc does; also for
        // `export { C }` lists. A default export follows the decoration instead.
        let mut cjs_aliases: Vec<String> = Vec::new();
        if tx.opts.is_cjs() && tx.ns_stack.is_empty() && tx.list_depth == 0 {
            if let Some(n) = source_name {
                cjs_aliases = crate::cjs::take_local_exports(tx, n);
            }
            if exported && !is_default {
                cjs_aliases.insert(0, name.clone());
            }
            for a in &cjs_aliases {
                crate::cjs::export_binding(tx, &name, a, at, out);
            }
        }
        out.extend(after);
        // `C = __decorate([...], C);`
        let mut elems = info.class_decorators.clone();
        for &(i, d) in &info.ctor_params {
            elems.push(param_call(tx, i, d, at));
        }
        if tx.opts.emit_decorator_metadata
            && let Some(types) = &info.ctor_types
        {
            let arr = param_types_array(tx, types, at);
            elems.push(metadata(tx, "design:paramtypes", arr, at));
        }
        let decorate = tx.helper_ref(Helper::Decorate, at);
        let arr = tx.array(&elems, at);
        tx.mark_multi_line(arr);
        let target = tx.ident_atom(name_atom, at);
        let call = tx.call(decorate, &[arr, target], at);
        let mut value = call;
        if let Some(a) = alias {
            let t = tx.ident_atom(a, at);
            value = tx.assign(t, call, at);
        }
        let lhs = tx.ident_atom(name_atom, at);
        let mut assign = tx.assign(lhs, value, at);
        if !tx.cjs_export_assignment() {
            for a in cjs_aliases.iter().rev() {
                let e = tx.ident("exports", at);
                let target = tx.member_or_index(e, a, at);
                assign = tx.assign(target, assign, at);
            }
        }
        let s = tx.expr_stmt(assign, at);
        out.push(s);
        if exported && (is_default || cjs_aliases.is_empty()) {
            export_after(tx, name_atom, is_default, at, out);
        }
        return;
    }
    crate::class::set_parts(tx, id, p);
    crate::class::export_decl(tx, id, p.modifiers, p.name, out);
    out.extend(after);
}

/// True if expression `id` mentions a private name (`x.#y`, `#y in x`).
fn has_private_name(tx: &Tx, id: NodeId) -> bool {
    let mut stack = vec![id];
    while let Some(n) = stack.pop() {
        if matches!(tx.n(n), Node::PrivateName { .. }) {
            return true;
        }
        stack.extend(tx.ast.children(n).iter());
    }
    false
}

/// `export { C };` / `export default C;` (or the namespace/CommonJS equivalent).
pub(crate) fn export_after(
    tx: &mut Tx,
    name: Atom,
    is_default: bool,
    at: u32,
    out: &mut Vec<NodeId>,
) {
    let local = tx.ast.atom(name).to_owned();
    if !tx.ns_stack.is_empty() {
        crate::namespace::export_binding(tx, &local, &local, at, out);
        return;
    }
    if tx.opts.is_cjs() {
        let exported = if is_default {
            "default"
        } else {
            local.as_str()
        };
        crate::cjs::export_binding(tx, &local, exported, at, out);
        return;
    }
    let l = tx.ident_atom(name, at);
    if is_default {
        let s = tx.mk(Node::ExportDefaultExpr { expr: l }, at);
        out.push(s);
    } else {
        let spec = tx.mk(
            Node::ExportSpecifier {
                type_only: false,
                local: l,
                exported: None,
            },
            at,
        );
        let specifiers = tx.new_list(&[spec]);
        let s = tx.mk(
            Node::ExportNamed {
                type_only: false,
                specifiers,
                source: None,
                attributes: None,
            },
            at,
        );
        out.push(s);
    }
}

/// Identifiers in the class members that refer to the class itself.
fn self_refs(tx: &Tx, name: NodeId, members: NodeList) -> Vec<NodeId> {
    let Some(&b) = tx.sc.decl_of.get(&name) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut stack: Vec<NodeId> = tx.ast.list(members).to_vec();
    while let Some(n) = stack.pop() {
        if tx.kind(n) == NodeKind::Ident && tx.sc.binding_of_ref(n) == Some(b) {
            out.push(n);
        }
        tx.ast.for_each_child(n, |c| stack.push(c));
    }
    out
}

fn param_call(tx: &mut Tx, index: usize, dec: NodeId, at: u32) -> NodeId {
    let f = tx.helper_ref(Helper::Param, at);
    let i = tx.number(index as f64, at);
    tx.call(f, &[i, dec], at)
}

fn metadata(tx: &mut Tx, key: &str, value: NodeId, at: u32) -> NodeId {
    let f = tx.helper_ref(Helper::Metadata, at);
    let k = tx.string(key, at);
    tx.call(f, &[k, value], at)
}

fn param_types_array(tx: &mut Tx, types: &[ParamType], at: u32) -> NodeId {
    let mut elems = Vec::with_capacity(types.len());
    for t in types {
        let ty = match (t.rest, t.ty) {
            (true, Some(ty)) => match tx.n(ty) {
                Node::ArrayType { elem } => Some(elem),
                Node::TypeRef {
                    type_args: Some(args),
                    name,
                } if tx.name(name) == "Array" => match tx.n(args) {
                    Node::TypeArgs { args } => tx.ast.list(args).first().copied(),
                    _ => None,
                },
                _ => Some(ty),
            },
            (_, ty) => ty,
        };
        let e = serialize(tx, ty, at);
        elems.push(e);
    }
    tx.array(&elems, at)
}

/// `__decorate([...], C.prototype, "key", null | void 0);`
fn member_decorate(tx: &mut Tx, m: &Member, class: &str) -> NodeId {
    let at = m.at;
    let mut elems = m.decorators.clone();
    for &(i, d) in &m.params {
        elems.push(param_call(tx, i, d, at));
    }
    if tx.opts.emit_decorator_metadata {
        match m.kind {
            MemberKind::Field => {
                let t = serialize(tx, m.ty, at);
                elems.push(metadata(tx, "design:type", t, at));
            }
            MemberKind::Method => {
                let f = tx.ident("Function", at);
                elems.push(metadata(tx, "design:type", f, at));
                let arr = param_types_array(tx, &m.param_types, at);
                elems.push(metadata(tx, "design:paramtypes", arr, at));
                let r = if m.return_type.is_none() && m.is_async {
                    tx.ident("Promise", at)
                } else {
                    serialize_return(tx, m.return_type, at)
                };
                elems.push(metadata(tx, "design:returntype", r, at));
            }
            MemberKind::Get | MemberKind::Set => {
                let t = serialize(tx, m.ty, at);
                elems.push(metadata(tx, "design:type", t, at));
                if m.kind == MemberKind::Get && m.param_types.is_empty() {
                    let arr = tx.array(&[], at);
                    elems.push(metadata(tx, "design:paramtypes", arr, at));
                } else {
                    let arr = param_types_array(tx, &m.param_types, at);
                    elems.push(metadata(tx, "design:paramtypes", arr, at));
                }
            }
        }
    }
    let decorate = tx.helper_ref(Helper::Decorate, at);
    let arr = tx.array(&elems, at);
    tx.mark_multi_line(arr);
    let cls = tx.ident(class, at);
    let target = if m.is_static {
        cls
    } else {
        tx.member(cls, "prototype", at)
    };
    let key = key_expr(tx, m.key, at);
    let desc = if m.kind == MemberKind::Field {
        tx.void0(at)
    } else {
        tx.null(at)
    };
    let call = tx.call(decorate, &[arr, target, key, desc], at);
    tx.expr_stmt(call, at)
}

/// The property key as an expression (`"name"`, `0`, or the computed expression).
fn key_expr(tx: &mut Tx, key: NodeId, at: u32) -> NodeId {
    match tx.n(key) {
        Node::Ident { name } | Node::PrivateName { name } => {
            let s = tx.ast.atom(name).to_owned();
            tx.string(&s, at)
        }
        Node::StringLit { value } => {
            let s = tx.ast.atom(value).to_owned();
            tx.string(&s, at)
        }
        Node::NumberLit { .. } | Node::BigIntLit { .. } => crate::builder::clone_node(tx.ast, key),
        Node::ComputedKey { expr } => match tx.n(expr) {
            Node::StringLit { value } => {
                let s = tx.ast.atom(value).to_owned();
                tx.string(&s, at)
            }
            _ => crate::builder::deep_clone(tx.ast, expr),
        },
        _ => crate::builder::clone_node(tx.ast, key),
    }
}

fn serialize_return(tx: &mut Tx, ty: Option<NodeId>, at: u32) -> NodeId {
    match ty {
        None => tx.void0(at),
        Some(_) => serialize(tx, ty, at),
    }
}

/// Global constructors tsc resolves to themselves.
const GLOBAL_CTORS: &[&str] = &[
    "Array",
    "ArrayBuffer",
    "BigInt",
    "BigInt64Array",
    "BigUint64Array",
    "Boolean",
    "DataView",
    "Date",
    "Error",
    "EvalError",
    "Float32Array",
    "Float64Array",
    "Function",
    "Int16Array",
    "Int32Array",
    "Int8Array",
    "Map",
    "Number",
    "Object",
    "Promise",
    "Proxy",
    "RangeError",
    "ReferenceError",
    "RegExp",
    "Set",
    "SharedArrayBuffer",
    "String",
    "Symbol",
    "SyntaxError",
    "TypeError",
    "URIError",
    "Uint16Array",
    "Uint32Array",
    "Uint8Array",
    "Uint8ClampedArray",
    "WeakMap",
    "WeakRef",
    "WeakSet",
    "URL",
    "URLSearchParams",
    "AbortController",
    "AbortSignal",
    "Blob",
    "FormData",
    "Headers",
    "Request",
    "Response",
    "TextDecoder",
    "TextEncoder",
    "Event",
    "EventTarget",
    "HTMLElement",
    "Element",
    "Node",
    "Document",
    "Window",
    "Buffer",
];

/// The runtime value `design:type` metadata uses for a type annotation.
fn serialize(tx: &mut Tx, ty: Option<NodeId>, at: u32) -> NodeId {
    let Some(ty) = ty else {
        return tx.ident("Object", at);
    };
    let k = serial_kind(tx, ty, 0);
    serial_expr(tx, k, at)
}

fn serial_expr(tx: &mut Tx, k: Serial, at: u32) -> NodeId {
    match k {
        // `BigInt` is a global only from ES2020: below, tsc guards it.
        Serial::Name(n) if n == "BigInt" && tx.opts.target < ts_r_config::ScriptTarget::ES2020 => {
            let x = tx.ident("BigInt", at);
            let tof = tx.unary(ts_r_ast::UnaryOp::TypeOf, x, at);
            let fun = tx.string("function", at);
            let test = tx.binary(tof, ts_r_ast::BinaryOp::StrictEq, fun, at);
            let cons = tx.ident("BigInt", at);
            let alt = tx.ident("Object", at);
            tx.mk(Node::Conditional { test, cons, alt }, at)
        }
        Serial::Name(n) => tx.ident(&n, at),
        Serial::Void => tx.void0(at),
        Serial::Entity(n) => entity_expr(tx, n, at),
        Serial::Members(guards, first) => {
            let mut value = None;
            for g in guards {
                let e = serial_expr(tx, Serial::Guarded(g), at);
                value.get_or_insert(e);
            }
            match value {
                Some(e) if first => e,
                _ => tx.ident("Object", at),
            }
        }
        Serial::Guarded(n) => {
            // typeof (_a = <checked name>) === "function" ? _a : Object; the temps of a
            // dotted name's guard come first.
            let checked = checked_entity(tx, n, at);
            let t = tx.temp();
            let tl = tx.ident_atom(t, at);
            let a = tx.assign(tl, checked, at);
            let pa = tx.paren(a, at);
            let tof = tx.unary(ts_r_ast::UnaryOp::TypeOf, pa, at);
            let fun = tx.string("function", at);
            let test = tx.binary(tof, ts_r_ast::BinaryOp::StrictEq, fun, at);
            let cons = tx.ident_atom(t, at);
            let alt = tx.ident("Object", at);
            tx.mk(Node::Conditional { test, cons, alt }, at)
        }
    }
}

/// An entity name (`A`, `A.B.C`) as an expression. Under CommonJS a name bound by an import
/// reads through its module variable (`a_1.A`).
fn entity_expr(tx: &mut Tx, n: NodeId, at: u32) -> NodeId {
    match tx.n(n) {
        Node::QualifiedName { left, right } => {
            let obj = entity_expr(tx, left, at);
            let name = tx.name(right).to_owned();
            tx.member(obj, &name, at)
        }
        _ => {
            let name = tx.name(n).to_owned();
            let id = tx.ident(&name, at);
            if tx.opts.is_cjs()
                && let Some(atom) = tx.ast.atoms.find(&name)
                && let Some(b) = tx.sc.module_binding(atom)
            {
                crate::cjs::rewrite_ref(tx, id, b);
                tx.cjs.detached.remove(&id);
            }
            id
        }
    }
}

/// `typeof A !== "undefined" && A` for a name that may not exist at run time; dotted names
/// check each step: `typeof A !== "undefined" && (_a = A.B) !== void 0 && _a.C`.
fn checked_entity(tx: &mut Tx, n: NodeId, at: u32) -> NodeId {
    let Node::QualifiedName { left, right } = tx.n(n) else {
        let x = entity_expr(tx, n, at);
        let y = entity_expr(tx, n, at);
        return defined_and(tx, x, y, at);
    };
    let right = tx.name(right).to_owned();
    if !matches!(tx.n(left), Node::QualifiedName { .. }) {
        let x = entity_expr(tx, left, at);
        let y = entity_expr(tx, n, at);
        return defined_and(tx, x, y, at);
    }
    let inner = checked_entity(tx, left, at);
    let Node::Binary {
        left: check,
        right: value,
        ..
    } = tx.n(inner)
    else {
        return inner;
    };
    let t = tx.temp();
    let tl = tx.ident_atom(t, at);
    let a = tx.assign(tl, value, at);
    let pa = tx.paren(a, at);
    let v0 = tx.void0(at);
    let ne = tx.binary(pa, ts_r_ast::BinaryOp::StrictNotEq, v0, at);
    let both = tx.binary(check, ts_r_ast::BinaryOp::LogicalAnd, ne, at);
    let obj = tx.ident_atom(t, at);
    let last = tx.member(obj, &right, at);
    tx.binary(both, ts_r_ast::BinaryOp::LogicalAnd, last, at)
}

/// `typeof x !== "undefined" && y`.
fn defined_and(tx: &mut Tx, x: NodeId, y: NodeId, at: u32) -> NodeId {
    let tof = tx.unary(ts_r_ast::UnaryOp::TypeOf, x, at);
    let undef = tx.string("undefined", at);
    let ne = tx.binary(tof, ts_r_ast::BinaryOp::StrictNotEq, undef, at);
    tx.binary(ne, ts_r_ast::BinaryOp::LogicalAnd, y, at)
}

#[derive(PartialEq, Clone)]
enum Serial {
    /// A global constructor or a local class, by name.
    Name(String),
    Void,
    /// The entity name of a type reference, as a value (an imported or namespaced class).
    Entity(NodeId),
    /// An entity name that may not exist at run time: guarded by `typeof`.
    Guarded(NodeId),
    /// A union's members that were guarded, in order: tsc builds each one's guard (and
    /// declares its temporaries) before comparing, so a union that ends as `Object`, or
    /// repeats a name, still takes their temps. `true`: the value is the first one's
    /// guard; `false`: `Object`.
    Members(Vec<NodeId>, bool),
}

fn obj() -> Serial {
    Serial::Name("Object".to_owned())
}

/// Two serializations of union members print the same (`Object` is returned otherwise).
fn same_serial(tx: &Tx, a: &Serial, b: &Serial) -> bool {
    match (a, b) {
        (Serial::Void, Serial::Void) => true,
        (Serial::Guarded(x), Serial::Guarded(y)) => entity_text(tx, *x) == entity_text(tx, *y),
        (Serial::Guarded(_), _)
        | (_, Serial::Guarded(_))
        | (Serial::Void, _)
        | (_, Serial::Void) => false,
        _ => serial_text(tx, a) == serial_text(tx, b),
    }
}

fn serial_text(tx: &Tx, k: &Serial) -> String {
    match k {
        Serial::Name(n) => n.clone(),
        Serial::Entity(n) | Serial::Guarded(n) => entity_text(tx, *n),
        Serial::Members(g, true) => entity_text(tx, g[0]),
        Serial::Members(_, false) => "Object".to_owned(),
        Serial::Void => String::new(),
    }
}

fn entity_text(tx: &Tx, n: NodeId) -> String {
    match tx.n(n) {
        Node::QualifiedName { left, right } => {
            format!("{}.{}", entity_text(tx, left), tx.name(right))
        }
        _ => tx.name(n).to_owned(),
    }
}

/// Maximum nesting of type nodes the serializer follows (deeper reads as `Object`).
const MAX_SERIAL_DEPTH: u32 = 256;

/// `depth` counts nested type nodes; bit 31 marks a conditional type's branch, where a name
/// that cannot be resolved reads as `Object`.
fn serial_kind(tx: &Tx, ty: NodeId, depth: u32) -> Serial {
    const COND: u32 = 1 << 31;
    if depth & !COND > MAX_SERIAL_DEPTH {
        return obj();
    }
    let inner = depth + 1;
    let name = |s: &str| Serial::Name(s.to_owned());
    match tx.n(ty) {
        Node::KeywordType { kind } => match kind {
            KeywordTypeKind::Number => name("Number"),
            KeywordTypeKind::String => name("String"),
            KeywordTypeKind::Boolean => name("Boolean"),
            KeywordTypeKind::BigInt => name("BigInt"),
            KeywordTypeKind::Symbol => name("Symbol"),
            KeywordTypeKind::Void
            | KeywordTypeKind::Undefined
            | KeywordTypeKind::Null
            | KeywordTypeKind::Never => Serial::Void,
            _ => obj(),
        },
        Node::StringLit { .. } | Node::TemplateLitType { .. } => name("String"),
        Node::NumberLit { .. } => name("Number"),
        Node::BigIntLit { .. } => name("BigInt"),
        Node::TypePredicate { asserts: true, .. } => Serial::Void,
        Node::BoolLit { .. } | Node::TypePredicate { .. } => name("Boolean"),
        Node::NullLit {} => Serial::Void,
        Node::Unary { arg, .. } => serial_kind(tx, arg, inner),
        Node::FunctionType { .. } | Node::ConstructorType { .. } => name("Function"),
        Node::ArrayType { .. } | Node::TupleType { .. } => name("Array"),
        Node::ParenType { ty } | Node::OptionalType { ty } => serial_kind(tx, ty, inner),
        Node::TypeOperator { op, ty } => match op {
            TypeOperatorKind::Readonly => serial_kind(tx, ty, inner),
            TypeOperatorKind::Unique => name("Symbol"),
            TypeOperatorKind::KeyOf => obj(),
        },
        Node::ConditionalType {
            true_ty, false_ty, ..
        } => constituents(tx, &[true_ty, false_ty], false, inner | COND),
        Node::UnionType { types } | Node::IntersectionType { types } => {
            let is_union = tx.kind(ty) == NodeKind::UnionType;
            let items = tx.ast.list(types).to_vec();
            constituents(tx, &items, !is_union, inner)
        }
        Node::TypeRef { name: n, .. } => type_ref(tx, ty, n, depth & COND != 0),
        _ => obj(),
    }
}

/// The members of a union or intersection (or a conditional type's two branches) serialize
/// to their common value, or to `Object` when they differ.
fn constituents(tx: &Tx, items: &[NodeId], intersection: bool, depth: u32) -> Serial {
    let mut result: Option<Serial> = None;
    let mut guards: Vec<NodeId> = Vec::new();
    // The value, with the guards built on the way (`Serial::Members`).
    let done = |guards: Vec<NodeId>, value: Serial| -> Serial {
        if guards.is_empty() {
            return value;
        }
        let first = matches!(value, Serial::Guarded(_));
        if first && guards.len() == 1 {
            return value;
        }
        Serial::Members(guards, first)
    };
    for &t in items {
        let mut t = t;
        while let Node::ParenType { ty } = tx.n(t) {
            t = ty;
        }
        match tx.n(t) {
            Node::KeywordType {
                kind: KeywordTypeKind::Never,
            } => {
                if intersection {
                    return done(guards, Serial::Void);
                }
                continue;
            }
            Node::KeywordType {
                kind: KeywordTypeKind::Unknown,
            } => {
                if !intersection {
                    return done(guards, obj());
                }
                continue;
            }
            Node::KeywordType {
                kind: KeywordTypeKind::Any,
            } => return done(guards, obj()),
            Node::KeywordType {
                kind: KeywordTypeKind::Null | KeywordTypeKind::Undefined,
            }
            | Node::NullLit {}
                if !tx.opts.strict_null_checks =>
            {
                continue;
            }
            _ => {}
        }
        let k = match serial_kind(tx, t, depth) {
            Serial::Guarded(n) => {
                guards.push(n);
                Serial::Guarded(n)
            }
            Serial::Members(g, first) => {
                let head = if first { Serial::Guarded(g[0]) } else { obj() };
                guards.extend(g);
                head
            }
            k => k,
        };
        if matches!(&k, Serial::Name(n) if n == "Object") {
            return done(guards, k);
        }
        match &result {
            None => result = Some(k),
            Some(r) if same_serial(tx, r, &k) => {}
            Some(_) => return done(guards, obj()),
        }
    }
    done(guards, result.unwrap_or(Serial::Void))
}

/// A type reference: the checker's answer when the emitter has one, else what the file
/// alone shows.
fn type_ref(tx: &Tx, ty: NodeId, n: NodeId, in_conditional: bool) -> Serial {
    let span = tx.span(ty);
    let answer = if span.start < span.end {
        tx.resolver.metadata_type(tx.file, ty)
    } else {
        MetadataType::Syntactic
    };
    let k = match answer {
        MetadataType::Syntactic => syntactic_type_ref(tx, n),
        MetadataType::Value => Serial::Entity(n),
        MetadataType::Unknown => Serial::Guarded(n),
        MetadataType::Void => Serial::Void,
        MetadataType::Global(g) | MetadataType::TypeOnly(g) => Serial::Name(g.to_owned()),
    };
    match k {
        Serial::Guarded(_) if in_conditional => obj(),
        k => k,
    }
}

fn syntactic_type_ref(tx: &Tx, n: NodeId) -> Serial {
    let Node::Ident { name } = tx.n(n) else {
        // Qualified names: a namespace member or an enum member we cannot see into.
        return obj();
    };
    let text = tx.ast.atom(name).to_owned();
    match tx.sc.module_binding(name) {
        Some(b) => {
            let bnd = tx.sc.binding(b);
            match bnd.kind {
                BindingKind::Class => Serial::Name(text),
                BindingKind::Enum | BindingKind::ConstEnum => {
                    enum_kind(tx, bnd.decl).unwrap_or_else(obj)
                }
                BindingKind::Import
                | BindingKind::ImportEquals
                | BindingKind::Var
                | BindingKind::Let
                | BindingKind::Const => Serial::Guarded(n),
                _ => obj(),
            }
        }
        None if tx.sc.type_names.contains(&name) => obj(),
        None if GLOBAL_CTORS.contains(&text.as_str()) => Serial::Name(text),
        None => obj(),
    }
}

/// `Number` / `String` for enums whose members are all numeric / all strings.
fn enum_kind(tx: &Tx, decl: NodeId) -> Option<Serial> {
    let mut e = decl;
    // `decl` is the enum's name identifier; find the declaration through the statements.
    if tx.kind(e) == NodeKind::Ident {
        e = find_enum(tx, decl)?;
    }
    let Node::Enum { members, .. } = tx.n(e) else {
        return None;
    };
    let mut all_str = true;
    let mut all_num = true;
    for &m in tx.ast.list(members) {
        if let Node::EnumMember { init, .. } = tx.n(m) {
            match init.map(|i| tx.kind(i)) {
                Some(NodeKind::StringLit) | Some(NodeKind::TemplateLit) => all_num = false,
                None | Some(NodeKind::NumberLit) | Some(NodeKind::Unary) => all_str = false,
                _ => {
                    all_str = false;
                }
            }
        }
    }
    if all_num {
        Some(Serial::Name("Number".to_owned()))
    } else if all_str {
        Some(Serial::Name("String".to_owned()))
    } else {
        Some(obj())
    }
}

fn find_enum(tx: &Tx, name: NodeId) -> Option<NodeId> {
    let Node::Program { stmts } = tx.n(tx.ast.root) else {
        return None;
    };
    tx.ast
        .list(stmts)
        .iter()
        .copied()
        .find(|&s| matches!(tx.n(s), Node::Enum { name: n, .. } if n == name))
}
