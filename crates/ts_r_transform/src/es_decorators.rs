//! TC39 decorators, lowered like tsc 7 for every target except ESNext with
//! `useDefineForClassFields` (where they stay native).
//!
//! A decorated class becomes an arrow IIFE that declares the decorator arrays, runs
//! `__esDecorate` for every decorated member (and the class) in a static block, and threads
//! `__runInitializers` through the field initializers:
//!
//! ```js
//! let C = (() => {
//!     let _classDecorators = [dec]; let _classDescriptor; let _classExtraInitializers = [];
//!     let _classThis; let _x_decorators; let _x_initializers = []; let _x_extraInitializers = [];
//!     var C = class {
//!         static { _classThis = this; }
//!         static { const _metadata = ...; __esDecorate(null, null, _x_decorators, { kind: "field", ... }, ...);
//!                  __esDecorate(null, _classDescriptor = { value: _classThis }, _classDecorators, ...);
//!                  C = _classThis = _classDescriptor.value; if (_metadata) ...;
//!                  __runInitializers(_classThis, _classExtraInitializers); }
//!         x = __runInitializers(this, _x_initializers, 1);
//!         constructor() { __runInitializers(this, _x_extraInitializers); }
//!     };
//!     return C = _classThis;
//! })();
//! ```
//!
//! Decorator expressions are evaluated in source order, interleaved with computed keys: they
//! are folded into the next computed key (`[(_x_decorators = [dec], _a = __propKey(k))]`), or
//! into the decoration block when no computed key follows (`this` there becomes `_outerThis`).
//!
//! In static fields and blocks of a class-decorated derived class, `super` reads, calls and
//! tags go through `Reflect.get(_classSuper, k, _classThis)`.
//!
//! Not done (tsc does): static private members of class-decorated classes are not moved to
//! `WeakMap`-style storage, writes to `super` properties in their static members are not
//! lowered to `Reflect.set` (with temporaries), and anonymous classes get no
//! `__setFunctionName` from the binding they are assigned to (named evaluation).

use rustc_hash::FxHashSet;
use ts_r_ast::{
    Atom, BinaryOp, FnFlags, HeritageKind, MethodKind, Modifiers, Node, NodeId, NodeKind, NodeList,
    UnaryOp, VarKind,
};

use crate::builder::{Build, clone_node, deep_clone, replace};
use crate::class::{parts, set_parts};
use crate::helpers::Helper;
use crate::transform::Tx;

/// A class declaration with TC39 decorators: `let C = (() => { ... })();` plus exports.
pub(crate) fn class_decl(tx: &mut Tx, id: NodeId, out: &mut Vec<NodeId>) {
    if !has_es_decorators(tx, id) {
        plain(tx, id, out);
        return;
    }
    let p = parts(tx, id).expect("class");
    let at = tx.at(id);
    let exported = p.modifiers.contains(Modifiers::EXPORT);
    let is_default = p.modifiers.contains(Modifiers::DEFAULT);
    let source_name = p.name;
    let cjs_top = tx.opts.is_cjs() && tx.ns_stack.is_empty() && tx.list_depth == 0;
    let (binding, anonymous_default) = match p.name.and_then(|n| tx.atom_of(n)) {
        Some(a) => (a, false),
        None => match crate::cjs::default_name(tx) {
            Some(a) if cjs_top => (a, true),
            _ => (tx.fresh("default", true), true),
        },
    };
    let fn_name = anonymous_default.then(|| "default".to_owned());
    let iife = lower(tx, id, binding, fn_name);
    if anonymous_default && tx.ns_stack.is_empty() {
        // `export default (() => ...)();`, in CommonJS `exports.default = (() => ...)();`
        // (observed).
        let s = if tx.opts.is_cjs() {
            crate::cjs::export_assign(tx, "default", iife, at)
        } else {
            tx.mk(Node::ExportDefaultExpr { expr: iife }, at)
        };
        tx.ast.set_span(s, tx.span(id));
        out.push(s);
        return;
    }
    let local = tx.ident_atom(binding, at);
    let decl = tx.var(VarKind::Let, Modifiers::NONE, local, Some(iife), at);
    tx.ast.set_span(decl, tx.span(id));
    out.push(decl);
    if exported {
        crate::decorators::export_after(tx, binding, is_default, at, out);
    }
    // CommonJS: `exports.D = C;` for `export { C as D }` (observed).
    if cjs_top && let Some(n) = source_name {
        let local = tx.ast.atom(binding).to_owned();
        for a in crate::cjs::take_local_exports(tx, n) {
            crate::cjs::export_binding(tx, &local, &a, at, out);
        }
    }
}

/// A decorated class expression becomes the IIFE in place.
pub(crate) fn class_expr(tx: &mut Tx, id: NodeId, name_hint: Option<Atom>) {
    if !has_es_decorators(tx, id) {
        crate::class::class_body(tx, id);
        crate::class::clear_decorators(tx, id);
        return;
    }
    let p = parts(tx, id).expect("class");
    let binding = match p.name.and_then(|n| tx.atom_of(n)) {
        Some(a) => a,
        None if !p.decorators.is_empty() => tx.fresh("class", true),
        None => Atom::EMPTY,
    };
    // Anonymous classes are named after their binding (or "" when there is none).
    let fn_name = match (p.name, name_hint) {
        (None, Some(a)) => Some(tx.ast.atom(a).to_owned()),
        (None, None) if !p.decorators.is_empty() => Some(String::new()),
        _ => None,
    };
    let cls = clone_node(tx.ast, id);
    let iife = lower(tx, cls, binding, fn_name);
    replace(tx.ast, id, iife);
}

/// Only parameter decorators (an error in TC39 mode): strip them and emit the class as is.
fn plain(tx: &mut Tx, id: NodeId, out: &mut Vec<NodeId>) {
    crate::class::class_body(tx, id);
    crate::class::clear_decorators(tx, id);
    let p = parts(tx, id).expect("class");
    crate::class::export_decl(tx, id, p.modifiers, p.name, out);
}

fn has_es_decorators(tx: &Tx, id: NodeId) -> bool {
    let Some(p) = parts(tx, id) else {
        return false;
    };
    !p.decorators.is_empty() || members_decorated(tx, p.members)
}

/// True if a member that is emitted (not abstract, ambient or an overload) has decorators.
fn members_decorated(tx: &Tx, members: NodeList) -> bool {
    tx.ast.list(members).iter().any(|&m| match tx.n(m) {
        Node::PropertyDef {
            decorators,
            modifiers,
            ..
        } => {
            !decorators.is_empty()
                && !modifiers.intersects(Modifiers::DECLARE | Modifiers::ABSTRACT)
        }
        Node::MethodDef {
            decorators,
            kind,
            modifiers,
            value,
            ..
        } => {
            !decorators.is_empty()
                && kind != MethodKind::Constructor
                && !modifiers.contains(Modifiers::ABSTRACT)
                && matches!(tx.n(value), Node::FunctionExpr { body: Some(_), .. })
        }
        _ => false,
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Method,
    Getter,
    Setter,
    Field,
    Accessor,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Method => "method",
            Kind::Getter => "getter",
            Kind::Setter => "setter",
            Kind::Field => "field",
            Kind::Accessor => "accessor",
        }
    }

    fn is_field(self) -> bool {
        self == Kind::Field
    }

    /// Methods, getters and setters share the class-wide extra initializers.
    fn is_method_like(self) -> bool {
        matches!(self, Kind::Method | Kind::Getter | Kind::Setter)
    }
}

/// The member name as the decorator context and the access functions see it.
#[derive(Clone, Copy)]
enum Name {
    /// `x`: `name: "x"`, `obj.x`.
    Ident(Atom),
    /// `#x` (atom without `#`).
    Private(Atom),
    /// `"x"`, `0`, `["x"]`: the name text and the literal node (for `obj["x"]`).
    Literal(Atom, NodeId),
    /// A computed key evaluated once into a temporary.
    Temp(Atom),
}

