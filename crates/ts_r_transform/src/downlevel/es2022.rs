//! ES2022 classes below ES2022: static blocks and static initializers move out of the class
//! (observed, tsc 7.0.2, `useDefineForClassFields` false).
//!
//! The TypeScript transform ([`crate::class`]) has already turned static fields into static
//! blocks (`static { this.x = 1; }`) and computed field keys into key captures
//! (`static { _a = k; }`), recorded in [`crate::transform::Tx::inline_static`]. Here:
//!
//! - A class declaration keeps its other members; after it come `_a = C, _b = k;` (the class
//!   alias, when the static code uses `this`, `super` or the class name, and the key
//!   captures), then each static member in order: `C.x = 1;` for a static property and
//!   `(() => { ... })();` for a source static block.
//! - A class expression becomes `(_a = class { }, __setFunctionName(_a, "x"), _a.x = 1,
//!   (() => { ... })(), _a)`, broken after every comma; it always has an alias.
//! - With an alias, `this` in static code and the class name anywhere in the class body
//!   become the alias; `super.m` in static code becomes `Reflect.get(_b, "m", _a)` with
//!   `class C extends (_b = A)`, and a write to it `Reflect.set(_b, "m", v, _a)` (with
//!   result and old-value temporaries where the value is used, and a `set value(_c)`
//!   setter object as a destructuring target).
//! - `export default class { static ... }` is named `default_1` and exported after; a
//!   named one (`export default class C`) is also exported after its statics.
//!
//! The alias, heritage and key temporaries share one `var` statement (the key captures move
//! there from the TypeScript transform's), and their names are reserved in nested functions.

use rustc_hash::FxHashMap;
use ts_r_ast::{Atom, FnFlags, HeritageKind, Modifiers, Node, NodeId, NodeKind, NodeList};

use super::lower::{Group, Lower};
use super::private::{PrivEnv, PrivKind, PrivMember};
use crate::builder::Build;
use crate::helpers::Helper;

/// A static member moved out of the class.
enum Static {
    /// `this.x = v;` (a static property; the statement, and the member it came from).
    Prop(NodeId, NodeId),
    /// A synthesized expression (`_a = k`).
    Expr(NodeId),
    /// A source static block's statements.
    Block(NodeList, NodeId),
    /// A static field kept as a field (`useDefineForClassFields`): `Object.defineProperty`.
    Define(NodeId),
    /// A static private field: `_C_s = { value: v }`.
    PrivField(NodeId),
}

#[derive(Default)]
struct Usage {
    this: bool,
    sup: bool,
    name: bool,
}

/// What the substitution walk replaces.
#[derive(Clone, Copy)]
struct Subst {
    alias: Atom,
    /// The class name, replaced by the alias.
    name: Option<Atom>,
    /// The heritage temporary `super` reads through.
    heritage: Option<Atom>,
}

impl Lower<'_, '_> {
    /// True if class `id` has static code to move out (and nothing this lowering does not
    /// handle yet: fields kept as fields, private names).
    pub(super) fn lowers_class(&self, id: NodeId) -> bool {
        let Some(p) = crate::class::parts(self.tx, id) else {
            return false;
        };
        if !p.decorators.is_empty() {
            return false;
        }
        let mut any = false;
        for m in self.tx.items(p.members) {
            match self.n(m) {
                Node::StaticBlock { .. } => any = true,
                Node::PropertyDef { modifiers, .. } => {
                    if modifiers.contains(Modifiers::ACCESSOR) {
                        return false;
                    }
                    any = true;
                }
                Node::MethodDef { key, .. } if self.tx.kind(key) == NodeKind::PrivateName => {
                    any = true;
                }
                _ => {}
            }
        }
        any
    }

    /// An anonymous class expression to lower that `id` names (`x = class {}`), and the name.
    pub(super) fn named_class(&self, id: NodeId) -> Option<(NodeId, Atom)> {
        match self.n(id) {
            Node::VarDeclarator {
                name,
                init: Some(init),
                ..
            } => self.class_hint(name, init),
            Node::Assign {
                op: ts_r_ast::AssignOp::Assign,
                left,
                right,
            } => self.class_hint(left, right),
            Node::Property { key, value, .. } => self.class_hint(key, value),
            Node::Param {
                pattern,
                init: Some(init),
                ..
            } => self.class_hint(pattern, init),
            Node::AssignPattern { left, right } => self.class_hint(left, right),
            _ => None,
        }
    }

    /// The name an anonymous class expression gets from where it is (`x = class {}`).
    fn class_hint(&self, target: NodeId, value: NodeId) -> Option<(NodeId, Atom)> {
        let v = self.skip_parens(value);
        if !matches!(self.n(v), Node::ClassExpr { name: None, .. }) || !self.lowers_class(v) {
            return None;
        }
        let name = match self.n(target) {
            Node::Ident { name } => name,
            Node::StringLit { value } => value,
            _ => return None,
        };
        Some((v, name))
    }

    /// Lowers class `id` (see the module docs); `hint` names an anonymous class expression,
    /// `in_var` when it is a variable's initializer.
    pub(super) fn class(&mut self, id: NodeId, hint: Option<Atom>, in_var: bool) {
        let depth = self.privates.len();
        self.class_inner(id, hint, in_var);
        self.privates.truncate(depth);
    }

