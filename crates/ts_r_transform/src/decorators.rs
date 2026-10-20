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
                if decorators.is_empty()
                    || modifiers.contains(Modifiers::DECLARE)
                    || matches!(tx.n(key), Node::PrivateName { .. })
                {
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
    // An accessor's metadata describes the pair: the getter's return type or else the
    // setter's parameter type, and the setter's parameters (observed).
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
        m.ty = get_ret.or_else(|| {
            set_types
                .as_ref()
                .and_then(|t| t.first())
                .and_then(|t| t.ty)
        });
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
    let name_atom = match p.name {
        Some(n) => tx.atom_of(n).unwrap_or(Atom::EMPTY),
        None => {
            let a = tx.fresh("default", true);
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
    match serial_kind(tx, ty) {
        Serial::Name(n) => tx.ident(&n, at),
        Serial::Void => tx.void0(at),
        Serial::Guarded(n) => {
            // typeof (_a = typeof X !== "undefined" && X) === "function" ? _a : Object
            let t = tx.temp();
            let x1 = tx.ident(&n, at);
            let tof = tx.unary(ts_r_ast::UnaryOp::TypeOf, x1, at);
            let undef = tx.string("undefined", at);
            let ne = tx.binary(tof, ts_r_ast::BinaryOp::StrictNotEq, undef, at);
            let x2 = tx.ident(&n, at);
            let and = tx.binary(ne, ts_r_ast::BinaryOp::LogicalAnd, x2, at);
            let tl = tx.ident_atom(t, at);
            let a = tx.assign(tl, and, at);
            let pa = tx.paren(a, at);
            let tof2 = tx.unary(ts_r_ast::UnaryOp::TypeOf, pa, at);
            let fun = tx.string("function", at);
            let test = tx.binary(tof2, ts_r_ast::BinaryOp::StrictEq, fun, at);
            let cons = tx.ident_atom(t, at);
            let alt = tx.ident("Object", at);
            tx.mk(Node::Conditional { test, cons, alt }, at)
        }
    }
}

#[derive(PartialEq, Clone)]
enum Serial {
    Name(String),
    Void,
    /// An imported (or otherwise unknown) value: guarded by `typeof`.
    Guarded(String),
}

fn obj() -> Serial {
    Serial::Name("Object".to_owned())
}

fn serial_kind(tx: &Tx, ty: NodeId) -> Serial {
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
        Node::BoolLit { .. } | Node::TypePredicate { .. } => name("Boolean"),
        Node::NullLit {} => Serial::Void,
        Node::Unary { arg, .. } => serial_kind(tx, arg),
        Node::FunctionType { .. } | Node::ConstructorType { .. } => name("Function"),
        Node::ArrayType { .. } | Node::TupleType { .. } => name("Array"),
        Node::ParenType { ty } | Node::OptionalType { ty } => serial_kind(tx, ty),
        Node::TypeOperator { op, ty } => match op {
            TypeOperatorKind::Readonly => serial_kind(tx, ty),
            TypeOperatorKind::Unique => name("Symbol"),
            TypeOperatorKind::KeyOf => obj(),
        },
        Node::UnionType { types } | Node::IntersectionType { types } => {
            let is_union = tx.kind(ty) == NodeKind::UnionType;
            let mut result: Option<Serial> = None;
            for &t in tx.ast.list(types) {
                let mut inner = t;
                while let Node::ParenType { ty } = tx.n(inner) {
                    inner = ty;
                }
                let k = serial_kind(tx, inner);
                let nullish = matches!(
                    tx.n(inner),
                    Node::KeywordType {
                        kind: KeywordTypeKind::Null | KeywordTypeKind::Undefined
                    } | Node::NullLit {}
                );
                let never = matches!(
                    tx.n(inner),
                    Node::KeywordType {
                        kind: KeywordTypeKind::Never
                    }
                );
                if never {
                    continue;
                }
                if nullish {
                    if tx.opts.strict_null_checks || !is_union {
                        return obj();
                    }
                    continue;
                }
                match k {
                    Serial::Name(ref n) if n == "Object" => return obj(),
                    Serial::Void | Serial::Guarded(_) => return obj(),
                    _ => {}
                }
                match &result {
                    None => result = Some(k),
                    Some(r) if *r == k => {}
                    Some(_) => return obj(),
                }
            }
            result.unwrap_or(Serial::Void)
        }
        Node::TypeRef { name: n, .. } => type_ref(tx, n),
        _ => obj(),
    }
}

fn type_ref(tx: &Tx, n: NodeId) -> Serial {
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
                BindingKind::Import | BindingKind::ImportEquals => Serial::Guarded(text),
                BindingKind::Var | BindingKind::Let | BindingKind::Const => Serial::Guarded(text),
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
