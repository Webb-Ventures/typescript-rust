//! Classes: member type erasure, parameter properties, field semantics
//! (`useDefineForClassFields`), `accessor` lowering, and dispatch to the decorator transforms.
//!
//! tsc 7 shapes (target ES2022+):
//! - Parameter properties become field declarations at the top of the class body (with
//!   `useDefineForClassFields`) and `this.x = x;` right after the top-level `super(...)` call
//!   (or at the start of the constructor body).
//! - Without `useDefineForClassFields`, instance field initializers move into the constructor
//!   (after the parameter properties), uninitialized fields disappear, private fields keep a
//!   bare declaration, and static initializers become `static { this.x = v; }` in place.
//! - `accessor` fields (below ESNext) become a `#x_accessor_storage` field plus a getter/setter.

use ts_r_ast::{HeritageKind, MethodKind, Modifiers, Node, NodeId, NodeKind, NodeList};

use crate::builder::Build;
use crate::transform::Tx;

pub(crate) fn class_decl(tx: &mut Tx, id: NodeId, out: &mut Vec<NodeId>) {
    let Node::ClassDecl {
        modifiers, name, ..
    } = tx.n(id)
    else {
        out.push(id);
        return;
    };
    let decorated = has_decorators(tx, id);
    if decorated && tx.opts.experimental_decorators {
        let info = crate::decorators::collect(tx, id);
        class_body(tx, id);
        out.append(&mut tx.class_pre_stmts);
        crate::decorators::emit_decl(tx, id, info, out);
        return;
    }
    if decorated && tx.opts.lower_es_decorators() {
        crate::es_decorators::class_decl(tx, id, out);
        return;
    }
    class_body(tx, id);
    out.append(&mut tx.class_pre_stmts);
    export_decl(tx, id, modifiers, name, out);
}

/// Pushes a (transformed) class or function declaration, handling namespace and CommonJS
/// exports.
pub(crate) fn export_decl(
    tx: &mut Tx,
    id: NodeId,
    modifiers: Modifiers,
    name: Option<NodeId>,
    out: &mut Vec<NodeId>,
) {
    if !tx.ns_stack.is_empty() && modifiers.contains(Modifiers::EXPORT) {
        crate::namespace::exported_decl(tx, id, name, out);
    } else if tx.opts.is_cjs() && tx.list_depth == 0 {
        crate::cjs::exported_function(tx, id, out);
    } else {
        out.push(id);
    }
}

pub(crate) fn class_expr(tx: &mut Tx, id: NodeId) {
    let name_hint = tx.class_name_hint.take();
    let hint_var = std::mem::take(&mut tx.class_name_hint_var);
    let decorated = has_decorators(tx, id);
    if decorated && tx.opts.lower_es_decorators() {
        crate::es_decorators::class_expr(tx, id, name_hint, hint_var);
        return;
    }
    class_body(tx, id);
    if decorated && tx.opts.experimental_decorators {
        // tsc reports decorators on class expressions (TS1206) and drops them.
        clear_decorators(tx, id);
    }
}

/// True if the class or any member or constructor parameter has decorators.
pub(crate) fn has_decorators(tx: &Tx, id: NodeId) -> bool {
    let (decorators, members) = match tx.n(id) {
        Node::ClassDecl {
            decorators,
            members,
            ..
        }
        | Node::ClassExpr {
            decorators,
            members,
            ..
        } => (decorators, members),
        _ => return false,
    };
    if !decorators.is_empty() {
        return true;
    }
    tx.ast.list(members).iter().any(|&m| match tx.n(m) {
        Node::PropertyDef { decorators, .. } => !decorators.is_empty(),
        Node::MethodDef {
            decorators,
            kind,
            value,
            ..
        } => {
            // Parameter decorators of methods count under `experimentalDecorators` (a class
            // whose only decorators are on method parameters is still decorated).
            !decorators.is_empty()
                || ((kind == MethodKind::Constructor || tx.opts.experimental_decorators)
                    && matches!(tx.n(value), Node::FunctionExpr { params, .. }
                        if tx.ast.list(params).iter().any(|&p| matches!(tx.n(p),
                            Node::Param { decorators, .. } if !decorators.is_empty()))))
        }
        _ => false,
    })
}