    pub(super) fn class_inner(&mut self, id: NodeId, hint: Option<Atom>, in_var: bool) {
        let Some(mut p) = crate::class::parts(self.tx, id) else {
            return;
        };
        let at = self.at(id);
        // A class expression standing for a declaration (legacy decorators' `let C = class C
        // {}`): its statics follow the statement, as a declaration's do (observed).
        let is_expr = p.is_expr && !self.tx.decl_like.contains(&id);
        let mut statics = Vec::new();
        let mut kept = Vec::new();
        let mut fields = Vec::new();
        let mut priv_fields = Vec::new();
        let mut priv_methods = Vec::new();
        for m in self.tx.items(p.members) {
            match self.n(m) {
                Node::PropertyDef { key, modifiers, .. }
                    if self.tx.kind(key) == NodeKind::PrivateName =>
                {
                    if modifiers.contains(Modifiers::STATIC) {
                        statics.push(Static::PrivField(m));
                    } else {
                        priv_fields.push(m);
                    }
                }
                Node::MethodDef { key, .. } if self.tx.kind(key) == NodeKind::PrivateName => {
                    priv_methods.push(m)
                }
                Node::StaticBlock { body } if self.tx.inline_static.contains(&m) => {
                    let stmts = self.tx.items(body);
                    match stmts.as_slice() {
                        [s] => match self.n(*s) {
                            Node::ExprStmt { expr } if self.is_this_prop(expr) => {
                                statics.push(Static::Prop(*s, m))
                            }
                            Node::ExprStmt { expr } => statics.push(Static::Expr(expr)),
                            _ => statics.push(Static::Block(body, m)),
                        },
                        _ => statics.push(Static::Block(body, m)),
                    }
                }
                Node::StaticBlock { body } => statics.push(Static::Block(body, m)),
                Node::PropertyDef {
                    modifiers, value, ..
                } if modifiers.contains(Modifiers::STATIC) => {
                    // A static field without an initializer defines nothing, but a computed
                    // key is still captured (observed).
                    if value.is_some() || self.key_captured(m) {
                        statics.push(Static::Define(m));
                    }
                }
                Node::PropertyDef { .. } => fields.push(m),
                _ => kept.push(m),
            }
        }
        // An extracted private method loses its home object; keep it native until its
        // `super` accesses can be rewritten with the original receiver.
        for &m in &priv_methods {
            let Node::MethodDef { value, .. } = self.n(m) else {
                continue;
            };
            let Node::FunctionExpr { params, body, .. } = self.n(value) else {
                continue;
            };
            if self
                .tx
                .items(params)
                .into_iter()
                .chain(body)
                .any(|n| self.extracted_super(n))
            {
                self.children(id);
                return;
            }
        }
        if self.loops > 0
            && (!priv_fields.is_empty()
                || !priv_methods.is_empty()
                || statics.iter().any(|s| matches!(s, Static::PrivField(_))))
        {
            if self.class_evaluation_suspends(id) {
                self.children(id);
            } else {
                self.scoped_private_class(id, hint);
            }
            return;
        }
        // The TC39 decorator transform's `static { _classThis = this; }` first: the class is
        // assigned to `_classThis`, which stands for `this` (`var C = _classThis = class {}`,
        // the static code after the variable statement; observed).
        let class_this = match statics.first() {
            Some(&Static::Expr(e)) if is_expr => match self.n(e) {
                Node::Assign {
                    op: ts_r_ast::AssignOp::Assign,
                    left,
                    right,
                } if self.n(right) == Node::This {} => self.tx.atom_of(left),
                _ => None,
            },
            _ => None,
        };
        if class_this.is_some() {
            statics.remove(0);
        }
        let class_name = p.name.and_then(|n| self.tx.atom_of(n));
        let mut u = Usage::default();
        for s in &statics {
            match *s {
                Static::Prop(st, _) => {
                    if let Node::ExprStmt { expr } = self.n(st)
                        && let Node::Assign { left, right, .. } = self.n(expr)
                    {
                        if let Node::Index { index, .. } = self.n(left) {
                            self.usage(index, class_name, true, &mut u);
                        }
                        self.usage(right, class_name, true, &mut u);
                    }
                }
                // A key capture is evaluated outside the class: its `this` and `super` are
                // the outer ones.
                Static::Expr(e) => {
                    let capture = self.is_temp_capture(e);
                    self.usage(e, class_name, !capture, &mut u)
                }
                Static::Block(body, _) => {
                    for st in self.tx.items(body) {
                        self.usage(st, class_name, true, &mut u);
                    }
                }
                Static::Define(m) | Static::PrivField(m) => {
                    if let Node::PropertyDef { value: Some(v), .. } = self.n(m) {
                        self.usage(v, class_name, true, &mut u);
                    }
                }
            }
        }
        // Computed field keys are captured in temporaries (`_a = k`).
        let caps_needed = statics
            .iter()
            .any(|s| matches!(*s, Static::Define(m) if self.key_captured(m)))
            || fields.iter().any(|&m| self.key_captured(m));
        let extends = self.extends_expr(p.heritage);
        // `super` in static code of a class without `extends`: tsc moves it out as it is
        // (`_a = Y; Y.d = super.foo;`, observed), which is a SyntaxError. The class stays as
        // written instead, so the output runs (esbuild's StaticClassBlockES2021).
        if u.sup && extends.is_none() {
            self.children(id);
            return;
        }

        // Private members: `_C_instances` first, the alias when static code needs it, then
        // the storage in member order; a class expression's alias otherwise after them.
        let static_privs = statics.iter().any(|s| matches!(s, Static::PrivField(_)))
            || priv_methods
                .iter()
                .any(|&m| self.n(m).modifiers().contains(Modifiers::STATIC));
        let has_privs = !priv_fields.is_empty() || !priv_methods.is_empty() || static_privs;
        let base_name = class_name.or(hint).map(|a| self.tx.ast.atom(a).to_owned());
        let instances = priv_methods
            .iter()
            .any(|&m| !self.n(m).modifiers().contains(Modifiers::STATIC))
            .then(|| self.priv_storage(base_name.as_deref(), "instances"));
        let mut alias = match class_this {
            Some(ct) => Some(ct),
            None => (u.this || u.sup || static_privs).then(|| self.class_temp()),
        };
        let mut env = PrivEnv {
            instances,
            ..PrivEnv::default()
        };
        let mut storage_of: FxHashMap<NodeId, Atom> = FxHashMap::default();
        if priv_fields.iter().any(|&m| self.is_accessor_storage(m)) {
            // An `accessor`'s getter and setter, reading and writing its storage, are made
            // before the other members are visited (observed helper order).
            self.tx.need(Helper::ClassPrivateFieldGet);
            self.tx.need(Helper::ClassPrivateFieldSet);
        }
        if has_privs {
            // An `accessor`'s storage is named after every other private member, a decorated
            // instance one's too unless its accessors are private (observed).
            let mut order = self.tx.items(p.members);
            let private_methods: Vec<String> = order
                .iter()
                .filter_map(|&m| match self.n(m) {
                    Node::MethodDef { key, .. } => match self.n(key) {
                        Node::PrivateName { name } => Some(self.tx.ast.atom(name).to_owned()),
                        _ => None,
                    },
                    _ => None,
                })
                .collect();
            order.sort_by_key(|&m| {
                self.is_accessor_storage(m)
                    || matches!(self.n(m), Node::PropertyDef { key, modifiers, .. }
                        if !modifiers.contains(Modifiers::STATIC)
                            && matches!(self.n(key), Node::PrivateName { name }
                                if self.tx.decorator_storages.contains(&name)
                                    && self.tx.ast.atom(name).strip_suffix("_accessor_storage")
                                        .is_some_and(|b| !private_methods.iter().any(|p| p == b))))
            });
            for m in order {
                let (key, is_static, kind) = match self.n(m) {
                    Node::PropertyDef { key, modifiers, .. } => (key, modifiers, None),
                    Node::MethodDef {
                        key,
                        modifiers,
                        kind,
                        ..
                    } => (key, modifiers, Some(kind)),
                    _ => continue,
                };
                let Node::PrivateName { name } = self.n(key) else {
                    continue;
                };
                let is_static = is_static.contains(Modifiers::STATIC);
                let text = self.tx.ast.atom(name).to_owned();
                let suffix = match kind {
                    Some(ts_r_ast::MethodKind::Get) => "_get",
                    Some(ts_r_ast::MethodKind::Set) => "_set",
                    _ => "",
                };
                let a = self.priv_storage(base_name.as_deref(), &format!("{text}{suffix}"));
                storage_of.insert(m, a);
                let pk = match kind {
                    None => PrivKind::Field,
                    Some(ts_r_ast::MethodKind::Get | ts_r_ast::MethodKind::Set) => {
                        PrivKind::Accessor
                    }
                    Some(_) => PrivKind::Method,
                };
                let e = env.members.entry(name).or_insert(PrivMember {
                    kind: pk,
                    is_static,
                    storage: None,
                    get: None,
                    set: None,
                });
                match kind {
                    Some(ts_r_ast::MethodKind::Get) => e.get = Some(a),
                    Some(ts_r_ast::MethodKind::Set) => e.set = Some(a),
                    _ => e.storage = Some(a),
                }
            }
        }
        // Static code can observe the class's name (`__setFunctionName` below); private
        // instance members alone cannot (observed).
        let static_code = !statics.is_empty() || caps_needed;
        // Only static members name the class (an instance field's key capture does not).
        let has_statics = statics.iter().any(|s| match *s {
            Static::Expr(e) => !self.is_temp_capture(e),
            Static::Define(m) => matches!(self.n(m), Node::PropertyDef { value: Some(_), .. }),
            _ => true,
        });
        let heritage = if u.sup { Some(self.class_temp()) } else { None };
        // Field keys, in member order.
        let mut caps = Vec::new();
        let mut keys: FxHashMap<NodeId, NodeId> = FxHashMap::default();
        for m in self.tx.items(p.members) {
            let is_field = fields.contains(&m)
                || statics
                    .iter()
                    .any(|s| matches!(*s, Static::Define(d) if d == m));
            if is_field && let Node::PropertyDef { key, .. } = self.n(m) {
                let k = self.field_key(key, &mut caps);
                keys.insert(m, k);
            }
        }
        // Key captures from the TypeScript transform join this transform's `var`.
        for s in &statics {
            let Static::Expr(e) = *s else {
                continue;
            };
            let items = match self.n(e) {
                Node::Seq { exprs } if self.tx.key_exprs.contains(&e) => self.tx.items(exprs),
                _ => vec![e],
            };
            for e in items {
                if let Node::Assign { left, .. } = self.n(e)
                    && let Node::Ident { name } = self.n(left)
                    && self.tx.temp_atoms.contains(&name)
                {
                    self.adopt_temp(name);
                }
            }
        }
        // An `accessor`'s key temporary (`get [_b = k]() {}` / `set [_b](v) {}`) too.
        let members = self.tx.items(p.members);
        for w in members.windows(2) {
            let (Node::MethodDef { key: k1, .. }, Node::MethodDef { key: k2, .. }) =
                (self.n(w[0]), self.n(w[1]))
            else {
                continue;
            };
            let (Node::ComputedKey { expr: e1 }, Node::ComputedKey { expr: e2 }) =
                (self.n(k1), self.n(k2))
            else {
                continue;
            };
            if let Node::Assign {
                op: ts_r_ast::AssignOp::Assign,
                left,
                ..
            } = self.n(self.skip_parens(e1))
                && let Some(name) = self.tx.atom_of(left)
                && self.tx.temp_atoms.contains(&name)
                && self.tx.atom_of(e2) == Some(name)
            {
                self.adopt_temp(name);
            }
        }
        // A class expression's alias is named after its keys' temporaries (observed).
        if alias.is_none() && is_expr && (static_code || has_privs) {
            alias = Some(self.class_temp());
        }
        if has_privs {
            env.brand = alias;
            self.declared.extend(env.members.keys().copied());
            self.privates.push(env);
        }
        // `export default class { static ... }`: `class default_1 { }` exported after.
        let mut export_default = None;
        if !p.is_expr && p.name.is_none() {
            let n = match self.tx.opts.default_class_name.as_deref() {
                Some(name) => self.unique_exact(name),
                None => self.unique("default"),
            };
            p.name = Some(self.ident_atom(n, at));
            p.modifiers.remove(Modifiers::EXPORT | Modifiers::DEFAULT);
            export_default = Some(n);
        }
        let name_ref = p.name.and_then(|n| self.tx.atom_of(n));
        if let Some(h) = heritage
            && let Some(e) = extends
        {
            let moved = crate::builder::clone_node(self.tx.ast, e);
            let t = self.ident_atom(h, at);
            let a = self.assign(t, moved, at);
            let par = self.paren(a, at);
            self.put(e, par);
        }
        if let Some(a) = alias {
            let s = Subst {
                alias: a,
                name: class_name,
                heritage,
            };
            if class_name.is_some() {
                for &m in kept
                    .iter()
                    .chain(&fields)
                    .chain(&priv_fields)
                    .chain(&priv_methods)
                {
                    self.subst(m, s, false);
                }
            }
            for st in &statics {
                match *st {
                    Static::Prop(st, _) => {
                        if let Node::ExprStmt { expr } = self.n(st)
                            && let Node::Assign { left, right, .. } = self.n(expr)
                        {
                            if let Node::Index { index, .. } = self.n(left) {
                                self.subst(index, s, true);
                            }
                            self.subst(right, s, true);
                        }
                    }
                    // A key capture is evaluated outside the class: its `this` is the outer
                    // one (observed).
                    Static::Expr(e) => {
                        let capture = self.is_temp_capture(e);
                        self.subst(e, s, !capture)
                    }
                    Static::Block(body, _) => {
                        for b in self.tx.items(body) {
                            self.subst(b, s, true);
                        }
                    }
                    Static::Define(m) | Static::PrivField(m) => {
                        if let Node::PropertyDef { value: Some(v), .. } = self.n(m) {
                            self.subst(v, s, true);
                        }
                    }
                }
            }
        }
        let derived = extends.is_some();
        if self.tx.opts.use_define_for_class_fields && !priv_fields.is_empty() {
            // Private fields are set among the defined ones, in member order.
            let all: Vec<NodeId> = self
                .tx
                .items(p.members)
                .into_iter()
                .filter(|m| fields.contains(m) || priv_fields.contains(m))
                .collect();
            self.define_fields(&mut kept, &all, &keys, &storage_of, derived, at);
        } else {
            if !fields.is_empty() {
                self.define_fields(&mut kept, &fields, &keys, &storage_of, derived, at);
            }
            if !priv_fields.is_empty() {
                self.private_inits(&mut kept, &priv_fields, &storage_of, derived, at);
            }
        }
        if let Some(i) = instances {
            // `_C_instances.add(this);` first (after `super(...)`).
            let r = self.ident_atom(i, at);
            let add = self.member(r, "add", at);
            let this = self.this(at);
            let c = self.call(add, &[this], at);
            let st = self.expr_stmt(c, at);
            // It stands for no source statement: it takes no comments.
            let none = ts_r_codegen::NO_MAP;
            self.tx.map_range(st, ts_r_ast::Span::new(none, none));
            self.ctor_prepend(&mut kept, st, derived, at);
        }
        p.members = self.new_list(&kept);
        crate::class::set_parts(self.tx, id, p);
        // The static members, in order, as expressions (`Prop` targets the class).
        let target = if is_expr { alias } else { name_ref };
        let mut lead = caps;
        let mut rest = Vec::new();
        // The member a moved static property or block stands for: its statement after a
        // class declaration takes the member's comments (`C.x = 1; // c`).
        let mut origin: FxHashMap<NodeId, ts_r_ast::Span> = FxHashMap::default();
        let mut leading = true;
        for st in statics {
            match st {
                Static::Define(m) => {
                    let Node::PropertyDef { value: Some(v), .. } = self.n(m) else {
                        continue;
                    };
                    leading = false;
                    let mat = self.at(m);
                    let Some(t) = target else {
                        continue;
                    };
                    let obj = self.ident_atom(t, mat);
                    let key = match keys.get(&m) {
                        Some(&k) => k,
                        None => self.string("", mat),
                    };
                    rest.push(self.define_property(obj, key, v, mat));
                }
                Static::PrivField(m) => {
                    leading = false;
                    let mat = self.at(m);
                    let Some(&st) = storage_of.get(&m) else {
                        continue;
                    };
                    let v = match self.n(m) {
                        Node::PropertyDef { value: Some(v), .. } => v,
                        _ => self.void0(mat),
                    };
                    let prop = self.prop("value", v, mat);
                    let obj = self.object(&[prop], mat);
                    let t = self.ident_atom(st, mat);
                    rest.push(self.assign(t, obj, mat));
                }
                Static::Expr(e) if leading => lead.push(e),
                Static::Expr(e) => rest.push(e),
                Static::Prop(st, m) => {
                    leading = false;
                    let Node::ExprStmt { expr } = self.n(st) else {
                        continue;
                    };
                    if let Some(t) = target {
                        self.retarget(expr, t);
                    }
                    origin.insert(expr, self.tx.span(m));
                    rest.push(expr);
                }
                Static::Block(body, m) => {
                    leading = false;
                    let bat = self.at(m);
                    let block = self.mk(Node::Block { stmts: body }, bat);
                    // A multi-line block keeps the source braces (and the comments between
                    // them); the IIFE of a one-line block is multi-line anyway (observed).
                    let sp = self.tx.span(m);
                    let text = self.tx.src.get(sp.start as usize..sp.end as usize);
                    if let Some(t) = text
                        && t.contains('\n')
                        && let Some(brace) = t.find('{')
                    {
                        let span = ts_r_ast::Span {
                            start: sp.start + brace as u32,
                            end: sp.end,
                        };
                        self.tx.ast.set_span(block, span);
                    }
                    let arrow = self.mk(
                        Node::Arrow {
                            flags: FnFlags::NONE,
                            type_params: None,
                            params: NodeList::EMPTY,
                            return_type: None,
                            body: block,
                        },
                        bat,
                    );
                    let par = self.paren(arrow, bat);
                    let call = self.call(par, &[], bat);
                    origin.insert(call, sp);
                    rest.push(call);
                }
            }
        }
        // A static block's comma expression joins the sequence element by element (the TC39
        // decorator transform's `(_x_decorators = [...], _b = __propKey(k))`, observed).
        lead = self.flatten_seq(lead);
        // Key captures are evaluated before the class is named (observed).
        if lead.iter().any(|&e| self.is_temp_capture(e)) {
            let (named, others): (Vec<NodeId>, Vec<NodeId>) =
                lead.iter().partition(|&&e| self.is_set_function_name(e));
            lead = others;
            lead.extend(named);
        }
        rest = self.flatten_seq(rest);
        if has_privs {
            let inits;
            (lead, inits) =
                self.private_lead(lead, &priv_fields, &priv_methods, instances, &storage_of);
            // The `WeakMap`s / `WeakSet` are created in the first computed key of the class,
            // before the key is evaluated (observed).
            let first_computed =
                self.tx
                    .items(p.members)
                    .into_iter()
                    .find_map(|m| match self.n(m) {
                        Node::MethodDef { key, .. } => match self.n(key) {
                            Node::ComputedKey { expr } => Some((key, expr)),
                            _ => None,
                        },
                        _ => None,
                    });
            if inits > 0
                && let Some((key, expr)) = first_computed
            {
                let mut items: Vec<NodeId> = lead.drain(..inits).collect();
                items.extend(self.flatten_seq(vec![expr]));
                let kat = self.at(key);
                let seq = self.seq(&items, kat);
                let e = self.paren(seq, kat);
                self.tx.set(key, Node::ComputedKey { expr: e });
            }
        }
        if is_expr {
            let Some(a) = alias else {
                self.children(id);
                return;
            };
            let class = crate::builder::clone_node(self.tx.ast, id);
            self.children(class);
            let t = self.ident_atom(a, at);
            let first = self.assign(t, class, at);
            // An anonymous class would take the alias's name (unless its static code names
            // it already).
            let names_itself = lead.iter().any(|&e| self.is_set_function_name(e));
            let named = match (class_name, hint) {
                (None, Some(h)) if !names_itself && has_statics => {
                    let f = self.tx.helper_ref(Helper::SetFunctionName, at);
                    let r = self.ident_atom(a, at);
                    let text = self.tx.ast.atom(h).to_owned();
                    let s = self.string(&text, at);
                    Some(self.call(f, &[r, s], at))
                }
                _ => None,
            };
            if class_this.is_some() && in_var {
                self.put(id, first);
                // Key captures, private storage and method functions come before the class
                // is named; static code after (observed).
                let (captures, lead): (Vec<NodeId>, Vec<NodeId>) = lead
                    .iter()
                    .partition(|&&e| matches!(self.n(e), Node::Assign { .. }));
                for e in captures.into_iter().chain(named).chain(lead).chain(rest) {
                    self.visit(e);
                    let eat = self.at(e);
                    let s = self.expr_stmt(e, eat);
                    self.after.push(s);
                }
                return;
            }
            // Key captures come before the name too (`(_a = class {}, _b = k(),
            // __setFunctionName(_a, "C"), _a[_b] = 1, _a)`, observed).
            let (captures, lead): (Vec<NodeId>, Vec<NodeId>) =
                lead.iter().partition(|&&e| self.is_temp_capture(e));
            let mut items = vec![first];
            for e in captures {
                self.visit(e);
                items.push(e);
            }
            items.extend(named);
            for e in lead.into_iter().chain(rest) {
                self.visit(e);
                items.push(e);
            }
            items.push(self.ident_atom(a, at));
            let seq = self.seq(&items, at);
            self.put(id, seq);
            self.tx.mark_multi_line(id);
            return;
        }
        self.children(id);
        let mut out = Vec::new();
        if let Some(a) = alias {
            let t = self.ident_atom(a, at);
            let Some(n) = name_ref else {
                return;
            };
            let r = self.ident_atom(n, at);
            lead.insert(0, self.assign(t, r, at));
        }
        if !lead.is_empty() {
            for &e in &lead {
                self.visit(e);
            }
            let e = self.seq(&lead, at);
            out.push(self.expr_stmt(e, at));
        }
        for e in rest {
            self.visit(e);
            let eat = self.at(e);
            let s = self.expr_stmt(e, eat);
            if let Some(&span) = origin.get(&e)
                && span.start < span.end
            {
                self.tx.moved_comments.push((s, span));
            }
            out.push(s);
        }
        // `export default class C { static ... }`: `class C { }`, the statics, then
        // `export default C;` (observed).
        if export_default.is_none()
            && !out.is_empty()
            && let Some(n) = name_ref
            && let Some(mut q) = crate::class::parts(self.tx, id)
            && !q.is_expr
            && q.modifiers.contains(Modifiers::EXPORT | Modifiers::DEFAULT)
        {
            q.modifiers.remove(Modifiers::EXPORT | Modifiers::DEFAULT);
            crate::class::set_parts(self.tx, id, q);
            export_default = Some(n);
        }
        if let Some(n) = export_default {
            let r = self.ident_atom(n, at);
            out.push(self.mk(Node::ExportDefaultExpr { expr: r }, at));
        }
        self.after.extend(out);
    }