struct Decorated {
    kind: Kind,
    is_static: bool,
    name: Name,
    decorators: Atom,
    initializers: Option<Atom>,
    extra: Option<Atom>,
    descriptor: Option<Atom>,
    /// Private methods/getters/setters: the member, replaced by a forwarding accessor.
    member: Option<NodeId>,
    /// The descriptor object of a private member (`{ value: __setFunctionName(...) }`).
    descriptor_value: Option<NodeId>,
    /// `__runInitializers(this, _x_initializers, v)`, the initializer of a field/accessor.
    wrapper: Option<NodeId>,
    at: u32,
}

impl Decorated {
    fn is_private(&self) -> bool {
        matches!(self.name, Name::Private(_))
    }
}

/// Per-class state of the lowering.
struct Lower {
    decorated: Vec<Decorated>,
    /// `_x_decorators = [...]` assignments not folded into a computed key.
    pending: Vec<NodeId>,
    used: FxHashSet<String>,
    storage_count: u32,
    /// Private names declared in the class and storage bases already used.
    private_names: FxHashSet<String>,
}

/// Lowers class `cls` (in place, turned into a class expression) and returns the IIFE call.
/// `binding` names the class inside the IIFE when it has class decorators.
fn lower(tx: &mut Tx, cls: NodeId, binding: Atom, fn_name: Option<String>) -> NodeId {
    let at = tx.at(cls);
    tx.temps.push(Vec::new());
    let mut p = parts(tx, cls).expect("class");
    let class_decorated = !p.decorators.is_empty();
    // Helper request order (tsc prints them in that order): the constructor (which runs
    // `_instanceExtraInitializers`) is visited before the members.
    let instance_methods_decorated = tx.items(p.members).iter().any(|&m| {
        matches!(tx.n(m), Node::MethodDef { decorators, modifiers, kind, .. }
            if !decorators.is_empty() && kind != MethodKind::Constructor
                && !modifiers.intersects(Modifiers::STATIC | Modifiers::ABSTRACT))
    });
    // A decorated class asks for it first for decorated instance fields and accessors too
    // (observed: `@dec class D { @dec accessor y = 1; }`; without the class decorator
    // `__esDecorate` comes first).
    let instance_fields_decorated = class_decorated
        && tx.items(p.members).iter().any(|&m| {
            matches!(tx.n(m), Node::PropertyDef { decorators, modifiers, .. }
                if !decorators.is_empty()
                    && !modifiers.intersects(Modifiers::STATIC | Modifiers::ABSTRACT | Modifiers::DECLARE))
        });
    if instance_methods_decorated || instance_fields_decorated {
        tx.need(Helper::RunInitializers);
    }
    // A decorated class with a static private member or a static `accessor` gets its private
    // statics lowered by the class-fields pass, which names the (now anonymous) class first
    // with `static { __setFunctionName(this, "C"); }`. Static blocks, fields and methods alone
    // don't (tsc 7.0.2: estransforms/esdecorator.go marks the class when it
    // `hasStaticPrivateClassElements`; classfields.go `isAnonymousClassNeedingAssignedName`).
    let static_code = tx.items(p.members).iter().any(|&m| match tx.n(m) {
        Node::PropertyDef { modifiers, key, .. } => {
            modifiers.contains(Modifiers::STATIC)
                && (tx.kind(key) == NodeKind::PrivateName
                    || modifiers.contains(Modifiers::ACCESSOR))
        }
        Node::MethodDef { modifiers, key, .. } => {
            modifiers.contains(Modifiers::STATIC) && tx.kind(key) == NodeKind::PrivateName
        }
        _ => false,
    });
    tx.need(Helper::EsDecorate);
    tx.need(Helper::RunInitializers);
    let derived = tx.items(p.heritage).iter().any(
        |&h| matches!(tx.n(h), Node::HeritageClause { kind, .. } if kind == HeritageKind::Extends),
    );
    let class_name = p.name.map(|n| tx.name(n).to_owned());
    let mut st = Lower {
        decorated: Vec::new(),
        pending: Vec::new(),
        used: FxHashSet::default(),
        storage_count: 0,
        private_names: FxHashSet::default(),
    };
    for m in tx.items(p.members) {
        let key = match tx.n(m) {
            Node::PropertyDef { key, .. } | Node::MethodDef { key, .. } => key,
            _ => continue,
        };
        if let Node::PrivateName { name } = tx.n(key) {
            st.private_names.insert(tx.ast.atom(name).to_owned());
        }
    }
    let receiver_name = if class_decorated {
        None
    } else {
        class_name.clone()
    };
    let members = prepare(tx, &mut st, p.members, receiver_name.as_deref());
    p.members = tx.new_list(&members);
    set_parts(tx, cls, p);

    crate::class::class_body(tx, cls);
    let p = parts(tx, cls).expect("class");
    let class_decorators: Vec<NodeId> = decorator_exprs(tx, p.decorators)
        .into_iter()
        .map(|d| bind_this(tx, d))
        .collect();
    crate::class::clear_decorators(tx, cls);
    let mut p = parts(tx, cls).expect("class");
    let define = tx.opts.use_define_for_class_fields;
    // `class extends _classSuper` with `let _classSuper = Base;` (metadata inherits from it).
    let mut class_super: Option<NodeId> = None;
    for h in tx.items(p.heritage) {
        if let Node::HeritageClause {
            kind: HeritageKind::Extends,
            types,
        } = tx.n(h)
            && let Some(&t) = tx.ast.list(types).first()
            && let Node::ExprWithTypeArgs { expr, type_args } = tx.n(t)
        {
            let r = tx.ident("_classSuper", at);
            tx.set(t, Node::ExprWithTypeArgs { expr: r, type_args });
            class_super = Some(expr);
        }
    }

    let mut members = tx.items(p.members);
    // A constructor synthesized by `class_body` goes after the members.
    if let Some(i) = members
        .iter()
        .position(|&m| is_ctor(tx, m) && is_synth(tx, m))
    {
        let c = members.remove(i);
        members.push(c);
    }
    // Leading synthesized static blocks evaluate hoisted computed keys.
    let key_blocks = members
        .iter()
        .take_while(|&&m| tx.kind(m) == NodeKind::StaticBlock && is_synth(tx, m))
        .count();

    // Private methods, getters and setters move into descriptors.
    for i in 0..st.decorated.len() {
        let d = &st.decorated[i];
        if let (Some(m), Name::Private(name), true) = (d.member, d.name, d.kind.is_method_like()) {
            let desc = d.descriptor.unwrap_or(Atom::EMPTY);
            let (value, made) = private_method(tx, m, name, desc, d.kind, d.at);
            st.decorated[i].descriptor_value = Some(value);
            replace(tx.ast, m, made);
        }
    }

    let static_extra = st
        .decorated
        .iter()
        .any(|d| d.is_static && d.kind.is_method_like())
        .then(|| tx.ast.intern("_staticExtraInitializers"));
    let instance_extra = st
        .decorated
        .iter()
        .any(|d| !d.is_static && d.kind.is_method_like())
        .then(|| tx.ast.intern("_instanceExtraInitializers"));
    let class_this = class_decorated.then(|| tx.ast.intern("_classThis"));

    // Extra initializers run before the next field's initializer (per placement).
    let ctor = members.iter().copied().find(|&m| is_ctor(tx, m));
    let instance_slots = instance_slots(tx, &members, ctor, define);
    let instance_left = chain(tx, &st, &instance_slots, instance_extra, at);
    let static_slots = static_slots(tx, &members[key_blocks..]);
    let static_left = chain(tx, &st, &static_slots, static_extra, at);

    if let Some(ct) = class_this {
        let sup = class_super.map(|_| tx.ast.intern("_classSuper"));
        for &m in &members[key_blocks..] {
            match tx.n(m) {
                Node::PropertyDef {
                    modifiers,
                    value: Some(v),
                    ..
                } if modifiers.contains(Modifiers::STATIC) => {
                    replace_this(tx, v, ct);
                    if let Some(sup) = sup {
                        replace_super_reads(tx, v, ct, sup);
                    }
                }
                Node::StaticBlock { body } => {
                    for s in tx.items(body) {
                        replace_this(tx, s, ct);
                        if let Some(sup) = sup {
                            replace_super_reads(tx, s, ct, sup);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    let has_static_init = members[key_blocks..].iter().any(|&m| match tx.n(m) {
        Node::PropertyDef { modifiers, .. } => modifiers.contains(Modifiers::STATIC),
        Node::StaticBlock { .. } => true,
        _ => false,
    });

    if let Some(e) = instance_left {
        let this = tx.this(at);
        let s = run_initializers_stmt(tx, this, e, at);
        match ctor {
            Some(c) => append_to_ctor(tx, c, s, !define),
            None => {
                let mut body = Vec::with_capacity(2);
                if derived {
                    let sup = tx.mk(Node::Super {}, at);
                    let args = tx.ident("arguments", at);
                    let spread = tx.spread(args, at);
                    let call = tx.call(sup, &[spread], at);
                    body.push(tx.expr_stmt(call, at));
                }
                body.push(s);
                let key = tx.ident("constructor", at);
                let c = tx.method(
                    Modifiers::NONE,
                    MethodKind::Constructor,
                    key,
                    &[],
                    &body,
                    at,
                );
                members.push(c);
            }
        }
    }

    // Decorator arrays evaluated in the decoration block; `this` there is the outer `this`.
    let mut outer_this: Option<Atom> = None;
    let pending = std::mem::take(&mut st.pending);
    for &a in &pending {
        tx.expr(a);
        if contains_this(tx, a) {
            let name = *outer_this.get_or_insert_with(|| tx.fresh("_outerThis", false));
            replace_this(tx, a, name);
        }
    }

    // The decoration block.
    let mut block: Vec<NodeId> = Vec::new();
    block.push(metadata_decl(tx, class_super.is_some(), at));
    for &a in &pending {
        block.push(tx.expr_stmt(a, at));
    }
    let groups: [(bool, bool); 4] = [(true, false), (false, false), (true, true), (false, true)];
    for (is_static, fields) in groups {
        for d in st
            .decorated
            .iter()
            .filter(|d| d.is_static == is_static && d.kind.is_field() == fields)
        {
            let extra = if d.is_static {
                static_extra
            } else {
                instance_extra
            };
            let call = es_decorate(tx, d, extra);
            block.push(tx.expr_stmt(call, d.at));
        }
    }
    if class_decorated {
        let class_decorate = class_decorate(tx, binding, at);
        block.extend(class_decorate);
    }
    let meta_target = match class_this {
        Some(ct) => tx.ident_atom(ct, at),
        None => tx.this(at),
    };
    block.push(define_metadata(tx, meta_target, at));
    let recv = |tx: &mut Tx| match class_this {
        Some(ct) => tx.ident_atom(ct, at),
        None => tx.this(at),
    };
    let mut trailing: Vec<NodeId> = Vec::new();
    if let Some(e) = static_left {
        let r = recv(tx);
        let s = run_initializers_stmt(tx, r, e, at);
        if Some(e) == static_extra {
            block.push(s);
        } else {
            trailing.push(s);
        }
    }
    if let Some(ct) = class_this {
        let r = tx.ident_atom(ct, at);
        let extra = tx.ast.intern("_classExtraInitializers");
        let s = run_initializers_stmt(tx, r, extra, at);
        if has_static_init {
            trailing.push(s);
        } else {
            block.push(s);
        }
    }

    // Members: [class this] [function name] [key blocks] decoration block, members, trailing.
    let mut out: Vec<NodeId> = Vec::with_capacity(members.len() + 5);
    if let Some(ct) = class_this {
        let target = tx.ident_atom(ct, at);
        let this = tx.this(at);
        let a = tx.assign(target, this, at);
        let s = tx.expr_stmt(a, at);
        let b = static_block(tx, &[s], at);
        tx.mark_single_line(b);
        tx.inline_static.push(b);
        out.push(b);
        let named = match &fn_name {
            Some(n) => Some((tx.ident_atom(ct, at), n.clone())),
            None if static_code => Some((tx.this(at), tx.ast.atom(binding).to_owned())),
            None => None,
        };
        if let Some((target, name)) = named {
            out.push(set_function_name_block(tx, target, &name, at));
        }
    } else if let Some(n) = &fn_name {
        let this = tx.this(at);
        out.push(set_function_name_block(tx, this, n, at));
    }
    out.extend_from_slice(&members[..key_blocks]);
    let decoration = static_block(tx, &block, at);
    out.push(decoration);
    out.extend_from_slice(&members[key_blocks..]);
    if !trailing.is_empty() {
        out.push(static_block(tx, &trailing, at));
    }
    p.members = tx.new_list(&out);
    p.modifiers = Modifiers::NONE;
    p.is_expr = true;
    p.decorators = NodeList::EMPTY;
    if class_decorated {
        p.name = None;
    }
    set_parts(tx, cls, p);

    // The IIFE body.
    let temps = tx.temps.pop().unwrap_or_default();
    let mut body: Vec<NodeId> = Vec::new();
    if let Some(t) = tx.temps_decl(temps) {
        body.push(t);
    }
    if let Some(o) = outer_this {
        let this = tx.this(at);
        body.push(let_decl(tx, o, Some(this), at));
    }
    if class_decorated {
        let decs = tx.array(&class_decorators, at);
        body.push(let_named(tx, "_classDecorators", Some(decs), at));
        body.push(let_named(tx, "_classDescriptor", None, at));
        let empty = tx.array(&[], at);
        body.push(let_named(tx, "_classExtraInitializers", Some(empty), at));
        body.push(let_named(tx, "_classThis", None, at));
    }
    if let Some(e) = class_super {
        // An anonymous class, function or arrow would take its name from `_classSuper`:
        // tsc writes `(0, e)` instead (esdecorator.go, transformClassLike step 2).
        let e = if is_anonymous_fn(tx, e) {
            let zero = tx.number(0.0, at);
            let seq = tx.seq(&[zero, e], at);
            tx.paren(seq, at)
        } else {
            e
        };
        body.push(let_named(tx, "_classSuper", Some(e), at));
    }
    for e in [static_extra, instance_extra].into_iter().flatten() {
        let empty = tx.array(&[], at);
        body.push(let_decl(tx, e, Some(empty), at));
    }
    for is_static in [true, false] {
        let vars: Vec<(Atom, bool)> = st
            .decorated
            .iter()
            .filter(|d| d.is_static == is_static)
            .flat_map(|d| {
                [
                    Some((d.decorators, false)),
                    d.initializers.map(|a| (a, true)),
                    d.extra.map(|a| (a, true)),
                    d.descriptor.map(|a| (a, false)),
                ]
            })
            .flatten()
            .collect();
        for (name, array) in vars {
            let init = array.then(|| tx.array(&[], at));
            body.push(let_decl(tx, name, init, at));
        }
    }
    if class_decorated {
        let local = tx.ident_atom(binding, at);
        let decl = tx.var(VarKind::Var, Modifiers::NONE, local, Some(cls), at);
        body.push(decl);
        let target = tx.ident_atom(binding, at);
        let ct = tx.ident("_classThis", at);
        let a = tx.assign(target, ct, at);
        body.push(tx.ret(Some(a), at));
    } else {
        body.push(tx.ret(Some(cls), at));
    }
    let block = tx.block(&body, at);
    let params = tx.new_list(&[]);
    let arrow = tx.mk(
        Node::Arrow {
            flags: FnFlags::NONE,
            type_params: None,
            params,
            return_type: None,
            body: block,
        },
        at,
    );
    tx.call(arrow, &[], at)
}

/// Collects decorated members before `class_body` runs: decorator arrays, initializer
/// wrappers, `accessor` lowering, and computed keys that absorb pending decorator arrays.
/// Expressions stay untransformed (`class_body` transforms keys and initializers once).
fn prepare(
    tx: &mut Tx,
    st: &mut Lower,
    members: NodeList,
    class_name: Option<&str>,
) -> Vec<NodeId> {
    let mut out = Vec::new();
    for m in tx.items(members) {
        match tx.n(m) {
            Node::MethodDef {
                decorators,
                modifiers,
                kind,
                key,
                optional,
                value,
            } => {
                let has_body = matches!(tx.n(value), Node::FunctionExpr { body: Some(_), .. });
                if kind == MethodKind::Constructor
                    || !has_body
                    || modifiers.contains(Modifiers::ABSTRACT)
                {
                    out.push(m);
                    continue;
                }
                if decorators.is_empty() {
                    fold_pending(tx, st, key);
                    out.push(m);
                    continue;
                }
                let at = tx.at(m);
                let is_static = modifiers.contains(Modifiers::STATIC);
                let dk = match kind {
                    MethodKind::Get => Kind::Getter,
                    MethodKind::Set => Kind::Setter,
                    _ => Kind::Method,
                };
                let decs = decorator_exprs(tx, decorators);
                tx.set(
                    m,
                    Node::MethodDef {
                        decorators: NodeList::EMPTY,
                        modifiers,
                        kind,
                        key,
                        optional,
                        value,
                    },
                );
                let (base, suffix) = var_base(tx, st, is_static, dk, key);
                let decorators_var = tx.ast.intern(&format!("{base}_decorators{suffix}"));
                push_decorators(tx, st, decorators_var, &decs, at);
                let name = key_name(tx, st, key, at);
                let private = matches!(name, Name::Private(_));
                let descriptor =
                    private.then(|| tx.ast.intern(&format!("{base}_descriptor{suffix}")));
                st.decorated.push(Decorated {
                    kind: dk,
                    is_static,
                    name,
                    decorators: decorators_var,
                    initializers: None,
                    extra: None,
                    descriptor,
                    member: Some(m),
                    descriptor_value: None,
                    wrapper: None,
                    at,
                });
                out.push(m);
            }
            Node::PropertyDef {
                decorators,
                modifiers,
                key,
                optional,
                definite,
                type_ann,
                value,
            } => {
                if modifiers.intersects(Modifiers::DECLARE | Modifiers::ABSTRACT) {
                    out.push(m);
                    continue;
                }
                if decorators.is_empty() {
                    fold_pending(tx, st, key);
                    out.push(m);
                    continue;
                }
                let at = tx.at(m);
                let is_static = modifiers.contains(Modifiers::STATIC);
                let is_accessor = modifiers.contains(Modifiers::ACCESSOR);
                let dk = if is_accessor {
                    Kind::Accessor
                } else {
                    Kind::Field
                };
                let decs = decorator_exprs(tx, decorators);
                let (base, suffix) = var_base(tx, st, is_static, dk, key);
                let decorators_var = tx.ast.intern(&format!("{base}_decorators{suffix}"));
                let initializers = tx.ast.intern(&format!("{base}_initializers{suffix}"));
                let extra = tx.ast.intern(&format!("{base}_extraInitializers{suffix}"));
                push_decorators(tx, st, decorators_var, &decs, at);
                let raw_key = tx.n(key);
                let name = key_name(tx, st, key, at);
                // `__runInitializers(this, _x_initializers, value)`
                let f = tx.helper_ref(Helper::RunInitializers, at);
                let this = tx.this(at);
                let inits = tx.ident_atom(initializers, at);
                let v = match value {
                    Some(v) => v,
                    None => tx.void0(at),
                };
                let wrapper = tx.call(f, &[this, inits, v], at);
                let mut descriptor = None;
                let mut descriptor_value = None;
                if is_accessor {
                    let storage = storage_name(tx, st, raw_key);
                    let mut mods = Modifiers::NONE;
                    if is_static {
                        mods.insert(Modifiers::STATIC);
                    }
                    let skey = tx.mk(Node::PrivateName { name: storage }, at);
                    let field = tx.mk(
                        Node::PropertyDef {
                            decorators: NodeList::EMPTY,
                            modifiers: mods,
                            key: skey,
                            optional: false,
                            definite: false,
                            type_ann: None,
                            value: Some(wrapper),
                        },
                        at,
                    );
                    tx.ast.set_span(field, tx.span(m));
                    tx.erase_opt(type_ann);
                    out.push(field);
                    let set_key = match name {
                        Name::Temp(t) => {
                            let r = tx.ident_atom(t, at);
                            tx.mk(Node::ComputedKey { expr: r }, at)
                        }
                        _ => deep_clone(tx.ast, key),
                    };
                    if let Name::Private(pn) = name {
                        let d = tx.ast.intern(&format!("{base}_descriptor{suffix}"));
                        descriptor = Some(d);
                        descriptor_value = Some(private_accessor_descriptor(tx, pn, storage, at));
                        let (g, s) = forwarding_accessors(tx, key, set_key, d, is_static, at);
                        out.push(g);
                        out.push(s);
                    } else {
                        let (g, s) = crate::class::accessor_methods(
                            tx, key, set_key, storage, is_static, class_name, at,
                        );
                        out.push(g);
                        out.push(s);
                    }
                } else {
                    tx.set(
                        m,
                        Node::PropertyDef {
                            decorators: NodeList::EMPTY,
                            modifiers,
                            key,
                            optional,
                            definite,
                            type_ann,
                            value: Some(wrapper),
                        },
                    );
                    out.push(m);
                }
                st.decorated.push(Decorated {
                    kind: dk,
                    is_static,
                    name,
                    decorators: decorators_var,
                    initializers: Some(initializers),
                    extra: Some(extra),
                    descriptor,
                    member: None,
                    descriptor_value,
                    wrapper: Some(wrapper),
                    at,
                });
            }
            _ => out.push(m),
        }
    }
    out
}

fn decorator_exprs(tx: &Tx, list: NodeList) -> Vec<NodeId> {
    tx.items(list)
        .into_iter()
        .filter_map(|d| match tx.n(d) {
            Node::Decorator { expr } => Some(expr),
            _ => None,
        })
        .collect()
}

fn push_decorators(tx: &mut Tx, st: &mut Lower, var: Atom, decs: &[NodeId], at: u32) {
    let decs: Vec<NodeId> = decs.iter().map(|&d| bind_this(tx, d)).collect();
    let target = tx.ident_atom(var, at);
    let arr = tx.array(&decs, at);
    let a = tx.assign(target, arr, at);
    st.pending.push(a);
}

/// A decorator that is a property access keeps its receiver: `(_a = obj).dec.bind(_a)`,
/// `super.dec.bind(this)` (parentheses kept).
fn bind_this(tx: &mut Tx, e: NodeId) -> NodeId {
    let at = tx.at(e);
    match tx.n(e) {
        Node::Paren { expr } => {
            let inner = bind_this(tx, expr);
            tx.set(e, Node::Paren { expr: inner });
            e
        }
        Node::Member {
            obj,
            optional: false,
            ..
        }
        | Node::Index {
            obj,
            optional: false,
            ..
        } => {
            let receiver = if tx.kind(obj) == NodeKind::Super {
                tx.this(at)
            } else {
                let t = tx.temp();
                let target = tx.ident_atom(t, at);
                let a = tx.assign(target, obj, at);
                match tx.n(e) {
                    Node::Member { prop, .. } => tx.set(
                        e,
                        Node::Member {
                            obj: a,
                            optional: false,
                            prop,
                        },
                    ),
                    Node::Index { index, .. } => tx.set(
                        e,
                        Node::Index {
                            obj: a,
                            optional: false,
                            index,
                        },
                    ),
                    _ => {}
                }
                tx.ident_atom(t, at)
            };
            let bind = tx.member(e, "bind", at);
            tx.call(bind, &[receiver], at)
        }
        _ => e,
    }
}

/// `_static_private_get_x` and a uniqueness suffix (`""`, `"_1"`, ...) for a member's variables.
fn var_base(
    tx: &mut Tx,
    st: &mut Lower,
    is_static: bool,
    kind: Kind,
    key: NodeId,
) -> (String, String) {
    let mut base = String::from("_");
    if is_static {
        base.push_str("static_");
    }
    let name = match tx.n(key) {
        Node::Ident { name } => Some(tx.ast.atom(name).to_owned()),
        Node::PrivateName { name } => {
            base.push_str("private_");
            Some(tx.ast.atom(name).to_owned())
        }
        _ => None,
    };
    match kind {
        Kind::Getter => base.push_str("get_"),
        Kind::Setter => base.push_str("set_"),
        _ => {}
    }
    base.push_str(name.as_deref().unwrap_or("member"));
    let mut i = 0usize;
    loop {
        let suffix = if i == 0 {
            String::new()
        } else {
            format!("_{i}")
        };
        let cand = format!("{base}_decorators{suffix}");
        let atom = tx.ast.intern(&cand);
        if !st.used.contains(&cand) && !tx.sc.names.contains(&atom) {
            st.used.insert(cand);
            return (base, suffix);
        }
        i += 1;
    }
}

/// The context name of a decorated member's key. A non-literal computed key becomes
/// `[(<pending decorators>, _a = __propKey(expr))]`.
fn key_name(tx: &mut Tx, st: &mut Lower, key: NodeId, at: u32) -> Name {
    match tx.n(key) {
        Node::Ident { name } => Name::Ident(name),
        Node::PrivateName { name } => Name::Private(name),
        Node::ComputedKey { expr } => match literal_text(tx, expr) {
            Some(t) => Name::Literal(t, expr),
            None => {
                let t = tx.temp();
                let f = tx.helper_ref(Helper::PropKey, at);
                let call = tx.call(f, &[expr], at);
                let target = tx.ident_atom(t, at);
                let a = tx.assign(target, call, at);
                let mut items = std::mem::take(&mut st.pending);
                items.push(a);
                let seq = tx.seq(&items, at);
                tx.set(key, Node::ComputedKey { expr: seq });
                Name::Temp(t)
            }
        },
        _ => match literal_text(tx, key) {
            Some(t) => Name::Literal(t, key),
            None => Name::Literal(Atom::EMPTY, key),
        },
    }
}

/// An undecorated member's computed key evaluates the pending decorator arrays first.
fn fold_pending(tx: &mut Tx, st: &mut Lower, key: NodeId) {
    if st.pending.is_empty() {
        return;
    }
    if let Node::ComputedKey { expr } = tx.n(key)
        && literal_text(tx, expr).is_none()
    {
        let at = tx.at(key);
        let mut items = std::mem::take(&mut st.pending);
        items.push(expr);
        let seq = tx.seq(&items, at);
        tx.set(key, Node::ComputedKey { expr: seq });
    }
}

fn literal_text(tx: &mut Tx, id: NodeId) -> Option<Atom> {
    match tx.n(id) {
        Node::StringLit { value } => Some(value),
        Node::NumberLit { value } => {
            let s = ts_r_codegen::literals::js_number(value.get());
            Some(tx.ast.intern(&s))
        }
        Node::TemplateLit { parts } if tx.ast.list(parts).len() == 1 => {
            match tx.n(tx.ast.list(parts)[0]) {
                Node::TemplateElement { cooked, .. } => Some(cooked),
                _ => None,
            }
        }
        _ => None,
    }
}

/// `x_accessor_storage` for `x`/`#x`; `_a_accessor_storage`, `_b_...` otherwise.
fn storage_name(tx: &mut Tx, st: &mut Lower, key: Node) -> Atom {
    let base = match key {
        Node::Ident { name } => {
            let n = tx.ast.atom(name).to_owned();
            let mut cand = n.clone();
            let mut i = 0;
            while st.private_names.contains(&cand) {
                i += 1;
                cand = format!("{n}_{i}");
            }
            st.private_names.insert(cand.clone());
            cand
        }
        Node::PrivateName { name } => tx.ast.atom(name).to_owned(),
        _ => {
            let i = st.storage_count;
            st.storage_count += 1;
            if i < 26 {
                format!("_{}", (b'a' + i as u8) as char)
            } else {
                format!("_{}", i - 26)
            }
        }
    };
    let name = tx.ast.intern(&format!("{base}_accessor_storage"));
    tx.decorator_storages.push(name);
    name
}

/// `{ get: __setFunctionName(function () { return this.#s; }, "#x", "get"), set: ... }`
fn private_accessor_descriptor(tx: &mut Tx, name: Atom, storage: Atom, at: u32) -> NodeId {
    let fname = format!("#{}", tx.ast.atom(name));
    // getter
    let this = tx.this(at);
    let p = tx.mk(Node::PrivateName { name: storage }, at);
    let read = tx.mk(
        Node::Member {
            obj: this,
            optional: false,
            prop: p,
        },
        at,
    );
    let ret = tx.ret(Some(read), at);
    let getter = tx.function_expr(None, &[], &[ret], at);
    tx.mark_single_line_body(getter);
    let f = tx.helper_ref(Helper::SetFunctionName, at);
    let n = tx.string(&fname, at);
    let k = tx.string("get", at);
    let get = tx.call(f, &[getter, n, k], at);
    let get = tx.prop("get", get, at);
    // setter
    let this = tx.this(at);
    let p = tx.mk(Node::PrivateName { name: storage }, at);
    let target = tx.mk(
        Node::Member {
            obj: this,
            optional: false,
            prop: p,
        },
        at,
    );
    let v = tx.ident("value", at);
    let a = tx.assign(target, v, at);
    let s = tx.expr_stmt(a, at);
    let pv = tx.ident("value", at);
    let param = tx.param(pv, at);
    let setter = tx.function_expr(None, &[param], &[s], at);
    tx.mark_single_line_body(setter);
    let f = tx.helper_ref(Helper::SetFunctionName, at);
    let n = tx.string(&fname, at);
    let k = tx.string("set", at);
    let set = tx.call(f, &[setter, n, k], at);
    let set = tx.prop("set", set, at);
    tx.object(&[get, set], at)
}

/// `get #x() { return _d.get.call(this); }` and `set #x(value) { return _d.set.call(this, value); }`.
fn forwarding_accessors(
    tx: &mut Tx,
    get_key: NodeId,
    set_key: NodeId,
    desc: Atom,
    is_static: bool,
    at: u32,
) -> (NodeId, NodeId) {
    let g = forward(tx, get_key, desc, Kind::Getter, is_static, at);
    let s = forward(tx, set_key, desc, Kind::Setter, is_static, at);
    (g, s)
}

/// The member that replaces a decorated private method/getter/setter.
fn forward(tx: &mut Tx, key: NodeId, desc: Atom, kind: Kind, is_static: bool, at: u32) -> NodeId {
    let mods = if is_static {
        Modifiers::STATIC
    } else {
        Modifiers::NONE
    };
    let d = tx.ident_atom(desc, at);
    let (mk, params, ret) = match kind {
        Kind::Getter => {
            let g = tx.member(d, "get", at);
            let c = tx.member(g, "call", at);
            let this = tx.this(at);
            let call = tx.call(c, &[this], at);
            (MethodKind::Get, vec![], call)
        }
        Kind::Setter => {
            let g = tx.member(d, "set", at);
            let c = tx.member(g, "call", at);
            let this = tx.this(at);
            let v = tx.ident("value", at);
            let call = tx.call(c, &[this, v], at);
            let pv = tx.ident("value", at);
            let param = tx.param(pv, at);
            (MethodKind::Set, vec![param], call)
        }
        _ => {
            let v = tx.member(d, "value", at);
            (MethodKind::Get, vec![], v)
        }
    };
    let r = tx.ret(Some(ret), at);
    let m = tx.method(mods, mk, key, &params, &[r], at);
    tx.mark_single_line_body(m);
    m
}

/// Descriptor object for a decorated private method/getter/setter (after `class_body`
/// transformed it) and the forwarding member that replaces it.
fn private_method(
    tx: &mut Tx,
    m: NodeId,
    name: Atom,
    desc: Atom,
    kind: Kind,
    at: u32,
) -> (NodeId, NodeId) {
    let Node::MethodDef {
        modifiers,
        key,
        value,
        ..
    } = tx.n(m)
    else {
        let o = tx.object(&[], at);
        return (o, m);
    };
    let fname = format!("#{}", tx.ast.atom(name));
    let f = tx.helper_ref(Helper::SetFunctionName, at);
    let n = tx.string(&fname, at);
    let (prop, call) = match kind {
        Kind::Getter => {
            let k = tx.string("get", at);
            ("get", tx.call(f, &[value, n, k], at))
        }
        Kind::Setter => {
            let k = tx.string("set", at);
            ("set", tx.call(f, &[value, n, k], at))
        }
        _ => ("value", tx.call(f, &[value, n], at)),
    };
    let p = tx.prop(prop, call, at);
    let obj = tx.object(&[p], at);
    let made = forward(
        tx,
        key,
        desc,
        kind,
        modifiers.contains(Modifiers::STATIC),
        at,
    );
    tx.ast.set_span(made, tx.span(m));
    (obj, made)
}

/// An initializer position: a field's value, or the right side of a moved assignment.
#[derive(Clone, Copy)]
enum Slot {
    Field(NodeId),
    Assign(NodeId),
}

fn slot_value(tx: &Tx, s: Slot) -> Option<NodeId> {
    match (
        s,
        tx.n(match s {
            Slot::Field(n) | Slot::Assign(n) => n,
        }),
    ) {
        (Slot::Field(_), Node::PropertyDef { value, .. }) => value,
        (Slot::Assign(_), Node::Assign { right, .. }) => Some(right),
        _ => None,
    }
}

fn set_slot_value(tx: &mut Tx, s: Slot, v: NodeId) {
    match s {
        Slot::Field(n) => {
            if let Node::PropertyDef {
                decorators,
                modifiers,
                key,
                optional,
                definite,
                type_ann,
                ..
            } = tx.n(n)
            {
                tx.set(
                    n,
                    Node::PropertyDef {
                        decorators,
                        modifiers,
                        key,
                        optional,
                        definite,
                        type_ann,
                        value: Some(v),
                    },
                );
            }
        }
        Slot::Assign(n) => {
            if let Node::Assign { left, op, .. } = tx.n(n) {
                tx.set(n, Node::Assign { left, op, right: v });
            }
        }
    }
}

/// `this.x = v` (or `this[k] = v`) synthesized by `class_body`.
fn this_assign(tx: &Tx, stmt: NodeId) -> Option<NodeId> {
    if !is_synth(tx, stmt) {
        return None;
    }
    let Node::ExprStmt { expr } = tx.n(stmt) else {
        return None;
    };
    let Node::Assign { left, .. } = tx.n(expr) else {
        return None;
    };
    match tx.n(left) {
        Node::Member { obj, .. } | Node::Index { obj, .. } if tx.kind(obj) == NodeKind::This => {
            Some(expr)
        }
        _ => None,
    }
}

fn instance_slots(tx: &Tx, members: &[NodeId], ctor: Option<NodeId>, define: bool) -> Vec<Slot> {
    let mut slots = Vec::new();
    for &m in members {
        if let Node::PropertyDef {
            modifiers, value, ..
        } = tx.n(m)
            && !modifiers.contains(Modifiers::STATIC)
            && (define || value.is_some())
        {
            slots.push(Slot::Field(m));
        }
    }
    if !define
        && let Some(c) = ctor
        && let Node::MethodDef { value, .. } = tx.n(c)
        && let Node::FunctionExpr { body: Some(b), .. } = tx.n(value)
        && let Node::Block { stmts } = tx.n(b)
    {
        for s in tx.items(stmts) {
            if let Some(a) = this_assign(tx, s) {
                slots.push(Slot::Assign(a));
            }
        }
    }
    slots
}

fn static_slots(tx: &Tx, members: &[NodeId]) -> Vec<Slot> {
    let mut slots = Vec::new();
    for &m in members {
        match tx.n(m) {
            Node::PropertyDef { modifiers, .. } if modifiers.contains(Modifiers::STATIC) => {
                slots.push(Slot::Field(m));
            }
            // `static { this.x = v; }` from a static field without useDefineForClassFields.
            Node::StaticBlock { body } if !is_synth(tx, m) => {
                let stmts = tx.items(body);
                if stmts.len() == 1
                    && let Some(a) = this_assign(tx, stmts[0])
                {
                    slots.push(Slot::Assign(a));
                }
            }
            _ => {}
        }
    }
    slots
}

/// Prefixes each initializer with the extra initializers still pending
/// (`(__runInitializers(this, _prev_extraInitializers), v)`); returns the ones left over.
fn chain(tx: &mut Tx, st: &Lower, slots: &[Slot], first: Option<Atom>, at: u32) -> Option<Atom> {
    let mut pending = first;
    for &slot in slots {
        let v = slot_value(tx, slot);
        let next = v.and_then(|v| {
            st.decorated
                .iter()
                .find(|d| d.wrapper == Some(v))
                .and_then(|d| d.extra)
        });
        if let Some(e) = pending.take() {
            let this = tx.this(at);
            let f = tx.helper_ref(Helper::RunInitializers, at);
            let r = tx.ident_atom(e, at);
            let run = tx.call(f, &[this, r], at);
            let nv = match v {
                Some(v) => tx.seq(&[run, v], at),
                None => run,
            };
            set_slot_value(tx, slot, nv);
        }
        if next.is_some() {
            pending = next;
        }
    }
    pending
}

fn run_initializers_stmt(tx: &mut Tx, recv: NodeId, list: Atom, at: u32) -> NodeId {
    let f = tx.helper_ref(Helper::RunInitializers, at);
    let r = tx.ident_atom(list, at);
    let call = tx.call(f, &[recv, r], at);
    tx.expr_stmt(call, at)
}

/// Inserts `s` into a constructor after `super(...)` (and, with `after_moved`, after the
/// field initializers `class_body` moved there).
fn append_to_ctor(tx: &mut Tx, ctor: NodeId, s: NodeId, after_moved: bool) {
    let Node::MethodDef { value, .. } = tx.n(ctor) else {
        return;
    };
    let Node::FunctionExpr { body: Some(b), .. } = tx.n(value) else {
        return;
    };
    let Node::Block { stmts } = tx.n(b) else {
        return;
    };
    let mut items = tx.items(stmts);
    let mut pos = items
        .iter()
        .take_while(|&&s| tx.kind(s) == NodeKind::Directive)
        .count();
    if let Some(i) = items.iter().position(|&s| is_super_call(tx, s)) {
        pos = i + 1;
    }
    while after_moved && pos < items.len() && is_synth(tx, items[pos]) {
        pos += 1;
    }
    items.insert(pos, s);
    let l = tx.new_list(&items);
    tx.set(b, Node::Block { stmts: l });
}

fn is_super_call(tx: &Tx, s: NodeId) -> bool {
    let Node::ExprStmt { expr } = tx.n(s) else {
        return false;
    };
    let mut e = expr;
    while let Node::Paren { expr } = tx.n(e) {
        e = expr;
    }
    matches!(tx.n(e), Node::Call { callee, .. } if tx.kind(callee) == NodeKind::Super)
}

fn is_ctor(tx: &Tx, m: NodeId) -> bool {
    matches!(
        tx.n(m),
        Node::MethodDef {
            kind: MethodKind::Constructor,
            ..
        }
    )
}

fn is_synth(tx: &Tx, id: NodeId) -> bool {
    let s = tx.span(id);
    s.start == s.end
}

/// `const _metadata = typeof Symbol === "function" && Symbol.metadata ? Object.create(null) : void 0;`
/// (`Object.create(_classSuper[Symbol.metadata] ?? null)` in a derived class).
fn metadata_decl(tx: &mut Tx, derived: bool, at: u32) -> NodeId {
    let sym = tx.ident("Symbol", at);
    let ty = tx.unary(UnaryOp::TypeOf, sym, at);
    let func = tx.string("function", at);
    let is_fn = tx.binary(ty, BinaryOp::StrictEq, func, at);
    let meta = tx.dotted("Symbol.metadata", at);
    let test = tx.binary(is_fn, BinaryOp::LogicalAnd, meta, at);
    let create = tx.dotted("Object.create", at);
    let mut proto = tx.null(at);
    if derived {
        let sup = tx.ident("_classSuper", at);
        let key = tx.dotted("Symbol.metadata", at);
        let inherited = tx.index(sup, key, at);
        proto = tx.binary(inherited, BinaryOp::Coalesce, proto, at);
    }
    let cons = tx.call(create, &[proto], at);
    let alt = tx.void0(at);
    let cond = tx.mk(Node::Conditional { test, cons, alt }, at);
    let name = tx.ident("_metadata", at);
    tx.var(VarKind::Const, Modifiers::NONE, name, Some(cond), at)
}

/// `if (_metadata) Object.defineProperty(target, Symbol.metadata, { ..., value: _metadata });`
fn define_metadata(tx: &mut Tx, target: NodeId, at: u32) -> NodeId {
    let dp = tx.dotted("Object.defineProperty", at);
    let key = tx.dotted("Symbol.metadata", at);
    let mut props = Vec::with_capacity(4);
    for k in ["enumerable", "configurable", "writable"] {
        let t = tx.bool_lit(true, at);
        props.push(tx.prop(k, t, at));
    }
    let m = tx.ident("_metadata", at);
    props.push(tx.prop("value", m, at));
    let obj = tx.object(&props, at);
    let call = tx.call(dp, &[target, key, obj], at);
    let cons = tx.expr_stmt(call, at);
    tx.mark_single_line(cons);
    let test = tx.ident("_metadata", at);
    tx.mk(
        Node::If {
            test,
            cons,
            alt: None,
        },
        at,
    )
}

/// `__esDecorate(this, null, _x_decorators, { kind, name, static, private, access, metadata }, ...)`
fn es_decorate(tx: &mut Tx, d: &Decorated, method_extra: Option<Atom>) -> NodeId {
    let at = d.at;
    let f = tx.helper_ref(Helper::EsDecorate, at);
    let ctor = if d.kind.is_field() {
        tx.null(at)
    } else {
        tx.this(at)
    };
    let descriptor = match (d.descriptor, d.descriptor_value) {
        (Some(name), Some(v)) => {
            let t = tx.ident_atom(name, at);
            tx.assign(t, v, at)
        }
        _ => tx.null(at),
    };
    let decorators = tx.ident_atom(d.decorators, at);
    // context
    let mut props = Vec::with_capacity(6);
    let kind = tx.string(d.kind.as_str(), at);
    props.push(tx.prop("kind", kind, at));
    let name = match d.name {
        Name::Ident(a) | Name::Literal(a, _) => {
            let s = tx.ast.atom(a).to_owned();
            tx.string(&s, at)
        }
        Name::Private(a) => {
            let s = format!("#{}", tx.ast.atom(a));
            tx.string(&s, at)
        }
        Name::Temp(t) => tx.ident_atom(t, at),
    };
    props.push(tx.prop("name", name, at));
    let s = tx.bool_lit(d.is_static, at);
    props.push(tx.prop("static", s, at));
    let p = tx.bool_lit(d.is_private(), at);
    props.push(tx.prop("private", p, at));
    let access = access_object(tx, d, at);
    props.push(tx.prop("access", access, at));
    let m = tx.ident("_metadata", at);
    props.push(tx.prop("metadata", m, at));
    let context = tx.object(&props, at);
    let (inits, extra) = match (d.initializers, d.extra) {
        (Some(i), Some(e)) => (tx.ident_atom(i, at), tx.ident_atom(e, at)),
        _ => {
            let n = tx.null(at);
            let e = match method_extra {
                Some(e) => tx.ident_atom(e, at),
                None => tx.null(at),
            };
            (n, e)
        }
    };
    tx.call(
        f,
        &[ctor, descriptor, decorators, context, inits, extra],
        at,
    )
}

/// `{ has: obj => "x" in obj, get: obj => obj.x, set: (obj, value) => { obj.x = value; } }`
fn access_object(tx: &mut Tx, d: &Decorated, at: u32) -> NodeId {
    let mut props = Vec::with_capacity(3);
    // has
    let key = match d.name {
        Name::Ident(a) | Name::Literal(a, _) => {
            let s = tx.ast.atom(a).to_owned();
            tx.string(&s, at)
        }
        Name::Private(a) => tx.mk(Node::PrivateName { name: a }, at),
        Name::Temp(t) => tx.ident_atom(t, at),
    };
    let obj = tx.ident("obj", at);
    let has = tx.binary(key, BinaryOp::In, obj, at);
    let po = tx.ident("obj", at);
    let param = tx.param(po, at);
    let has = tx.arrow_expr(&[param], has, at);
    props.push(tx.prop("has", has, at));
    if d.kind != Kind::Setter {
        let obj = tx.ident("obj", at);
        let read = access(tx, d.name, obj, at);
        let po = tx.ident("obj", at);
        let param = tx.param(po, at);
        let get = tx.arrow_expr(&[param], read, at);
        props.push(tx.prop("get", get, at));
    }
    if matches!(d.kind, Kind::Field | Kind::Accessor | Kind::Setter) {
        let obj = tx.ident("obj", at);
        let target = access(tx, d.name, obj, at);
        let v = tx.ident("value", at);
        let a = tx.assign(target, v, at);
        let s = tx.expr_stmt(a, at);
        let body = tx.block(&[s], at);
        tx.mark_single_line(body);
        let po = tx.ident("obj", at);
        let p1 = tx.param(po, at);
        let pv = tx.ident("value", at);
        let p2 = tx.param(pv, at);
        let params = tx.new_list(&[p1, p2]);
        let set = tx.mk(
            Node::Arrow {
                flags: FnFlags::NONE,
                type_params: None,
                params,
                return_type: None,
                body,
            },
            at,
        );
        props.push(tx.prop("set", set, at));
    }
    tx.object(&props, at)
}

/// `obj.x`, `obj.#x`, `obj["x"]` or `obj[_a]`.
fn access(tx: &mut Tx, name: Name, obj: NodeId, at: u32) -> NodeId {
    match name {
        Name::Ident(a) => {
            let s = tx.ast.atom(a).to_owned();
            tx.member(obj, &s, at)
        }
        Name::Private(a) => {
            let p = tx.mk(Node::PrivateName { name: a }, at);
            tx.mk(
                Node::Member {
                    obj,
                    optional: false,
                    prop: p,
                },
                at,
            )
        }
        Name::Literal(_, lit) => {
            let k = deep_clone(tx.ast, lit);
            tx.index(obj, k, at)
        }
        Name::Temp(t) => {
            let k = tx.ident_atom(t, at);
            tx.index(obj, k, at)
        }
    }
}

/// `__esDecorate(null, _classDescriptor = { value: _classThis }, _classDecorators, { kind:
/// "class", name: _classThis.name, metadata: _metadata }, null, _classExtraInitializers);`
/// and `C = _classThis = _classDescriptor.value;`
fn class_decorate(tx: &mut Tx, binding: Atom, at: u32) -> [NodeId; 2] {
    let f = tx.helper_ref(Helper::EsDecorate, at);
    let null = tx.null(at);
    let ct = tx.ident("_classThis", at);
    let vp = tx.prop("value", ct, at);
    let obj = tx.object(&[vp], at);
    let desc = tx.ident("_classDescriptor", at);
    let desc = tx.assign(desc, obj, at);
    let decs = tx.ident("_classDecorators", at);
    let kind = tx.string("class", at);
    let kind = tx.prop("kind", kind, at);
    let name = tx.dotted("_classThis.name", at);
    let name = tx.prop("name", name, at);
    let m = tx.ident("_metadata", at);
    let meta = tx.prop("metadata", m, at);
    let ctx = tx.object(&[kind, name, meta], at);
    let null2 = tx.null(at);
    let extra = tx.ident("_classExtraInitializers", at);
    let call = tx.call(f, &[null, desc, decs, ctx, null2, extra], at);
    let s1 = tx.expr_stmt(call, at);
    let value = tx.dotted("_classDescriptor.value", at);
    let ct = tx.ident("_classThis", at);
    let inner = tx.assign(ct, value, at);
    let c = tx.ident_atom(binding, at);
    let outer = tx.assign(c, inner, at);
    let s2 = tx.expr_stmt(outer, at);
    [s1, s2]
}

/// `static { __setFunctionName(target, "name"); }`
fn set_function_name_block(tx: &mut Tx, target: NodeId, name: &str, at: u32) -> NodeId {
    let f = tx.helper_ref(Helper::SetFunctionName, at);
    let n = tx.string(name, at);
    let call = tx.call(f, &[target, n], at);
    let s = tx.expr_stmt(call, at);
    let b = static_block(tx, &[s], at);
    tx.mark_single_line(b);
    tx.inline_static.push(b);
    b
}

fn static_block(tx: &mut Tx, stmts: &[NodeId], at: u32) -> NodeId {
    let body = tx.new_list(stmts);
    tx.mk(Node::StaticBlock { body }, at)
}

fn let_decl(tx: &mut Tx, name: Atom, init: Option<NodeId>, at: u32) -> NodeId {
    let n = tx.ident_atom(name, at);
    tx.var(VarKind::Let, Modifiers::NONE, n, init, at)
}

fn let_named(tx: &mut Tx, name: &str, init: Option<NodeId>, at: u32) -> NodeId {
    let n = tx.ident(name, at);
    tx.var(VarKind::Let, Modifiers::NONE, n, init, at)
}

/// True if `root` uses `this` outside nested functions and classes.
fn contains_this(tx: &Tx, root: NodeId) -> bool {
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        match tx.n(n) {
            Node::This {} => return true,
            Node::FunctionExpr { .. }
            | Node::FunctionDecl { .. }
            | Node::ClassDecl { .. }
            | Node::ClassExpr { .. }
            | Node::MethodDef { .. } => {}
            _ => tx.ast.for_each_child(n, |c| stack.push(c)),
        }
    }
    false
}

/// True if `e`, under parentheses and type assertions, is a class or function expression
/// without a name or an arrow function.
fn is_anonymous_fn(tx: &Tx, mut e: NodeId) -> bool {
    loop {
        match tx.n(e) {
            Node::Paren { expr }
            | Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::TypeAssertion { expr, .. }
            | Node::NonNull { expr } => e = expr,
            Node::ClassExpr { name, .. } | Node::FunctionExpr { name, .. } => {
                return name.is_none();
            }
            Node::Arrow { .. } => return true,
            _ => return false,
        }
    }
}

/// True if `e` is `super.x` or `super[k]`.
fn is_super_prop(tx: &Tx, e: NodeId) -> bool {
    matches!(tx.n(e), Node::Member { obj, optional: false, .. } | Node::Index { obj, optional: false, .. }
        if tx.kind(obj) == NodeKind::Super)
}

/// In the static fields and blocks of a class-decorated derived class, the decorator may
/// replace the class, so tsc reads `super` through the saved heritage and the final class
/// (observed, tsc 7.0.2): `super.x` is `Reflect.get(_classSuper, "x", _classThis)`,
/// `super.m(a)` is `Reflect.get(..).call(_classThis, a)` and `` super.m`t` `` is
/// `` Reflect.get(..).bind(_classThis)`t` ``. Writes to `super` properties (which tsc lowers
/// through `Reflect.set` and temporaries) are left as written. Nested functions and classes
/// have their own `super` and are skipped; arrows are not.
fn replace_super_reads(tx: &mut Tx, root: NodeId, this: Atom, sup: Atom) {
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        let at = tx.at(n);
        match tx.n(n) {
            Node::Call {
                mut callee,
                optional,
                type_args,
                args,
            } => {
                while let Node::Paren { expr } = tx.n(callee) {
                    callee = expr;
                }
                if !is_super_prop(tx, callee) {
                    tx.ast.for_each_child(n, |c| stack.push(c));
                    continue;
                }
                stack.extend(tx.items(args));
                let get = super_get(tx, callee, this, sup, &mut stack);
                let prop = tx.ident("call", at);
                let f = tx.mk(
                    Node::Member {
                        obj: get,
                        optional,
                        prop,
                    },
                    at,
                );
                let mut list = vec![tx.ident_atom(this, at)];
                list.extend(tx.items(args));
                let args = tx.new_list(&list);
                tx.set(
                    n,
                    Node::Call {
                        callee: f,
                        optional: false,
                        type_args,
                        args,
                    },
                );
            }
            Node::TaggedTemplate {
                tag,
                type_args,
                quasi,
            } if is_super_prop(tx, tag) => {
                stack.push(quasi);
                let get = super_get(tx, tag, this, sup, &mut stack);
                let bind = tx.member(get, "bind", at);
                let r = tx.ident_atom(this, at);
                let bound = tx.call(bind, &[r], at);
                tx.set(
                    n,
                    Node::TaggedTemplate {
                        tag: bound,
                        type_args,
                        quasi,
                    },
                );
            }
            Node::Member { .. } | Node::Index { .. } if is_super_prop(tx, n) => {
                let get = super_get(tx, n, this, sup, &mut stack);
                replace(tx.ast, n, get);
            }
            // Write targets keep `super`; only their computed keys and values are reads.
            Node::Assign { left, right, .. } => {
                stack.push(right);
                push_target(tx, left, &mut stack);
            }
            Node::Update { arg, .. } => push_target(tx, arg, &mut stack),
            Node::ForIn { left, right, body }
            | Node::ForOf {
                left, right, body, ..
            } => {
                stack.push(body);
                stack.push(right);
                push_target(tx, left, &mut stack);
            }
            Node::FunctionExpr { .. }
            | Node::FunctionDecl { .. }
            | Node::ClassDecl { .. }
            | Node::ClassExpr { .. }
            | Node::MethodDef { .. } => {}
            _ => tx.ast.for_each_child(n, |c| stack.push(c)),
        }
    }
}

/// `Reflect.get(sup, key, this)` for `super.x` / `super[k]`; a computed key is pushed to be
/// visited.
fn super_get(tx: &mut Tx, prop: NodeId, this: Atom, sup: Atom, stack: &mut Vec<NodeId>) -> NodeId {
    let at = tx.at(prop);
    let key = match tx.n(prop) {
        Node::Index { index, .. } => {
            stack.push(index);
            index
        }
        Node::Member { prop, .. } => {
            let text = tx
                .atom_of(prop)
                .map(|a| tx.ast.atom(a).to_owned())
                .unwrap_or_default();
            tx.string(&text, at)
        }
        _ => tx.string("", at),
    };
    let f = tx.dotted("Reflect.get", at);
    let h = tx.ident_atom(sup, at);
    let r = tx.ident_atom(this, at);
    tx.call(f, &[h, key, r], at)
}

/// Visits the reads inside a write target: the keys of `super` properties, the objects of
/// other members and the defaults and computed keys of patterns, not `super.x` itself.
fn push_target(tx: &Tx, target: NodeId, stack: &mut Vec<NodeId>) {
    match tx.n(target) {
        Node::Member { obj, .. } if tx.kind(obj) == NodeKind::Super => {}
        Node::Index { obj, index, .. } if tx.kind(obj) == NodeKind::Super => stack.push(index),
        Node::ObjectLit { .. } | Node::ArrayLit { .. } | Node::Paren { .. } => {
            tx.ast.for_each_child(target, |c| {
                if is_super_prop(tx, c) {
                    push_target(tx, c, stack);
                } else {
                    // Nested patterns, defaults (`a = d`) and `key: target` properties.
                    push_target_or_read(tx, c, stack);
                }
            });
        }
        _ => stack.push(target),
    }
}

fn push_target_or_read(tx: &Tx, n: NodeId, stack: &mut Vec<NodeId>) {
    match tx.n(n) {
        Node::Property { key, value, .. } => {
            if tx.kind(key) == NodeKind::ComputedKey {
                stack.push(key);
            }
            push_target(tx, value, stack);
        }
        Node::Spread { arg } => push_target(tx, arg, stack),
        Node::Assign { left, right, .. } => {
            stack.push(right);
            push_target(tx, left, stack);
        }
        _ => push_target(tx, n, stack),
    }
}

/// Replaces `this` (outside nested functions and classes) with `name`.
fn replace_this(tx: &mut Tx, root: NodeId, name: Atom) {
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        match tx.n(n) {
            Node::This {} => {
                let at = tx.at(n);
                let id = tx.ident_atom(name, at);
                replace(tx.ast, n, id);
            }
            Node::FunctionExpr { .. }
            | Node::FunctionDecl { .. }
            | Node::ClassDecl { .. }
            | Node::ClassExpr { .. }
            | Node::MethodDef { .. } => {}
            _ => tx.ast.for_each_child(n, |c| stack.push(c)),
        }
    }
}