/// Removes every decorator list in the class (after they were transformed into calls).
pub(crate) fn clear_decorators(tx: &mut Tx, id: NodeId) {
    let members = match tx.n(id) {
        Node::ClassDecl {
            decorators,
            modifiers,
            name,
            type_params,
            heritage,
            members,
        } => {
            tx.set(
                id,
                Node::ClassDecl {
                    decorators: NodeList::EMPTY,
                    modifiers,
                    name,
                    type_params,
                    heritage,
                    members,
                },
            );
            let _ = decorators;
            members
        }
        Node::ClassExpr {
            modifiers,
            name,
            type_params,
            heritage,
            members,
            ..
        } => {
            tx.set(
                id,
                Node::ClassExpr {
                    decorators: NodeList::EMPTY,
                    modifiers,
                    name,
                    type_params,
                    heritage,
                    members,
                },
            );
            members
        }
        _ => return,
    };
    for m in tx.items(members) {
        match tx.n(m) {
            Node::PropertyDef {
                modifiers,
                key,
                optional,
                definite,
                type_ann,
                value,
                ..
            } => tx.set(
                m,
                Node::PropertyDef {
                    decorators: NodeList::EMPTY,
                    modifiers,
                    key,
                    optional,
                    definite,
                    type_ann,
                    value,
                },
            ),
            Node::MethodDef {
                modifiers,
                kind,
                key,
                optional,
                value,
                ..
            } => {
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
                if let Node::FunctionExpr { params, .. } = tx.n(value) {
                    for p in tx.items(params) {
                        if let Node::Param {
                            modifiers,
                            rest,
                            pattern,
                            optional,
                            type_ann,
                            init,
                            ..
                        } = tx.n(p)
                        {
                            tx.set(
                                p,
                                Node::Param {
                                    decorators: NodeList::EMPTY,
                                    modifiers,
                                    rest,
                                    pattern,
                                    optional,
                                    type_ann,
                                    init,
                                },
                            );
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// Fields of a class node.
#[derive(Clone, Copy)]
pub(crate) struct ClassParts {
    pub decorators: NodeList,
    pub modifiers: Modifiers,
    pub name: Option<NodeId>,
    pub heritage: NodeList,
    pub members: NodeList,
    pub is_expr: bool,
}

pub(crate) fn parts(tx: &Tx, id: NodeId) -> Option<ClassParts> {
    match tx.n(id) {
        Node::ClassDecl {
            decorators,
            modifiers,
            name,
            heritage,
            members,
            ..
        } => Some(ClassParts {
            decorators,
            modifiers,
            name,
            heritage,
            members,
            is_expr: false,
        }),
        Node::ClassExpr {
            decorators,
            modifiers,
            name,
            heritage,
            members,
            ..
        } => Some(ClassParts {
            decorators,
            modifiers,
            name,
            heritage,
            members,
            is_expr: true,
        }),
        _ => None,
    }
}

pub(crate) fn set_parts(tx: &mut Tx, id: NodeId, p: ClassParts) {
    let node = if p.is_expr {
        Node::ClassExpr {
            decorators: p.decorators,
            modifiers: p.modifiers,
            name: p.name,
            type_params: None,
            heritage: p.heritage,
            members: p.members,
        }
    } else {
        Node::ClassDecl {
            decorators: p.decorators,
            modifiers: p.modifiers,
            name: p.name,
            type_params: None,
            heritage: p.heritage,
            members: p.members,
        }
    };
    tx.set(id, node);
}

/// Strips types and lowers fields of a class in place. Decorator lists are kept (their
/// expressions transformed) for the decorator transforms.
pub(crate) fn class_body(tx: &mut Tx, id: NodeId) {
    // Generated storage names for computed `accessor` keys are numbered per class.
    let saved = std::mem::take(&mut tx.class_storage_count);
    let saved_capture = tx.class_key_capture.take();
    class_body_inner(tx, id);
    tx.class_storage_count = saved;
    let capture = std::mem::replace(&mut tx.class_key_capture, saved_capture);
    if let Some((c, capture)) = capture
        && c == id
    {
        if tx.kind(id) == NodeKind::ClassExpr {
            // `(_c = () => { ... }, class { ... })`
            let at = tx.at(id);
            let class = crate::builder::clone_node(tx.ast, id);
            let seq = tx.seq(&[capture, class], at);
            let paren = tx.paren(seq, at);
            crate::builder::replace(tx.ast, id, paren);
        } else {
            // `_c = () => { ... };` before the declaration (`class_decl`).
            let at = tx.at(id);
            let s = tx.expr_stmt(capture, at);
            tx.class_pre_stmts.push(s);
        }
    }
}

/// `v` of a synthesized `__runInitializers(this, _x_initializers, v)`.
fn run_initializers_value(tx: &Tx, e: NodeId) -> Option<NodeId> {
    let Node::Call { callee, args, .. } = tx.n(e) else {
        return None;
    };
    let name = match tx.n(callee) {
        Node::Ident { name } => name,
        Node::Member { prop, .. } => tx.atom_of(prop)?,
        _ => return None,
    };
    let args = tx.ast.list(args);
    (tx.ast.atom(name) == "__runInitializers"
        && args.len() == 3
        && tx.kind(args[0]) == NodeKind::This)
        .then(|| args[2])
}

/// True if `root` uses `this` or `super` outside nested functions and classes.
fn uses_this_or_super(tx: &Tx, root: NodeId) -> bool {
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        match tx.n(n) {
            Node::This {} | Node::Super {} => return true,
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

fn class_body_inner(tx: &mut Tx, id: NodeId) {
    let (type_params, mut p) = match tx.n(id) {
        Node::ClassDecl { type_params, .. } | Node::ClassExpr { type_params, .. } => {
            (type_params, parts(tx, id).expect("class"))
        }
        _ => return,
    };
    tx.erase_opt(type_params);
    for d in tx.items(p.decorators) {
        if let Node::Decorator { expr } = tx.n(d) {
            tx.expr(expr);
        }
    }
    // Heritage: drop `implements`, strip type arguments from `extends`.
    let mut heritage = Vec::new();
    let mut derived = false;
    for h in tx.items(p.heritage) {
        if let Node::HeritageClause { kind, types } = tx.n(h) {
            if kind == HeritageKind::Implements {
                tx.erase(h);
                continue;
            }
            derived = true;
            for t in tx.items(types) {
                if let Node::ExprWithTypeArgs { expr, type_args } = tx.n(t) {
                    tx.erase_opt(type_args);
                    tx.expr(expr);
                    tx.set(
                        t,
                        Node::ExprWithTypeArgs {
                            expr,
                            type_args: None,
                        },
                    );
                }
            }
            heritage.push(h);
        }
    }
    if heritage.len() != tx.ast.list(p.heritage).len() {
        p.heritage = tx.new_list(&heritage);
    }
    let mut keep = Modifiers::NONE;
    for f in [Modifiers::EXPORT, Modifiers::DEFAULT] {
        if p.modifiers.contains(f) {
            keep.insert(f);
        }
    }
    p.modifiers = keep;
    // With TC39 class decorators the class is `_classThis` in its static accessors
    // (observed).
    let class_name = if !p.decorators.is_empty() && !tx.opts.experimental_decorators {
        Some("_classThis".to_owned())
    } else {
        p.name.map(|n| tx.name(n).to_owned())
    };
    let define = tx.opts.use_define_for_class_fields;
    let lower_accessors = !tx.opts.native_decorators();

    // Constructor and its parameter properties (read before the params are stripped).
    let members = tx.items(p.members);
    let ctor = members.iter().copied().find(|&m| {
        matches!(tx.n(m), Node::MethodDef { kind: MethodKind::Constructor, value, .. }
            if matches!(tx.n(value), Node::FunctionExpr { body: Some(_), .. }))
    });
    let mut param_props: Vec<(String, u32, ts_r_span::Span, u32)> = Vec::new();
    if let Some(c) = ctor
        && let Node::MethodDef { value, .. } = tx.n(c)
        && let Node::FunctionExpr { params, .. } = tx.n(value)
    {
        for prm in tx.items(params) {
            if let Node::Param {
                modifiers, pattern, ..
            } = tx.n(prm)
                && modifiers.intersects(
                    Modifiers::PUBLIC
                        | Modifiers::PRIVATE
                        | Modifiers::PROTECTED
                        | Modifiers::READONLY
                        | Modifiers::OVERRIDE,
                )
            {
                if tx.kind(pattern) == NodeKind::Ident {
                    param_props.push((
                        tx.name(pattern).to_owned(),
                        tx.at(prm),
                        tx.span(pattern),
                        tx.span(prm).end,
                    ));
                } else if let Node::FunctionExpr { body: Some(b), .. } = tx.n(value) {
                    // A binding pattern with a modifier (an error) assigns nothing, but tsc
                    // still writes the body over several lines.
                    tx.mark_multi_line(b);
                }
            }
        }
    }

    // Without useDefineForClassFields, public instance initializers move into the
    // constructor; when any does (or there are parameter properties), every instance
    // initializer moves, private ones and `accessor` storage included, to keep their order.
    let moves = !define
        && (!param_props.is_empty()
            || members.iter().any(|&m| {
                matches!(tx.n(m), Node::PropertyDef { modifiers, key, value: Some(_), .. }
                    if !modifiers.intersects(Modifiers::STATIC | Modifiers::DECLARE | Modifiers::ABSTRACT | Modifiers::ACCESSOR)
                        && tx.kind(key) != NodeKind::PrivateName)
            }));
    let mut out: Vec<NodeId> = Vec::with_capacity(members.len() + param_props.len());
    // Initializers moved to the constructor (without useDefineForClassFields).
    let mut ctor_inits: Vec<NodeId> = Vec::new();
    // `this.x = v` of static fields moved into static blocks, with the key's end.
    let mut static_assigns: Vec<(NodeId, u32)> = Vec::new();
    // Hoisted computed keys (`_a = key`) and the keys of removed fields, in member order:
    // one `static { o.k, _a = key; }` block (observed).
    let mut key_blocks: Vec<NodeId> = Vec::new();
    for m in members {
        match tx.n(m) {
            Node::IndexSignature { .. } => tx.erase(m),
            Node::MethodDef {
                decorators,
                modifiers,
                kind,
                key,
                value,
                ..
            } => {
                // An abstract member with a body, or an accessor without one (errors), is
                // kept: `get x() { }` (observed).
                let abstract_ = modifiers.contains(Modifiers::ABSTRACT);
                if !abstract_ {
                    tx.accessor_body(kind, value);
                }
                let body = matches!(tx.n(value), Node::FunctionExpr { body: Some(_), .. });
                if !body {
                    tx.erase(m);
                    continue;
                }
                member_key(tx, key);
                for d in tx.items(decorators) {
                    if let Node::Decorator { expr } = tx.n(d) {
                        tx.expr(expr);
                    }
                }
                // Parameter decorators.
                if let Node::FunctionExpr { params, .. } = tx.n(value) {
                    for prm in tx.items(params) {
                        if let Node::Param { decorators, .. } = tx.n(prm) {
                            for d in tx.items(decorators) {
                                if let Node::Decorator { expr } = tx.n(d) {
                                    tx.expr(expr);
                                }
                            }
                        }
                    }
                }
                tx.function(value);
                let mut mm = Modifiers::NONE;
                if modifiers.contains(Modifiers::STATIC) {
                    mm.insert(Modifiers::STATIC);
                }
                tx.set(
                    m,
                    Node::MethodDef {
                        decorators,
                        modifiers: mm,
                        kind,
                        key,
                        optional: false,
                        value,
                    },
                );
                out.push(m);
            }
            Node::PropertyDef {
                decorators,
                modifiers,
                key,
                type_ann,
                value,
                ..
            } => {
                if modifiers.intersects(Modifiers::DECLARE | Modifiers::ABSTRACT) {
                    tx.erase(m);
                    continue;
                }
                tx.erase_opt(type_ann);
                // A decorated field's value is already `__runInitializers(this, _x, v)`
                // (es_decorators.rs): `v` is still named after the key.
                let hint = value
                    .map(|v| run_initializers_value(tx, v).unwrap_or(v))
                    .and_then(|v| tx.key_class_hint(key, v, false));
                member_key(tx, key);
                for d in tx.items(decorators) {
                    if let Node::Decorator { expr } = tx.n(d) {
                        tx.expr(expr);
                    }
                }
                if let Some(v) = value {
                    tx.key_named(key, v, hint);
                }
                let is_static = modifiers.contains(Modifiers::STATIC);
                let mut mm = Modifiers::NONE;
                if is_static {
                    mm.insert(Modifiers::STATIC);
                }
                if modifiers.contains(Modifiers::ACCESSOR) {
                    if lower_accessors || (moves && !is_static) {
                        let at = tx.at(m);
                        let key_temp = match tx.n(key) {
                            Node::ComputedKey { expr } if !is_literal(tx, expr) => Some(tx.temp()),
                            _ => None,
                        };
                        let storage = accessor_storage(tx, key, key_temp);
                        let skey = tx.mk(Node::PrivateName { name: storage }, at);
                        let (field_value, init) = if !moves || is_static {
                            (value, None)
                        } else {
                            (None, value)
                        };
                        let field = tx.mk(
                            Node::PropertyDef {
                                decorators: NodeList::EMPTY,
                                modifiers: mm,
                                key: skey,
                                optional: false,
                                definite: false,
                                type_ann: None,
                                value: field_value,
                            },
                            at,
                        );
                        tx.ast.set_span(field, tx.span(m));
                        out.push(field);
                        if let Some(v) = init {
                            let this = tx.this(at);
                            let k = tx.mk(Node::PrivateName { name: storage }, at);
                            let target = tx.mk(
                                Node::Member {
                                    obj: this,
                                    optional: false,
                                    prop: k,
                                },
                                at,
                            );
                            let a = tx.assign(target, v, at);
                            let s = tx.expr_stmt(a, at);
                            ctor_inits.push(s);
                        }
                        let (getter, setter) = accessor_pair(
                            tx,
                            key,
                            key_temp,
                            storage,
                            is_static,
                            class_name.as_deref(),
                            at,
                        );
                        // Decorators stay on the getter (legacy decorators read them there).
                        if !decorators.is_empty()
                            && let Node::MethodDef {
                                modifiers,
                                kind,
                                key,
                                optional,
                                value,
                                ..
                            } = tx.n(getter)
                        {
                            tx.set(
                                getter,
                                Node::MethodDef {
                                    decorators,
                                    modifiers,
                                    kind,
                                    key,
                                    optional,
                                    value,
                                },
                            );
                        }
                        out.push(getter);
                        out.push(setter);
                        continue;
                    }
                    mm.insert(Modifiers::ACCESSOR);
                }
                if !define && !modifiers.contains(Modifiers::ACCESSOR) {
                    let private = tx.kind(key) == NodeKind::PrivateName;
                    if is_static {
                        if private {
                            set_field(tx, m, decorators, mm, key, value);
                            out.push(m);
                        } else if let Some(v) = value {
                            let at = tx.at(m);
                            let this = tx.this(at);
                            let target = field_target(tx, this, key, &mut key_blocks);
                            let a = tx.assign(target, v, at);
                            let s = tx.expr_stmt(a, at);
                            // Source maps (tsc's): the statement ends at the field, the
                            // block maps nothing; the assignment ends at the key when the
                            // class moves instance initializers (set after the loop).
                            let none = ts_r_codegen::NO_MAP;
                            let (key_end, field_end) = (tx.span(key).end, tx.span(m).end);
                            static_assigns.push((a, key_end));
                            tx.map_range(s, ts_r_span::Span::new(none, field_end));
                            let body = tx.new_list(&[s]);
                            let block = tx.mk(Node::StaticBlock { body }, at);
                            tx.ast.set_span(block, tx.span(m));
                            tx.map_range(block, ts_r_span::Span::new(none, none));
                            // `static { this.x = [\n...\n]; }` on one line (observed).
                            tx.mark_single_line(block);
                            tx.inline_static.push(block);
                            out.push(block);
                        } else {
                            removed_key(tx, key, &mut key_blocks);
                            tx.erase(m);
                        }
                        continue;
                    }
                    if private && !moves {
                        set_field(tx, m, decorators, mm, key, value);
                        out.push(m);
                        continue;
                    }
                    if private {
                        set_field(tx, m, decorators, mm, key, None);
                        out.push(m);
                    }
                    if let Some(v) = value {
                        let at = tx.at(m);
                        let this = tx.this(at);
                        let target = field_target(tx, this, key, &mut key_blocks);
                        let a = tx.assign(target, v, at);
                        let s = tx.expr_stmt(a, at);
                        // Source maps (tsc's): `this.x = v;` maps to the field; it takes
                        // the field's comments along.
                        let span = tx.span(m);
                        tx.map_range(s, span);
                        tx.moved_comments.push((s, span));
                        ctor_inits.push(s);
                    } else if !private {
                        removed_key(tx, key, &mut key_blocks);
                        tx.erase(m);
                    }
                    continue;
                }
                set_field(tx, m, decorators, mm, key, value);
                out.push(m);
            }
            Node::StaticBlock { body } => {
                tx.list_depth += 1;
                tx.temps.push(Vec::new());
                let l = tx.stmt_list(body);
                let temps = tx.temps.pop().unwrap_or_default();
                tx.list_depth -= 1;
                // Static block temporaries are hoisted to the enclosing scope.
                if let Some(t) = tx.temps.last_mut() {
                    t.extend(temps);
                }
                tx.set(m, Node::StaticBlock { body: l });
                out.push(m);
            }
            _ => out.push(m),
        }
    }

    // Observed in tsc 7's maps: a static field's assignment ends at its key only in a
    // class that also moves instance initializers into the constructor (parameter
    // properties alone do not count).
    if !ctor_inits.is_empty() {
        let none = ts_r_codegen::NO_MAP;
        for (a, key_end) in static_assigns {
            tx.map_range(a, ts_r_span::Span::new(none, key_end));
        }
    }

    // Parameter properties.
    let mut assigns: Vec<NodeId> = Vec::with_capacity(param_props.len() + ctor_inits.len());
    let mut decls: Vec<NodeId> = Vec::new();
    for (name, at, name_span, param_end) in &param_props {
        let this = tx.this(*at);
        let target = tx.member(this, name, *at);
        let v = tx.ident(name, *at);
        // Source maps (tsc's): the value and the field's key map to the parameter's name;
        // when field initializers move into the constructor too (no fields), the statement
        // maps to the parameter (from its name) and `this.p` to the name.
        tx.map_range(v, *name_span);
        let assign_stmt = !define && !ctor_inits.is_empty();
        let a = tx.assign(target, v, *at);
        let s = tx.expr_stmt(a, *at);
        if assign_stmt {
            tx.map_range(target, *name_span);
            tx.map_range(s, ts_r_span::Span::new(name_span.start, *param_end));
        }
        assigns.push(s);
        if define {
            let key = tx.ident(name, *at);
            tx.map_range(key, *name_span);
            let f = tx.mk(
                Node::PropertyDef {
                    decorators: NodeList::EMPTY,
                    modifiers: Modifiers::NONE,
                    key,
                    optional: false,
                    definite: false,
                    type_ann: None,
                    value: None,
                },
                *at,
            );
            decls.push(f);
        }
    }
    assigns.extend(ctor_inits);
    if !decls.is_empty() {
        decls.extend(out);
        out = decls;
    }
    if !assigns.is_empty() {
        match ctor {
            Some(c) => {
                if let Node::MethodDef { value, .. } = tx.n(c)
                    && let Node::FunctionExpr { body: Some(b), .. } = tx.n(value)
                {
                    insert_after_super(tx, b, &assigns, derived);
                }
            }
            None => {
                let at = tx.at(id);
                let mut body = Vec::with_capacity(assigns.len() + 1);
                if derived {
                    let sup = tx.mk(Node::Super {}, at);
                    let args = tx.ident("arguments", at);
                    let spread = tx.spread(args, at);
                    let call = tx.call(sup, &[spread], at);
                    let s = tx.expr_stmt(call, at);
                    body.push(s);
                }
                body.extend(assigns);
                let key = tx.ident("constructor", at);
                let c = tx.method(
                    Modifiers::NONE,
                    MethodKind::Constructor,
                    key,
                    &[],
                    &body,
                    at,
                );
                // Source maps (tsc's): the constructor and its `}` map to the class.
                let span = tx.span(id);
                tx.map_range(c, span);
                if let Node::MethodDef { value, .. } = tx.n(c)
                    && let Node::FunctionExpr { body: Some(b), .. } = tx.n(value)
                {
                    tx.map_range(b, span);
                }
                out.insert(0, c);
            }
        }
    }
    if !key_blocks.is_empty() {
        let pos = usize::from(
            ctor.is_none()
                && out.first().is_some_and(|&m| {
                    matches!(
                        tx.n(m),
                        Node::MethodDef {
                            kind: MethodKind::Constructor,
                            ..
                        }
                    )
                }),
        );
        let at = tx.at(key_blocks[0]);
        let expr = match key_blocks.as_slice() {
            [e] => *e,
            _ => {
                let mut exprs = Vec::with_capacity(key_blocks.len());
                for &e in &key_blocks {
                    match tx.n(e) {
                        Node::Seq { exprs: list } => exprs.extend(tx.items(list)),
                        _ => exprs.push(e),
                    }
                }
                tx.seq(&exprs, at)
            }
        };
        // A lone TC39 decorator key sequence keeps its temporaries' own `var` (observed).
        if key_blocks.len() > 1 || tx.kind(expr) != NodeKind::Seq {
            tx.key_exprs.push(expr);
        }
        // Keys that use `this` or `super` mean the enclosing code's, not the class's:
        // from ES2022 (where the static block stays) they run in an arrow made before the
        // class, `(_c = () => { _a = this.k; }, class { static { _c(); } })` (observed, tsc
        // 7.0.2 `typeOfThisInStaticMembers12`).
        let mut expr = expr;
        if tx.opts.target >= ts_r_config::ScriptTarget::ES2022 && uses_this_or_super(tx, expr) {
            let f = tx.temp();
            let s = tx.expr_stmt(expr, at);
            let body = tx.block(&[s], at);
            let arrow = tx.arrow_expr(&[], body, at);
            tx.mark_single_line(body);
            let t = tx.ident_atom(f, at);
            let capture = tx.assign(t, arrow, at);
            tx.class_key_capture = Some((id, capture));
            let callee = tx.ident_atom(f, at);
            expr = tx.call(callee, &[], at);
        }
        let s = tx.expr_stmt(expr, at);
        let body = tx.new_list(&[s]);
        let block = tx.mk(Node::StaticBlock { body }, at);
        tx.mark_single_line(block);
        tx.inline_static.push(block);
        out.insert(pos, block);
    }
    if out != tx.ast.list(p.members) {
        p.members = tx.new_list(&out);
    }
    set_parts(tx, id, p);
}

fn set_field(
    tx: &mut Tx,
    m: NodeId,
    decorators: NodeList,
    modifiers: Modifiers,
    key: NodeId,
    value: Option<NodeId>,
) {
    tx.set(
        m,
        Node::PropertyDef {
            decorators,
            modifiers,
            key,
            optional: false,
            definite: false,
            type_ann: None,
            value,
        },
    );
}

/// Transforms a computed member key's expression.
fn member_key(tx: &mut Tx, key: NodeId) {
    if let Node::ComputedKey { expr } = tx.n(key) {
        tx.expr(expr);
    }
}

/// A field removed without useDefineForClassFields still evaluates a computed key that is
/// not an identifier, a literal or a keyword (observed).
fn removed_key(tx: &mut Tx, key: NodeId, key_exprs: &mut Vec<NodeId>) {
    if let Node::ComputedKey { expr } = tx.n(key)
        && !matches!(
            tx.kind(expr),
            NodeKind::Ident | NodeKind::BoolLit | NodeKind::NullLit | NodeKind::This
        )
        && !is_literal(tx, expr)
    {
        key_exprs.push(expr);
    }
}

/// `this.x`, `this["x"]`, `this[0]`, `this.#x` or `this[_a]` (hoisting a computed key: its
/// `_a = key` is pushed to `key_exprs`).
fn field_target(tx: &mut Tx, obj: NodeId, key: NodeId, key_exprs: &mut Vec<NodeId>) -> NodeId {
    let at = tx.at(key);
    match tx.n(key) {
        Node::Ident { .. } | Node::PrivateName { .. } => {
            let k = crate::builder::clone_node(tx.ast, key);
            // Source maps (tsc's): `this.x` maps to the name it moved from, `x` not at all.
            let span = tx.span(key);
            tx.ast.set_span(k, ts_r_span::Span::new(at, at));
            let m = tx.mk(
                Node::Member {
                    obj,
                    optional: false,
                    prop: k,
                },
                at,
            );
            tx.map_range(m, span);
            m
        }
        Node::ComputedKey { expr } => {
            let index = if is_literal(tx, expr) {
                expr
            } else if let Some(t) = assigned_temp(tx, expr) {
                // `(..., _a = __propKey(k))` from the TC39 decorator transform: evaluate it in
                // the static block and index with `_a`.
                key_exprs.push(expr);
                tx.ident_atom(t, at)
            } else {
                let t = tx.temp();
                let target = tx.ident_atom(t, at);
                let a = tx.assign(target, expr, at);
                key_exprs.push(a);
                tx.ident_atom(t, at)
            };
            tx.index(obj, index, at)
        }
        _ => {
            let k = crate::builder::clone_node(tx.ast, key);
            // Source maps (tsc's): `this["x"]` maps to the key, the key itself not at all.
            let span = tx.span(key);
            let none = ts_r_span::Span::new(ts_r_codegen::NO_MAP, ts_r_codegen::NO_MAP);
            tx.map_range(k, none);
            let i = tx.index(obj, k, at);
            tx.map_range(i, span);
            i
        }
    }
}

/// `_a` when `expr` is `_a = ...` or a sequence ending in one, with `_a` synthesized.
fn assigned_temp(tx: &Tx, expr: NodeId) -> Option<ts_r_ast::Atom> {
    let last = match tx.n(expr) {
        Node::Seq { exprs } => *tx.ast.list(exprs).last()?,
        _ => expr,
    };
    match tx.n(last) {
        Node::Assign { left, .. } if tx.span(left).start == tx.span(left).end => match tx.n(left) {
            Node::Ident { name } => Some(name),
            _ => None,
        },
        _ => None,
    }
}

pub(crate) fn is_literal(tx: &Tx, id: NodeId) -> bool {
    matches!(
        tx.kind(id),
        NodeKind::StringLit | NodeKind::NumberLit | NodeKind::BigIntLit
    ) || matches!(tx.n(id), Node::TemplateLit { parts } if tx.ast.list(parts).len() == 1)
}

/// The private storage name of an `accessor` field: `x_accessor_storage` for `x`/`#x`
/// (`x_1_accessor_storage` when the source already has that private name), `_a_...`,
/// `_b_...` (numbered per class from ES2022, across the file below it) otherwise
/// (observed, tsc 7.0.2 `autoAccessor5`, `autoAccessor10`).
fn accessor_storage(tx: &mut Tx, key: NodeId, _key_temp: Option<ts_r_ast::Atom>) -> ts_r_ast::Atom {
    let base = match tx.n(key) {
        Node::Ident { name } | Node::PrivateName { name } => {
            let n = tx.ast.atom(name).to_owned();
            let taken = source_private_names(tx);
            let mut cand = n.clone();
            let mut i = 0;
            while taken.contains(&tx.ast.intern(&format!("{cand}_accessor_storage"))) {
                i += 1;
                cand = format!("{n}_{i}");
            }
            tx.source_private_names = Some(taken);
            cand
        }
        _ if tx.opts.target >= ts_r_config::ScriptTarget::ES2022 => {
            let i = tx.class_storage_count;
            tx.class_storage_count += 1;
            if i < 26 {
                format!("_{}", (b'a' + i as u8) as char)
            } else {
                format!("_{}", i - 26)
            }
        }
        _ => {
            // The storage becomes a `WeakMap` named after it; the generated names are
            // counted across the file and take no temporary (observed).
            let i = tx.storage_count;
            tx.storage_count += 1;
            if i < 26 {
                format!("_{}", (b'a' + i as u8) as char)
            } else {
                format!("_{}", i - 26)
            }
        }
    };
    tx.ast.intern(&format!("{base}_accessor_storage"))
}

/// The private names the parser made (taken out of `tx`; the caller puts it back).
fn source_private_names(tx: &mut Tx) -> rustc_hash::FxHashSet<ts_r_ast::Atom> {
    if let Some(s) = tx.source_private_names.take() {
        return s;
    }
    let mut set = rustc_hash::FxHashSet::default();
    for i in 1..=tx.source_nodes {
        if let Some(id) = NodeId::from_raw(i as u32)
            && let Node::PrivateName { name } = tx.n(id)
        {
            set.insert(name);
        }
    }
    set
}

/// `get key() { return this.#storage; }` and `set key(value) { this.#storage = value; }`.
fn accessor_pair(
    tx: &mut Tx,
    key: NodeId,
    key_temp: Option<ts_r_ast::Atom>,
    storage: ts_r_ast::Atom,
    is_static: bool,
    class_name: Option<&str>,
    at: u32,
) -> (NodeId, NodeId) {
    // A computed key is evaluated once: `get [_a = expr]()`, `set [_a](value)`.
    let (get_key, set_key) = match (tx.n(key), key_temp) {
        (Node::ComputedKey { expr }, Some(t)) => {
            let target = tx.ident_atom(t, at);
            let a = tx.assign(target, expr, at);
            let k1 = tx.mk(Node::ComputedKey { expr: a }, at);
            let r = tx.ident_atom(t, at);
            let k2 = tx.mk(Node::ComputedKey { expr: r }, at);
            (k1, k2)
        }
        (_, _) => {
            let k2 = crate::builder::deep_clone(tx.ast, key);
            (key, k2)
        }
    };
    accessor_methods(tx, get_key, set_key, storage, is_static, class_name, at)
}

/// The getter/setter pair of an `accessor` field with the given keys; static ones read the
/// storage through `class_name` when there is one.
pub(crate) fn accessor_methods(
    tx: &mut Tx,
    get_key: NodeId,
    set_key: NodeId,
    storage: ts_r_ast::Atom,
    is_static: bool,
    class_name: Option<&str>,
    at: u32,
) -> (NodeId, NodeId) {
    let mods = if is_static {
        Modifiers::STATIC
    } else {
        Modifiers::NONE
    };
    let receiver = |tx: &mut Tx| match (is_static, class_name) {
        (true, Some(n)) => tx.ident(n, at),
        _ => tx.this(at),
    };
    let obj = receiver(tx);
    let p = tx.mk(Node::PrivateName { name: storage }, at);
    let read = tx.mk(
        Node::Member {
            obj,
            optional: false,
            prop: p,
        },
        at,
    );
    let ret = tx.ret(Some(read), at);
    let getter = tx.method(mods, MethodKind::Get, get_key, &[], &[ret], at);
    let obj = receiver(tx);
    let p = tx.mk(Node::PrivateName { name: storage }, at);
    let target = tx.mk(
        Node::Member {
            obj,
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
    let setter = tx.method(mods, MethodKind::Set, set_key, &[param], &[s], at);
    tx.mark_single_line_body(getter);
    tx.mark_single_line_body(setter);
    (getter, setter)
}

/// Inserts `stmts` after the top-level `super(...)` call of a constructor body (or after the
/// directive prologue when there is none).
pub(crate) fn insert_after_super(tx: &mut Tx, body: NodeId, stmts: &[NodeId], derived: bool) {
    let Node::Block { stmts: list } = tx.n(body) else {
        return;
    };
    let mut items = tx.items(list);
    let prologue = items
        .iter()
        .take_while(|&&s| tx.kind(s) == NodeKind::Directive)
        .count();
    let mut pos = prologue;
    let _ = derived;
    if let Some(i) = items.iter().position(|&s| is_super_call_stmt(tx, s)) {
        pos = i + 1;
    }
    for (i, &s) in stmts.iter().enumerate() {
        items.insert(pos + i, s);
    }
    let l = tx.new_list(&items);
    tx.set(body, Node::Block { stmts: l });
}

fn is_super_call_stmt(tx: &Tx, s: NodeId) -> bool {
    let Node::ExprStmt { expr } = tx.n(s) else {
        return false;
    };
    let mut e = expr;
    while let Node::Paren { expr } = tx.n(e) {
        e = expr;
    }
    matches!(tx.n(e), Node::Call { callee, .. } if tx.kind(callee) == NodeKind::Super)
}