    /// `super` keeps the extracted method's home object through arrows and nested class
    /// heritage/computed keys, but not through another method or class initializer.
    fn extracted_super(&self, id: NodeId) -> bool {
        let mut stack = vec![id];
        while let Some(id) = stack.pop() {
            match self.n(id) {
                Node::Super {} => return true,
                Node::FunctionDecl { .. }
                | Node::FunctionExpr { .. }
                | Node::StaticBlock { .. } => {}
                Node::MethodDef { key, .. } | Node::PropertyDef { key, .. } => {
                    if self.tx.kind(key) == NodeKind::ComputedKey {
                        stack.push(key);
                    }
                }
                _ => stack.extend(self.tx.ast.children(id)),
            }
        }
        false
    }

    /// An IIFE cannot contain an outer `await` or `yield` from a class's heritage or keys.
    fn class_evaluation_suspends(&self, id: NodeId) -> bool {
        let mut stack = vec![id];
        while let Some(id) = stack.pop() {
            match self.n(id) {
                Node::Await { .. } | Node::Yield { .. } => return true,
                Node::FunctionDecl { .. } | Node::FunctionExpr { .. } | Node::Arrow { .. } => {}
                Node::MethodDef { key, .. } => {
                    if self.tx.kind(key) == NodeKind::ComputedKey {
                        stack.push(key);
                    }
                }
                Node::PropertyDef {
                    key,
                    value,
                    modifiers,
                    ..
                } => {
                    if self.tx.kind(key) == NodeKind::ComputedKey {
                        stack.push(key);
                    }
                    if modifiers.contains(Modifiers::STATIC) {
                        stack.extend(value);
                    }
                }
                _ => stack.extend(self.tx.ast.children(id)),
            }
        }
        false
    }

    /// True if field `m` has a computed key captured in a temporary.
    fn key_captured(&self, m: NodeId) -> bool {
        matches!(self.n(m), Node::PropertyDef { key, .. }
            if matches!(self.n(key), Node::ComputedKey { expr } if !self.is_literal_key(expr)))
    }

    /// True if a computed key's expression is a literal (used as is; see `class::is_literal`).
    fn is_literal_key(&self, e: NodeId) -> bool {
        crate::class::is_literal(self.tx, e)
    }

    /// The key expression of a field defined with `Object.defineProperty` (`"x"`, `0`, or a
    /// temporary capturing a computed key, its `_a = k` pushed to `caps`).
    fn field_key(&mut self, key: NodeId, caps: &mut Vec<NodeId>) -> NodeId {
        let at = self.at(key);
        match self.n(key) {
            Node::Ident { name } => {
                let text = self.tx.ast.atom(name).to_owned();
                self.string(&text, at)
            }
            Node::ComputedKey { expr } if self.is_literal_key(expr) => expr,
            Node::ComputedKey { expr } if self.seq_into_temp(expr).is_some() => {
                // `[(a, b, _b = __propKey(k))]`: the elements are captured as they are and
                // `_b` is the key (observed).
                let (items, t) = self.seq_into_temp(expr).unwrap_or_default();
                caps.extend(items);
                self.ident_atom(t, at)
            }
            Node::ComputedKey { expr } => {
                let t = self.class_temp();
                let target = self.ident_atom(t, at);
                caps.push(self.assign(target, expr, at));
                self.ident_atom(t, at)
            }
            _ => key,
        }
    }

    /// A (parenthesized) comma expression whose last element assigns a temporary: its
    /// elements and the temporary.
    fn seq_into_temp(&self, e: NodeId) -> Option<(Vec<NodeId>, Atom)> {
        let Node::Seq { exprs } = self.n(self.skip_parens(e)) else {
            return None;
        };
        let items = self.tx.items(exprs);
        let &last = items.last()?;
        let Node::Assign {
            op: ts_r_ast::AssignOp::Assign,
            left,
            ..
        } = self.n(last)
        else {
            return None;
        };
        let t = self.tx.atom_of(left)?;
        self.tx.temp_atoms.contains(&t).then_some((items, t))
    }

    /// Expressions with (parenthesized) comma sequences spread into their elements.
    fn flatten_seq(&self, items: Vec<NodeId>) -> Vec<NodeId> {
        let mut out = Vec::with_capacity(items.len());
        for e in items {
            match self.n(self.skip_parens(e)) {
                Node::Seq { exprs } => out.extend(self.tx.items(exprs)),
                _ => out.push(e),
            }
        }
        out
    }

    /// `Object.defineProperty(obj, key, { enumerable: true, configurable: true, writable:
    /// true, value: v })`, the descriptor one property per line.
    fn define_property(&mut self, obj: NodeId, key: NodeId, value: NodeId, at: u32) -> NodeId {
        let f = self.dotted("Object.defineProperty", at);
        let mut props = Vec::with_capacity(4);
        for name in ["enumerable", "configurable", "writable"] {
            let t = self.bool_lit(true, at);
            props.push(self.prop(name, t, at));
        }
        props.push(self.prop("value", value, at));
        let desc = self.object(&props, at);
        self.tx.mark_multi_line(desc);
        self.call(f, &[obj, key, desc], at)
    }

    /// Instance fields kept as fields become `Object.defineProperty(this, ...)` in the
    /// constructor, after `super(...)` and the parameter properties (which become
    /// definitions too).
    fn define_fields(
        &mut self,
        kept: &mut Vec<NodeId>,
        fields: &[NodeId],
        keys: &FxHashMap<NodeId, NodeId>,
        privs: &FxHashMap<NodeId, Atom>,
        derived: bool,
        at: u32,
    ) {
        let ctor = kept.iter().copied().find(|&m| {
            matches!(
                self.n(m),
                Node::MethodDef {
                    kind: ts_r_ast::MethodKind::Constructor,
                    ..
                }
            )
        });
        let body = ctor.and_then(|c| match self.n(c) {
            Node::MethodDef { value, .. } => match self.n(value) {
                Node::FunctionExpr { body, .. } => body,
                _ => None,
            },
            _ => None,
        });
        let mut stmts = body
            .map(|b| match self.n(b) {
                Node::Block { stmts } => self.tx.items(stmts),
                _ => Vec::new(),
            })
            .unwrap_or_default();
        // Where definitions go: after `super(...)` and the parameter property assignments.
        let mut pos = match stmts.iter().position(|&s| self.is_super_call_stmt(s)) {
            Some(i) => i + 1,
            None => stmts
                .iter()
                .take_while(|&&s| self.tx.kind(s) == NodeKind::Directive)
                .count(),
        };
        let mut inits = Vec::new();
        for &m in fields {
            let Node::PropertyDef { key, value, .. } = self.n(m) else {
                continue;
            };
            let mat = self.at(m);
            let k = match keys.get(&m) {
                Some(&k) => k,
                None => self.string("", mat),
            };
            let span = self.tx.span(m);
            // A parameter property: its `this.p = p;` becomes the definition.
            if span.start == span.end
                && value.is_none()
                && let Some(name) = self.tx.atom_of(key)
                && let Some(i) = stmts
                    .iter()
                    .position(|&s| self.is_param_prop_assign(s, name))
                && let Node::ExprStmt { expr } = self.n(stmts[i])
                && let Node::Assign { right, .. } = self.n(expr)
            {
                // The value is the decoded name, even when escaped (`value: arg2`; observed).
                self.tx.ast.set_span(right, ts_r_ast::Span::new(mat, mat));
                let this = self.this(mat);
                let d = self.define_property(this, k, right, mat);
                stmts[i] = self.expr_stmt(d, mat);
                pos = pos.max(i + 1);
                continue;
            }
            let this = self.this(mat);
            let v = match value {
                Some(v) => v,
                None => {
                    let zero = self.number(0.0, mat);
                    self.unary(ts_r_ast::UnaryOp::Void, zero, mat)
                }
            };
            let init = if let Some(&st) = privs.get(&m) {
                self.weak_set_call(st, this, v, mat)
            } else {
                self.define_property(this, k, v, mat)
            };
            let st = self.expr_stmt(init, mat);
            // It takes the field's comments along (`Object.defineProperty(...); // c`).
            if span.start < span.end {
                self.tx.moved_comments.push((st, span));
            }
            inits.push(st);
        }
        kept.retain(|m| !fields.contains(m));
        for (i, s) in inits.into_iter().enumerate() {
            stmts.insert(pos + i, s);
        }
        match body {
            Some(b) => {
                let l = self.new_list(&stmts);
                self.tx.set(b, Node::Block { stmts: l });
            }
            None => {
                if stmts.is_empty() {
                    return;
                }
                let mut all = Vec::with_capacity(stmts.len() + 1);
                if derived {
                    let sup = self.mk(Node::Super {}, at);
                    let args = self.ident_atom(self.arguments, at);
                    let spread = self.spread(args, at);
                    let call = self.call(sup, &[spread], at);
                    all.push(self.expr_stmt(call, at));
                }
                all.extend(stmts);
                let key = self.ident("constructor", at);
                let c = self.method(
                    Modifiers::NONE,
                    ts_r_ast::MethodKind::Constructor,
                    key,
                    &[],
                    &all,
                    at,
                );
                kept.insert(0, c);
            }
        }
    }

    /// A private storage name `_C_x` (`_x` for an anonymous class), hoisted.
    fn priv_storage(&mut self, class: Option<&str>, name: &str) -> Atom {
        let text = match class {
            Some(c) => format!("_{c}_{name}"),
            None => format!("_{name}"),
        };
        let a = self.unique_exact(&text);
        self.hoist(a, Group::Es2022);
        a
    }

    /// `_C_x.set(this, v)`
    fn weak_set_call(&mut self, storage: Atom, this: NodeId, v: NodeId, at: u32) -> NodeId {
        let r = self.ident_atom(storage, at);
        let f = self.member(r, "set", at);
        self.call(f, &[this, v], at)
    }

    /// The statement after a class declaration (or the class expression's items): the
    /// instance fields' `WeakMap`s, `_C_instances = new WeakSet()`, then the private
    /// methods' functions and the key captures in member order (observed).
    fn private_lead(
        &mut self,
        lead: Vec<NodeId>,
        priv_fields: &[NodeId],
        priv_methods: &[NodeId],
        instances: Option<Atom>,
        storage_of: &FxHashMap<NodeId, Atom>,
    ) -> (Vec<NodeId>, usize) {
        let mut out = Vec::new();
        // Fields' `WeakMap`s, `_C_instances`, then the `accessor` storages' (observed).
        for pass in [false, true] {
            if pass && let Some(i) = instances {
                let at = priv_methods.first().map_or(0, |&m| self.at(m));
                let e = self.new_of("WeakSet", at);
                let t = self.ident_atom(i, at);
                out.push(self.assign(t, e, at));
            }
            for &m in priv_fields {
                if self.is_accessor_storage(m) != pass {
                    continue;
                }
                let Some(&st) = storage_of.get(&m) else {
                    continue;
                };
                let mat = self.at(m);
                let e = self.new_of("WeakMap", mat);
                let t = self.ident_atom(st, mat);
                out.push(self.assign(t, e, mat));
            }
        }
        let inits = out.len();
        // Key captures (`_b = k`) are in member order with the methods; other static
        // expressions (`__setFunctionName(...)`) follow the method functions (observed).
        let mut ordered: Vec<(u32, NodeId)> = lead
            .iter()
            .map(|&e| {
                let capture = matches!(self.n(e), Node::Assign { left, .. }
                    if self.tx.atom_of(left).is_some_and(|a| self.tx.temp_atoms.contains(&a) || self.reserved.contains(&a)));
                (if capture { self.at(e) } else { u32::MAX }, e)
            })
            .collect();
        for &m in priv_methods {
            let Some(&st) = storage_of.get(&m) else {
                continue;
            };
            let Node::MethodDef { value, .. } = self.n(m) else {
                continue;
            };
            let mat = self.at(m);
            if let Node::FunctionExpr {
                modifiers,
                flags,
                type_params,
                params,
                return_type,
                body,
                ..
            } = self.n(value)
            {
                let name = self.ident_atom(st, mat);
                self.tx.set(
                    value,
                    Node::FunctionExpr {
                        modifiers,
                        flags,
                        name: Some(name),
                        type_params,
                        params,
                        return_type,
                        body,
                    },
                );
            }
            let t = self.ident_atom(st, mat);
            ordered.push((mat, self.assign(t, value, mat)));
        }
        ordered.sort_by_key(|&(pos, _)| pos);
        out.extend(ordered.into_iter().map(|(_, e)| e));
        (out, inits)
    }

    /// The private storage field the TypeScript transform made for an `accessor`
    /// (`#x_accessor_storage`).
    fn is_accessor_storage(&self, m: NodeId) -> bool {
        matches!(self.n(m), Node::PropertyDef { key, .. }
            if matches!(self.n(key), Node::PrivateName { name }
                if self.tx.ast.atom(name).ends_with("_accessor_storage")
                    && !self.tx.decorator_storages.contains(&name)))
    }

    /// `new WeakMap()`
    fn new_of(&mut self, class: &str, at: u32) -> NodeId {
        let callee = self.ident(class, at);
        self.mk(
            Node::New {
                callee,
                type_args: None,
                has_args: true,
                args: NodeList::EMPTY,
            },
            at,
        )
    }

    /// The constructor's body block and statements (an empty constructor is added first,
    /// `super(...arguments)` in a derived class, when there is none).
    fn ctor_stmts(
        &mut self,
        kept: &mut Vec<NodeId>,
        derived: bool,
        at: u32,
    ) -> (NodeId, Vec<NodeId>) {
        let ctor = kept.iter().copied().find(|&m| {
            matches!(
                self.n(m),
                Node::MethodDef {
                    kind: ts_r_ast::MethodKind::Constructor,
                    ..
                }
            )
        });
        let body = ctor.and_then(|c| match self.n(c) {
            Node::MethodDef { value, .. } => match self.n(value) {
                Node::FunctionExpr { body, .. } => body,
                _ => None,
            },
            _ => None,
        });
        if let Some(b) = body {
            let stmts = match self.n(b) {
                Node::Block { stmts } => self.tx.items(stmts),
                _ => Vec::new(),
            };
            return (b, stmts);
        }
        let mut all = Vec::new();
        if derived {
            let sup = self.mk(Node::Super {}, at);
            let args = self.ident_atom(self.arguments, at);
            let spread = self.spread(args, at);
            let call = self.call(sup, &[spread], at);
            all.push(self.expr_stmt(call, at));
        }
        let key = self.ident("constructor", at);
        let c = self.method(
            Modifiers::NONE,
            ts_r_ast::MethodKind::Constructor,
            key,
            &[],
            &all,
            at,
        );
        kept.insert(0, c);
        let b = match self.n(c) {
            Node::MethodDef { value, .. } => match self.n(value) {
                Node::FunctionExpr { body: Some(b), .. } => b,
                _ => c,
            },
            _ => c,
        };
        (b, all)
    }

    /// Where a constructor's own code starts: after `super(...)`, else after the prologue.
    fn ctor_start(&self, stmts: &[NodeId]) -> usize {
        match stmts.iter().position(|&s| self.is_super_call_stmt(s)) {
            Some(i) => i + 1,
            None => stmts
                .iter()
                .take_while(|&&s| self.tx.kind(s) == NodeKind::Directive)
                .count(),
        }
    }

    /// Inserts `st` where the constructor's own code starts.
    fn ctor_prepend(&mut self, kept: &mut Vec<NodeId>, st: NodeId, derived: bool, at: u32) {
        let (b, mut stmts) = self.ctor_stmts(kept, derived, at);
        let pos = self.ctor_start(&stmts);
        stmts.insert(pos, st);
        let l = self.new_list(&stmts);
        self.tx.set(b, Node::Block { stmts: l });
    }

    /// Without `useDefineForClassFields`: the initializers the TypeScript transform moved
    /// into the constructor (`this.#x = v;`) become `_C_x.set(this, v);`, and a field without
    /// one (or one it left in place) gets `_C_x.set(this, v)` in member order among them
    /// (observed).
    fn private_inits(
        &mut self,
        kept: &mut Vec<NodeId>,
        priv_fields: &[NodeId],
        storage_of: &FxHashMap<NodeId, Atom>,
        derived: bool,
        at: u32,
    ) {
        let (b, mut stmts) = self.ctor_stmts(kept, derived, at);
        let bs = self.tx.span(b);
        // A statement moved in from a field: its position is outside the body.
        let moved = |l: &Self, s: NodeId| {
            let sp = l.tx.span(s);
            bs.start >= bs.end || sp.start < bs.start || sp.start >= bs.end
        };
        let mut by_name: FxHashMap<Atom, Atom> = FxHashMap::default();
        for &m in priv_fields {
            if let Node::PropertyDef { key, .. } = self.n(m)
                && let Node::PrivateName { name } = self.n(key)
                && let Some(&st) = storage_of.get(&m)
            {
                by_name.insert(name, st);
            }
        }
        let mut done: Vec<Atom> = Vec::new();
        // Moved statements replaced by their `_C_x.set(this, v);` (which takes their comments).
        let mut replaced: FxHashMap<NodeId, NodeId> = FxHashMap::default();
        for s in stmts.iter_mut() {
            if !moved(self, *s) {
                continue;
            }
            let Node::ExprStmt { expr } = self.n(*s) else {
                continue;
            };
            let Node::Assign {
                op: ts_r_ast::AssignOp::Assign,
                left,
                right,
            } = self.n(expr)
            else {
                continue;
            };
            let Node::Member { obj, prop, .. } = self.n(left) else {
                continue;
            };
            let Node::PrivateName { name } = self.n(prop) else {
                continue;
            };
            if self.n(obj) != (Node::This {}) {
                continue;
            }
            let Some(&st) = by_name.get(&name) else {
                continue;
            };
            let sat = self.at(*s);
            let this = self.this(sat);
            let c = self.weak_set_call(st, this, right, sat);
            let ns = self.expr_stmt(c, sat);
            self.tx.ast.set_span(ns, self.tx.span(*s));
            replaced.insert(*s, ns);
            *s = ns;
            done.push(name);
        }
        self.tx.moved_remap.extend(replaced);
        for &m in priv_fields {
            let Node::PropertyDef { key, value, .. } = self.n(m) else {
                continue;
            };
            let Node::PrivateName { name } = self.n(key) else {
                continue;
            };
            if done.contains(&name) {
                continue;
            }
            let Some(&st) = by_name.get(&name) else {
                continue;
            };
            let mat = self.at(m);
            let field_init = |l: &Self, s: NodeId| {
                moved(l, s)
                    && !l.is_super_call_stmt(s)
                    && !l.is_any_param_prop(s)
                    && l.is_field_init(s)
            };
            let idx = stmts
                .iter()
                .position(|&s| field_init(self, s) && self.at(s) > mat)
                .or_else(|| {
                    stmts
                        .iter()
                        .rposition(|&s| field_init(self, s))
                        .map(|i| i + 1)
                })
                .unwrap_or_else(|| {
                    let start = self.ctor_start(&stmts);
                    start
                        + stmts[start..]
                            .iter()
                            .take_while(|&&s| self.is_any_param_prop(s))
                            .count()
                });
            let this = self.this(mat);
            // A field the TypeScript transform left with its initializer (no other field
            // moved), or none.
            let v = match value {
                Some(v) => v,
                None => self.void0(mat),
            };
            let c = self.weak_set_call(st, this, v, mat);
            let ns = self.expr_stmt(c, mat);
            let span = self.tx.span(m);
            if span.start < span.end {
                self.tx.moved_comments.push((ns, span));
            }
            stmts.insert(idx, ns);
        }
        let l = self.new_list(&stmts);
        self.tx.set(b, Node::Block { stmts: l });
    }

    /// A field initialization in a constructor: `this.x = v;`, `this[k] = v;`,
    /// `_C_x.set(this, v);` or `Object.defineProperty(this, ...)`.
    fn is_field_init(&self, s: NodeId) -> bool {
        let Node::ExprStmt { expr } = self.n(s) else {
            return false;
        };
        match self.n(expr) {
            Node::Assign {
                op: ts_r_ast::AssignOp::Assign,
                left,
                ..
            } => matches!(self.n(left), Node::Member { obj, .. } | Node::Index { obj, .. }
                if self.n(obj) == Node::This {}),
            Node::Call { callee, args, .. } => {
                matches!(self.n(callee), Node::Member { prop, .. }
                    if self.tx.atom_of(prop).is_some_and(|a| matches!(self.tx.ast.atom(a), "set" | "defineProperty")))
                    && self
                        .tx
                        .items(args)
                        .first()
                        .is_some_and(|&a| self.n(a) == Node::This {})
            }
            _ => false,
        }
    }

    /// `this.p = p;` (a parameter property's assignment, any name).
    fn is_any_param_prop(&self, s: NodeId) -> bool {
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

    /// `super(...);`
    fn is_super_call_stmt(&self, s: NodeId) -> bool {
        let Node::ExprStmt { expr } = self.n(s) else {
            return false;
        };
        let e = self.skip_parens(expr);
        matches!(self.n(e), Node::Call { callee, .. } if self.n(callee) == Node::Super {})
    }

    /// `this.p = p;` for the parameter property `p`.
    fn is_param_prop_assign(&self, s: NodeId, name: Atom) -> bool {
        let Node::ExprStmt { expr } = self.n(s) else {
            return false;
        };
        let Node::Assign { left, right, .. } = self.n(expr) else {
            return false;
        };
        matches!(self.n(left), Node::Member { obj, prop, .. }
            if self.n(obj) == Node::This {} && self.tx.atom_of(prop) == Some(name))
            && self.tx.atom_of(right) == Some(name)
    }

    /// `_a = k` (a TypeScript transform temporary assigned: a computed key's capture).
    pub(super) fn is_temp_capture(&self, e: NodeId) -> bool {
        self.tx.key_exprs.contains(&e)
            || matches!(self.n(e), Node::Assign { op: ts_r_ast::AssignOp::Assign, left, .. }
                if self.tx.atom_of(left).is_some_and(|a| self.tx.temp_atoms.contains(&a)))
    }

    /// `__setFunctionName(...)`.
    fn is_set_function_name(&self, e: NodeId) -> bool {
        matches!(self.n(e), Node::Call { callee, .. }
            if self.tx.atom_of(callee).is_some_and(|a| self.tx.ast.atom(a) == "__setFunctionName"))
    }

    /// A new class temporary (alias or heritage), reserved in nested functions.
    fn class_temp(&mut self) -> Atom {
        let t = self.temp(Group::Es2022);
        self.reserved.insert(t);
        t
    }

    /// Moves the TypeScript transform's temporary `name` into this transform's `var`.
    fn adopt_temp(&mut self, name: Atom) {
        if self.moved.insert(name) {
            self.hoist(name, Group::Es2022);
            self.reserved.insert(name);
        }
    }

    /// The `extends` expression of a heritage list.
    fn extends_expr(&self, heritage: NodeList) -> Option<NodeId> {
        for h in self.tx.items(heritage) {
            if let Node::HeritageClause {
                kind: HeritageKind::Extends,
                types,
            } = self.n(h)
            {
                let t = *self.tx.ast.list(types).first()?;
                return match self.n(t) {
                    Node::ExprWithTypeArgs { expr, .. } => Some(expr),
                    _ => Some(t),
                };
            }
        }
        None
    }

    /// `this.x = v` / `this[k] = v`.
    fn is_this_prop(&self, e: NodeId) -> bool {
        let Node::Assign {
            op: ts_r_ast::AssignOp::Assign,
            left,
            ..
        } = self.n(e)
        else {
            return false;
        };
        matches!(self.n(left), Node::Member { obj, .. } | Node::Index { obj, .. } if self.n(obj) == Node::This {})
    }

    /// `this.x = v` → `T.x = v`.
    fn retarget(&mut self, assign: NodeId, t: Atom) {
        if let Node::Assign { left, .. } = self.n(assign)
            && let Node::Member { obj, .. } | Node::Index { obj, .. } = self.n(left)
        {
            let at = self.at(obj);
            let r = self.ident_atom(t, at);
            self.put(obj, r);
        }
    }

    /// Records what static code `id` uses: `this` and `super` (outside non-arrow functions,
    /// when `this_ok`) and the class name `name`.
    fn usage(&self, id: NodeId, name: Option<Atom>, this_ok: bool, u: &mut Usage) {
        let mut stack = vec![(id, this_ok)];
        while let Some((id, ok)) = stack.pop() {
            match self.n(id) {
                Node::This {} => u.this |= ok,
                Node::Super {} => u.sup |= ok,
                Node::Ident { name: n } => u.name |= Some(n) == name,
                Node::Member { obj, .. } => stack.push((obj, ok)),
                Node::Property { key, value, .. } => {
                    if self.tx.kind(key) == NodeKind::ComputedKey {
                        stack.push((key, ok));
                    }
                    stack.push((value, ok));
                }
                Node::MethodDef { key, value, .. } => {
                    if self.tx.kind(key) == NodeKind::ComputedKey {
                        stack.push((key, ok));
                    }
                    stack.push((value, false));
                }
                Node::PropertyDef { key, value, .. } => {
                    if self.tx.kind(key) == NodeKind::ComputedKey {
                        stack.push((key, ok));
                    }
                    if let Some(v) = value {
                        stack.push((v, false));
                    }
                }
                Node::Labeled { body, .. } => stack.push((body, ok)),
                Node::Break { .. } | Node::Continue { .. } => {}
                // A nested class's heritage, computed keys (above) and key captures run here.
                Node::StaticBlock { body } => {
                    for st in self.tx.items(body) {
                        let outer = matches!(self.n(st), Node::ExprStmt { expr }
                            if self.tx.inline_static.contains(&id) && self.is_temp_capture(expr));
                        stack.push((st, ok && outer));
                    }
                }
                Node::ClassDecl { .. } | Node::ClassExpr { .. } => {
                    for c in self.tx.ast.children(id) {
                        stack.push((c, ok));
                    }
                }
                Node::FunctionDecl { .. } | Node::FunctionExpr { .. } => {
                    let inner = ok && self.tx.static_descriptor_fns.contains(&id);
                    for c in self.tx.ast.children(id) {
                        stack.push((c, inner));
                    }
                }
                _ => {
                    for c in self.tx.ast.children(id) {
                        stack.push((c, ok));
                    }
                }
            }
        }
    }

    /// Replaces the class name by the alias in `id`, and (when `this_ok`) `this` and
    /// `super` outside non-arrow functions.
    fn subst(&mut self, id: NodeId, s: Subst, this_ok: bool) {
        let at = self.at(id);
        match self.n(id) {
            Node::This {} if this_ok => {
                let r = self.ident_atom(s.alias, at);
                self.put(id, r);
            }
            Node::Ident { name } if Some(name) == s.name => {
                let r = self.ident_atom(s.alias, at);
                self.put(id, r);
            }
            Node::ShorthandProp {
                modifiers,
                name,
                init: None,
            } if self.tx.atom_of(name) == s.name && s.name.is_some() => {
                let value = self.ident_atom(s.alias, at);
                self.tx.set(
                    id,
                    Node::Property {
                        modifiers,
                        key: name,
                        value,
                    },
                );
            }
            Node::Call { callee, args, .. } if this_ok && self.is_super_prop(callee) => {
                for a in self.tx.items(args) {
                    self.subst(a, s, this_ok);
                }
                let get = self.reflect_get(callee, s);
                let f = self.member(get, "call", at);
                let mut list = vec![self.ident_atom(s.alias, at)];
                list.extend(self.tx.items(args));
                let c = self.call(f, &list, at);
                self.put(id, c);
            }
            Node::ExprStmt { expr } if this_ok && self.is_super_write(expr) => {
                self.super_write(expr, s, true);
            }
            Node::Assign { .. } | Node::Update { .. } if this_ok && self.is_super_write(id) => {
                self.super_write(id, s, false);
            }
            Node::Assign {
                op: ts_r_ast::AssignOp::Assign,
                left,
                right,
            } if this_ok
                && matches!(self.tx.kind(left), NodeKind::ObjectLit | NodeKind::ArrayLit) =>
            {
                self.super_target(left, s);
                self.subst(right, s, this_ok);
            }
            Node::TaggedTemplate {
                tag,
                type_args,
                quasi,
            } if this_ok && self.is_super_prop(tag) => {
                self.subst(quasi, s, this_ok);
                let get = self.reflect_get(tag, s);
                let bind = self.member(get, "bind", at);
                let r = self.ident_atom(s.alias, at);
                let bound = self.call(bind, &[r], at);
                self.tx.set(
                    id,
                    Node::TaggedTemplate {
                        tag: bound,
                        type_args,
                        quasi,
                    },
                );
            }
            Node::Member { .. } | Node::Index { .. } if this_ok && self.is_super_prop(id) => {
                let get = self.reflect_get(id, s);
                self.put(id, get);
            }
            Node::Member { obj, .. } => self.subst(obj, s, this_ok),
            Node::Property { key, value, .. } => {
                if self.tx.kind(key) == NodeKind::ComputedKey {
                    self.subst(key, s, this_ok);
                }
                self.subst(value, s, this_ok);
            }
            Node::BindingProperty { key, value, init } => {
                if self.tx.kind(key) == NodeKind::ComputedKey {
                    self.subst(key, s, this_ok);
                }
                if let Some(v) = value {
                    self.subst(v, s, this_ok);
                }
                if let Some(i) = init {
                    self.subst(i, s, this_ok);
                }
            }
            Node::MethodDef { key, value, .. } => {
                if self.tx.kind(key) == NodeKind::ComputedKey {
                    self.subst(key, s, this_ok);
                }
                self.subst(value, s, false);
            }
            Node::PropertyDef { key, value, .. } => {
                if self.tx.kind(key) == NodeKind::ComputedKey {
                    self.subst(key, s, this_ok);
                }
                if let Some(v) = value {
                    self.subst(v, s, false);
                }
            }
            Node::Labeled { body, .. } => self.subst(body, s, this_ok),
            Node::Break { .. } | Node::Continue { .. } => {}
            // A nested class's heritage, computed keys (above) and key captures run here.
            Node::StaticBlock { body } => {
                for st in self.tx.items(body) {
                    let outer = matches!(self.n(st), Node::ExprStmt { expr }
                        if self.tx.inline_static.contains(&id) && self.is_temp_capture(expr));
                    self.subst(st, s, this_ok && outer);
                }
            }
            Node::ClassDecl { .. } | Node::ClassExpr { .. } => {
                for c in self.tx.ast.children(id) {
                    self.subst(c, s, this_ok);
                }
            }
            Node::FunctionDecl { .. } | Node::FunctionExpr { .. } => {
                let inner = this_ok && self.tx.static_descriptor_fns.contains(&id);
                for c in self.tx.ast.children(id) {
                    self.subst(c, s, inner);
                }
            }
            _ => {
                for c in self.tx.ast.children(id) {
                    self.subst(c, s, this_ok);
                }
            }
        }
    }

    /// `super.x` / `super[k]`.
    fn is_super_prop(&self, id: NodeId) -> bool {
        matches!(self.n(id), Node::Member { obj, .. } | Node::Index { obj, .. } if self.n(obj) == Node::Super {})
    }

    /// The heritage temporary and the key of `super.x` / `super[k]` (`"x"` / `k`).
    fn super_key(&mut self, id: NodeId, s: Subst) -> (NodeId, NodeId) {
        let at = self.at(id);
        let h = match s.heritage {
            Some(h) => self.ident_atom(h, at),
            None => self.this(at),
        };
        let key = match self.n(id) {
            Node::Member { prop, .. } => {
                let text = self
                    .tx
                    .atom_of(prop)
                    .map(|a| self.tx.ast.atom(a).to_owned())
                    .unwrap_or_default();
                self.string(&text, at)
            }
            Node::Index { index, .. } => {
                self.subst(index, s, true);
                index
            }
            _ => self.string("", at),
        };
        (h, key)
    }

    /// `Reflect.get(_b, "x", _a)` for `super.x`.
    fn reflect_get(&mut self, id: NodeId, s: Subst) -> NodeId {
        let at = self.at(id);
        let (h, key) = self.super_key(id, s);
        self.reflect_call("Reflect.get", h, key, None, s, at)
    }

    /// `Reflect.get(h, key, _a)` / `Reflect.set(h, key, value, _a)`.
    fn reflect_call(
        &mut self,
        f: &str,
        h: NodeId,
        key: NodeId,
        value: Option<NodeId>,
        s: Subst,
        at: u32,
    ) -> NodeId {
        let f = self.dotted(f, at);
        let r = self.ident_atom(s.alias, at);
        let mut args = vec![h, key];
        args.extend(value);
        args.push(r);
        self.call(f, &args, at)
    }

    /// An assignment or update whose target is `super.x` / `super[k]`.
    fn is_super_write(&self, id: NodeId) -> bool {
        match self.n(id) {
            Node::Assign { left, .. } => self.is_super_prop(left),
            Node::Update { arg, .. } => self.is_super_prop(arg),
            _ => false,
        }
    }

    /// The heritage temporary.
    fn heritage_ref(&mut self, s: Subst, at: u32) -> NodeId {
        match s.heritage {
            Some(h) => self.ident_atom(h, at),
            None => self.this(at),
        }
    }

    /// The key of `super.x` / `super[k]` for a read and a write: a computed key other than
    /// a literal is captured (`_a = k`, then `_a`).
    fn super_key_twice(&mut self, id: NodeId, s: Subst) -> (NodeId, NodeId) {
        let at = self.at(id);
        match self.n(id) {
            Node::Index { index, .. } => {
                self.subst(index, s, true);
                if matches!(
                    self.tx.kind(index),
                    NodeKind::StringLit | NodeKind::NumberLit
                ) {
                    let again = crate::builder::clone_node(self.tx.ast, index);
                    return (index, again);
                }
                let t = self.temp(Group::Es2022);
                let a = self.ident_atom(t, at);
                let first = self.assign(a, index, at);
                (first, self.ident_atom(t, at))
            }
            _ => {
                let (_, k1) = self.super_key(id, s);
                let (_, k2) = self.super_key(id, s);
                (k1, k2)
            }
        }
    }

    /// Lowers a write to `super.x` in static code through `Reflect.set` (observed):
    /// `super.x = v` is `(Reflect.set(_b, "x", _c = v, _a), _c)` (`stmt`: without the
    /// result), `super.x += v` reads with `Reflect.get`, and `++super.x` is
    /// `(Reflect.set(_b, "x", (_d = Reflect.get(..), _c = ++_d), _a), _c)`. The temporaries
    /// are made key, result, value.
    fn super_write(&mut self, id: NodeId, s: Subst, stmt: bool) {
        let at = self.at(id);
        match self.n(id) {
            Node::Assign { op, left, right } => {
                self.subst(right, s, true);
                if op == ts_r_ast::AssignOp::Assign {
                    let (h, key) = self.super_key(left, s);
                    let (value, result) = self.result_of(right, stmt, at);
                    let set = self.reflect_call("Reflect.set", h, key, Some(value), s, at);
                    let out = self.with_result(set, result, at);
                    self.put(id, out);
                    return;
                }
                let (k1, k2) = self.super_key_twice(left, s);
                let h1 = self.heritage_ref(s, at);
                let h2 = self.heritage_ref(s, at);
                let bop = super::private::compound_op(op);
                if matches!(
                    op,
                    ts_r_ast::AssignOp::OrAssign
                        | ts_r_ast::AssignOp::AndAssign
                        | ts_r_ast::AssignOp::CoalesceAssign
                ) {
                    // `Reflect.get(..) || (Reflect.set(.., _c = v, _a), _c)`
                    let get = self.reflect_call("Reflect.get", h1, k1, None, s, at);
                    let (value, result) = self.result_of(right, stmt, at);
                    let set = self.reflect_call("Reflect.set", h2, k2, Some(value), s, at);
                    let set = match result {
                        Some(_) => self.with_result(set, result, at),
                        None => set,
                    };
                    let b = self.binary(get, bop, set, at);
                    self.put(id, b);
                    return;
                }
                let get = self.reflect_call("Reflect.get", h2, k2, None, s, at);
                let v = self.binary(get, bop, right, at);
                let (value, result) = self.result_of(v, stmt, at);
                let set = self.reflect_call("Reflect.set", h1, k1, Some(value), s, at);
                let out = self.with_result(set, result, at);
                self.put(id, out);
            }
            Node::Update { op, prefix, arg } => {
                let (k1, k2) = self.super_key_twice(arg, s);
                let h1 = self.heritage_ref(s, at);
                let h2 = self.heritage_ref(s, at);
                let result = if stmt {
                    None
                } else {
                    Some(self.temp(Group::Es2022))
                };
                let t = self.temp(Group::Es2022);
                let get = self.reflect_call("Reflect.get", h2, k2, None, s, at);
                let a = self.ident_atom(t, at);
                let first = self.assign(a, get, at);
                let b = self.ident_atom(t, at);
                let mut upd = self.mk(Node::Update { op, prefix, arg: b }, at);
                if let Some(r) = result {
                    let ra = self.ident_atom(r, at);
                    upd = self.assign(ra, upd, at);
                }
                let mut items = vec![first, upd];
                if !prefix {
                    items.push(self.ident_atom(t, at));
                }
                let seq = self.seq(&items, at);
                let v = self.paren(seq, at);
                let set = self.reflect_call("Reflect.set", h1, k1, Some(v), s, at);
                let out = self.with_result(set, result, at);
                self.put(id, out);
            }
            _ => {}
        }
    }

    /// The value a write stores, assigned to a new result temporary unless `stmt`.
    fn result_of(&mut self, value: NodeId, stmt: bool, at: u32) -> (NodeId, Option<Atom>) {
        if stmt {
            return (value, None);
        }
        let t = self.temp(Group::Es2022);
        let a = self.ident_atom(t, at);
        (self.assign(a, value, at), Some(t))
    }

    /// `(set, _c)` when the write's value is used.
    fn with_result(&mut self, set: NodeId, result: Option<Atom>, at: u32) -> NodeId {
        let Some(r) = result else {
            return set;
        };
        let ra = self.ident_atom(r, at);
        let seq = self.seq(&[set, ra], at);
        self.paren(seq, at)
    }

    /// Substitutes in a destructuring target: `super.x` there becomes
    /// `({ set value(_a) { Reflect.set(_b, "x", _a, _c); } }).value` (observed).
    fn super_target(&mut self, id: NodeId, s: Subst) {
        match self.n(id) {
            Node::ObjectLit { props } => {
                for p in self.tx.items(props) {
                    match self.n(p) {
                        Node::Property { key, value, .. } => {
                            if self.tx.kind(key) == NodeKind::ComputedKey {
                                self.subst(key, s, true);
                            }
                            self.super_target(value, s);
                        }
                        Node::Spread { arg } => self.super_target(arg, s),
                        _ => self.subst(p, s, true),
                    }
                }
            }
            Node::ArrayLit { elements } => {
                for e in self.tx.items(elements) {
                    match self.n(e) {
                        Node::Spread { arg } => self.super_target(arg, s),
                        _ => self.super_target(e, s),
                    }
                }
            }
            Node::Assign {
                op: ts_r_ast::AssignOp::Assign,
                left,
                right,
            } => {
                self.super_target(left, s);
                self.subst(right, s, true);
            }
            Node::Paren { expr } => self.super_target(expr, s),
            _ if self.is_super_prop(id) => {
                let at = self.at(id);
                let (h, key) = self.super_key(id, s);
                let p = self.local_temp();
                let v = self.ident_atom(p, at);
                let set = self.reflect_call("Reflect.set", h, key, Some(v), s, at);
                let stmt = self.expr_stmt(set, at);
                let k = self.ident("value", at);
                let pname = self.ident_atom(p, at);
                let param = self.param(pname, at);
                let setter = self.method(
                    Modifiers::NONE,
                    ts_r_ast::MethodKind::Set,
                    k,
                    &[param],
                    &[stmt],
                    at,
                );
                self.tx.mark_single_line_body(setter);
                let o = self.object(&[setter], at);
                let par = self.paren(o, at);
                let m = self.member(par, "value", at);
                self.put(id, m);
            }
            _ => self.subst(id, s, true),
        }
    }
}
