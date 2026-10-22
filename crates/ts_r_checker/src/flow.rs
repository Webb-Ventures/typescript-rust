//! Control-flow narrowing.
//!
//! A reference (`x`, `this`, `x.y`, `x["k"]`) is narrowed by walking the binder's flow graph
//! backwards from the flow node recorded at the reference: assignments give the assigned
//! type (reduced against the declared type), conditions narrow (`typeof`, truthiness,
//! equality, discriminants, `instanceof`, `in`, type predicates), labels union their
//! antecedents, loop labels iterate to a fixed point, and assertion calls narrow or end the
//! path. Results are cached at labels per reference.

use std::hash::{Hash, Hasher};

use rustc_hash::FxHasher;
use ts_r_ast::{AssignOp, BinaryOp, Node, NodeId, UnaryOp, VarKind};
use ts_r_binder::flow::flow_flags;
use ts_r_binder::{FlowId, FlowKind, SymbolFlags, meaning};
use ts_r_types::{ObjectKind, PredicateKind, SymId, TypeFlags, TypeId};

use crate::checker::{Ck, GlobalType, Loc};
use crate::diag::dg;
use crate::members::SigKind;
use crate::nodes::{key_text, skip_parens};

/// Recursion limit of the backwards walk.
const MAX_FLOW_DEPTH: u32 = 1200;
/// Fixed-point iterations at a loop label.
const MAX_LOOP_ITERATIONS: u32 = 6;
/// Nodes scanned when checking whether a condition mentions a reference.
const MAX_CONDITION_SCAN: u32 = 200;

/// The identity of a narrowable reference.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct RefKey {
    pub root: RefRoot,
    pub path: Vec<Box<str>>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum RefRoot {
    Sym(SymId),
    This,
    Super,
    /// `import.meta` (its properties narrow like `this.x`).
    ImportMeta,
}

/// What a const binding (or a binding of a parameter pattern) stands for: `prop` of the
/// reference `key` (`const { kind } = v` and `const kind = v.kind` are `v.kind`), so
/// conditions on the binding narrow the reference and its siblings.
#[derive(Clone, Debug)]
pub(crate) struct BindingAlias {
    key: RefKey,
    root: Box<str>,
    prop: Box<str>,
    /// The pattern the binding is an element of (sibling narrowing reads its parent type).
    pattern: Option<Loc>,
}

/// The reference being narrowed during one walk.
struct Walk {
    key: RefKey,
    hash: u64,
    /// The root identifier's text (`this` for `this`).
    root_name: Box<str>,
    declared: TypeId,
    initial: TypeId,
    /// Narrowing continues into the flow where a function expression was created.
    cross_functions: bool,
    file: ts_r_diagnostics::FileId,
    /// The reference expression read, when the walk is for one.
    at: Option<Loc>,
}

/// Cache key of a flow type at a label: (file, flow node, reference hash, declared, initial).
pub(crate) type FlowKey = (u32, u32, u64, TypeId, TypeId);

mod facts {
    pub const STRING: u16 = 1;
    pub const NUMBER: u16 = 2;
    pub const BIGINT: u16 = 4;
    pub const BOOLEAN: u16 = 8;
    pub const SYMBOL: u16 = 16;
    pub const UNDEFINED: u16 = 32;
    pub const OBJECT: u16 = 64;
    pub const FUNCTION: u16 = 128;
    pub const ALL: u16 = 255;
    pub const NON_NULLISH: u16 = ALL & !UNDEFINED;
}

fn typeof_fact(name: &str) -> Option<u16> {
    Some(match name {
        "string" => facts::STRING,
        "number" => facts::NUMBER,
        "bigint" => facts::BIGINT,
        "boolean" => facts::BOOLEAN,
        "symbol" => facts::SYMBOL,
        "undefined" => facts::UNDEFINED,
        "object" => facts::OBJECT,
        "function" => facts::FUNCTION,
        _ => return None,
    })
}

impl<'c, 'a> Ck<'c, 'a> {
    pub fn flow_type_of_reference_quiet(&mut self, l: Loc, s: SymId, t: TypeId) -> TypeId {
        let f = self.c.sym_flags(s);
        if !f.intersects(SymbolFlags::VARIABLE) {
            return t;
        }
        // Narrowed through the closures around it like a read (`(arg: typeof a) => ...`
        // after `typeof a === "number"`).
        let cross = self.c.value_decl(s).is_some_and(|decl| {
            decl.file == l.file && {
                let is_param = matches!(self.ast(decl.file).node(decl.node), Node::Param { .. });
                let decl_container = self.flow_container(decl);
                !self.is_var_declaration(decl)
                    && ((!is_param && self.is_const_declaration(decl))
                        || !self.is_symbol_assigned(s, decl.file)
                        || self.closure_past_last_assignment(s, l, decl_container))
            }
        });
        self.quietly(|ck| ck.flow_type_of_reference_ext(l, t, t, cross))
    }

    // ---------------------------------------------------------------------------------
    // References
    // ---------------------------------------------------------------------------------

    /// The reference identity of an expression, if it is narrowable.
    pub fn ref_key(&mut self, l: Loc) -> Option<RefKey> {
        if let Some(k) = self.ref_keys.get(&l) {
            return k.clone();
        }
        let k = self.compute_ref_key(l, 0, false);
        self.ref_keys.insert(l, k.clone());
        k
    }

    /// `through_assign`: an assignment object (`(x = f()).p`) stands for its target, as
    /// when matching a condition against a reference (never for a reference's own key).
    fn compute_ref_key(&mut self, l: Loc, depth: u32, through_assign: bool) -> Option<RefKey> {
        if depth > 32 {
            return None;
        }
        let ast = self.ast(l.file);
        match *ast.node(l.node) {
            // `this` in a type query (`typeof this`) is an identifier.
            Node::Ident { name } if ast.atom(name) == "this" => Some(RefKey {
                root: RefRoot::This,
                path: Vec::new(),
            }),
            Node::Ident { .. } => {
                let s = self.ident_ref_symbol(l)?;
                Some(RefKey {
                    root: RefRoot::Sym(s),
                    path: Vec::new(),
                })
            }
            Node::This {} => Some(RefKey {
                root: RefRoot::This,
                path: Vec::new(),
            }),
            Node::Super {} => Some(RefKey {
                root: RefRoot::Super,
                path: Vec::new(),
            }),
            Node::MetaProperty {
                kind: ts_r_ast::MetaPropKind::ImportMeta,
            } => Some(RefKey {
                root: RefRoot::ImportMeta,
                path: Vec::new(),
            }),
            Node::Paren { expr } | Node::NonNull { expr } => {
                self.compute_ref_key(Loc::new(l.file, expr), depth + 1, through_assign)
            }
            Node::Assign {
                left,
                op:
                    AssignOp::Assign
                    | AssignOp::AndAssign
                    | AssignOp::OrAssign
                    | AssignOp::CoalesceAssign,
                ..
            } if through_assign => {
                self.compute_ref_key(Loc::new(l.file, left), depth + 1, through_assign)
            }
            // `(f(), x).p` is `x.p`.
            Node::Seq { exprs } => {
                let last = *ast.list(exprs).last()?;
                self.compute_ref_key(Loc::new(l.file, last), depth + 1, through_assign)
            }
            // `typeof x.p` and `typeof this.a.b` in a type name the references `x.p` and
            // `this.a.b`.
            Node::Member { obj, prop, .. }
            | Node::QualifiedName {
                left: obj,
                right: prop,
            } => {
                let name = key_text(ast, prop)?;
                let mut k =
                    self.compute_ref_key(Loc::new(l.file, obj), depth + 1, through_assign)?;
                k.path.push(name.into());
                Some(k)
            }
            Node::Index { obj, index, .. } => {
                let i = skip_parens(ast, index);
                let name: String = match ast.node(i) {
                    Node::StringLit { .. } | Node::NumberLit { .. } | Node::TemplateLit { .. } => {
                        key_text(ast, i)?
                    }
                    // `obj[E.A]`: an enum member (or other literal-typed property) key.
                    Node::Member { .. } => {
                        let il = Loc::new(l.file, i);
                        let it = self.quietly(|ck| ck.check_expr(il));
                        if !self.store().flags(it).intersects(TypeFlags::UNIT) {
                            return None;
                        }
                        self.property_name_of_type(it)?
                    }
                    // `obj[key]` with a constant `key` (a `const` or a parameter never
                    // assigned): its literal name, or the key symbol itself.
                    Node::Ident { .. } => {
                        let il = Loc::new(l.file, i);
                        let ks = self.ident_ref_symbol(il)?;
                        if !self.is_constant_key_symbol(ks) {
                            return None;
                        }
                        let it = self.quietly(|ck| ck.check_expr(il));
                        if self.store().flags(it).intersects(TypeFlags::UNIT) {
                            self.property_name_of_type(it)?
                        } else {
                            format!("\u{1}{}", ks.0)
                        }
                    }
                    _ => return None,
                };
                let mut k =
                    self.compute_ref_key(Loc::new(l.file, obj), depth + 1, through_assign)?;
                k.path.push(name.into());
                Some(k)
            }
            _ => None,
        }
    }

    /// The symbol an identifier refers to or declares (variables and parameters).
    fn ident_ref_symbol(&mut self, l: Loc) -> Option<SymId> {
        if let Some(s) = self.c.sym_of_decl(l) {
            return Some(s);
        }
        if let Some(p) = self.c.parent(l)
            && let Node::Param { pattern, .. } = *self.node(p)
            && pattern == l.node
        {
            return self.c.sym_of_decl(p);
        }
        let s = self.c.resolve_ident(l, meaning::VALUE)?;
        if self.c.sym_flags(s).intersects(SymbolFlags::ALIAS) {
            return self.c.resolve_alias(s).or(Some(s));
        }
        Some(s)
    }

    fn root_name(&self, l: Loc) -> Option<Box<str>> {
        self.root_name_of(l, false)
    }

    fn root_name_of(&self, l: Loc, through_assign: bool) -> Option<Box<str>> {
        let ast = self.ast(l.file);
        let mut n = l.node;
        for _ in 0..64 {
            match *ast.node(n) {
                Node::Ident { name } => return Some(ast.atom(name).into()),
                Node::This {} => return Some("this".into()),
                Node::Super {} => return Some("super".into()),
                Node::MetaProperty {
                    kind: ts_r_ast::MetaPropKind::ImportMeta,
                } => return Some("import.meta".into()),
                Node::Paren { expr } | Node::NonNull { expr } => n = expr,
                Node::Member { obj, .. }
                | Node::Index { obj, .. }
                | Node::QualifiedName { left: obj, .. } => n = obj,
                Node::Seq { exprs } => n = *ast.list(exprs).last()?,
                Node::Assign {
                    left,
                    op:
                        AssignOp::Assign
                        | AssignOp::AndAssign
                        | AssignOp::OrAssign
                        | AssignOp::CoalesceAssign,
                    ..
                } if through_assign => n = left,
                _ => return None,
            }
        }
        None
    }

    /// The flow node recorded at a reference.
    fn flow_of_reference(&self, e: Loc) -> Option<FlowId> {
        let flow = &self.c.bound(e.file).flow;
        if let Some(f) = flow.flow_at(e.node) {
            return Some(f);
        }
        // A qualified name in a type query (`typeof this.a.b`) flows where its leftmost
        // identifier does.
        if let Node::QualifiedName { .. } = self.node(e) {
            let ast = self.ast(e.file);
            let mut n = e.node;
            for _ in 0..64 {
                match *ast.node(n) {
                    Node::QualifiedName { left, .. } => n = left,
                    _ => return flow.flow_at(n),
                }
            }
            return None;
        }
        let p = self.c.parent(e)?;
        match *self.node(p) {
            Node::ShorthandProp { name, .. } if name == e.node => flow.flow_at(p.node),
            _ => None,
        }
    }

    // ---------------------------------------------------------------------------------
    // Entry points
    // ---------------------------------------------------------------------------------

    /// The narrowed type of reference `e` with declared type `declared`; `initial` is the
    /// type at the start of the flow container.
    pub fn flow_type_of_reference(&mut self, e: Loc, declared: TypeId, initial: TypeId) -> TypeId {
        self.flow_type_of_reference_ext(e, declared, initial, false)
    }

    fn flow_type_of_reference_ext(
        &mut self,
        e: Loc,
        declared: TypeId,
        initial: TypeId,
        cross: bool,
    ) -> TypeId {
        // The error type narrows like `any` (`isRole(missing)` makes it a `Role`).
        if declared == TypeId::UNRESOLVED {
            return declared;
        }
        let Some(flow) = self.flow_of_reference(e) else {
            return declared;
        };
        if flow.is_unreachable() {
            return declared;
        }
        let Some(key) = self.ref_key(e) else {
            return declared;
        };
        let Some(root_name) = self.root_name(e) else {
            return declared;
        };
        let mut h = FxHasher::default();
        key.hash(&mut h);
        let hash = h.finish();
        let w = Walk {
            key,
            hash,
            root_name,
            declared,
            initial,
            cross_functions: cross,
            file: e.file,
            at: Some(e),
        };
        // Temporary results computed under loop assumptions are redone per reference;
        // speculative ones are kept while the speculative assumptions stay the same.
        if self.flow_depth == 0 {
            if self.flow_loop > 0 {
                self.clear_flow_temp(0);
            } else if self.speculative > 0 && self.flow_temp_epoch != self.spec_epoch {
                self.clear_flow_temp(0);
                self.flow_temp_epoch = self.spec_epoch;
            }
        }
        let t = self.walk_flow(&w, flow);
        if t == TypeId::NEVER && declared != TypeId::NEVER && self.is_unreachable_result(&w, flow) {
            return declared;
        }
        // `x!` where `x` narrowed to only `null`/`undefined` (`x = undefined; x!.y`) reads
        // the declared type.
        if t != TypeId::NEVER
            && self
                .c
                .parent(e)
                .is_some_and(|p| matches!(self.node(p), Node::NonNull { .. }))
            && self.non_nullable(t) == TypeId::NEVER
        {
            return declared;
        }
        t
    }

    /// The type of the object binding element `prop` (a `BindingProperty`) of an
    /// unannotated variable initialized by a reference: the narrowed type of the property
    /// read from that reference where the variable is declared (`g.a = g.a || 0;
    /// const { a } = g` gives `number`). Other bindings keep `declared`.
    pub(crate) fn destructured_flow_type(&mut self, prop: Loc, declared: TypeId) -> TypeId {
        if declared == TypeId::UNRESOLVED || self.store().flags(declared).intersects(TypeFlags::ANY)
        {
            return declared;
        }
        let file = prop.file;
        let ast = self.ast(file);
        let mut names: Vec<Box<str>> = Vec::new();
        let mut cur = prop;
        let init = loop {
            if names.len() > 32 {
                return declared;
            }
            let Node::BindingProperty { key, .. } = *ast.node(cur.node) else {
                return declared;
            };
            let Some(name) = key_text(ast, key) else {
                return declared;
            };
            names.push(name.into());
            let Some(obj_pat) = self.c.parent(cur) else {
                return declared;
            };
            if !matches!(ast.node(obj_pat.node), Node::ObjectPattern { .. }) {
                return declared;
            }
            let Some(up) = self.c.parent(obj_pat) else {
                return declared;
            };
            match *ast.node(up.node) {
                Node::VarDeclarator {
                    name,
                    init: Some(i),
                    type_ann: None,
                    ..
                } if name == obj_pat.node => break i,
                Node::BindingProperty { value: Some(v), .. } if v == obj_pat.node => cur = up,
                _ => return declared,
            }
        };
        let init = Loc::new(file, skip_parens(ast, init));
        names.reverse();
        self.narrowed_path_type(init, names, declared)
    }

    /// The narrowed type of `init.<path...>` (`init` a reference: an identifier, a property
    /// access or `this`) at `init`'s position, starting from `declared`
    /// (`if (o[0]) [a] = o` reads `o[0]` narrowed).
    pub(crate) fn narrowed_path_type(
        &mut self,
        init: Loc,
        names: Vec<Box<str>>,
        declared: TypeId,
    ) -> TypeId {
        if declared == TypeId::UNRESOLVED || self.store().flags(declared).intersects(TypeFlags::ANY)
        {
            return declared;
        }
        let file = init.file;
        let ast = self.ast(file);
        let init = Loc::new(file, skip_parens(ast, init.node));
        if !matches!(
            ast.node(init.node),
            Node::Ident { .. } | Node::Member { .. } | Node::This {}
        ) {
            return declared;
        }
        let Some(flow) = self.flow_of_reference(init) else {
            return declared;
        };
        if flow.is_unreachable() {
            return declared;
        }
        let Some(mut key) = self.ref_key(init) else {
            return declared;
        };
        let Some(root_name) = self.root_name(init) else {
            return declared;
        };
        key.path.extend(names);
        let mut h = FxHasher::default();
        key.hash(&mut h);
        let w = Walk {
            key,
            hash: h.finish(),
            root_name,
            declared,
            initial: declared,
            cross_functions: false,
            file,
            at: None,
        };
        let t = self.walk_flow(&w, flow);
        if t == TypeId::NEVER && declared != TypeId::NEVER {
            return declared;
        }
        t
    }

    /// A reference whose every path is unreachable keeps its declared type (tsc reports
    /// errors in unreachable code against declared types).
    /// (Only `never`-returning calls and unreachable nodes count: a switch's dead implicit
    /// `default` narrows to `never` as tsc does.)
    fn is_unreachable_result(&mut self, w: &Walk, flow: FlowId) -> bool {
        !self.flow_reachable_with(w.file, flow, false, w.cross_functions)
    }

    /// The type of `this.<name>` where the constructor function `ctor` (its `FunctionExpr`)
    /// returns, starting from `undefined`: the type of an unannotated, uninitialised property
    /// declaration under `noImplicitAny`. `None` when it is never assigned there or only
    /// assigned `null`/`undefined`.
    pub(crate) fn property_type_in_constructor(
        &mut self,
        file: ts_r_diagnostics::FileId,
        ctor: NodeId,
        name: &str,
    ) -> Option<TypeId> {
        let ff = *self.c.bound(file).flow.function_flow(ctor)?;
        let key = RefKey {
            root: RefRoot::This,
            path: vec![name.into()],
        };
        let mut h = FxHasher::default();
        key.hash(&mut h);
        let w = Walk {
            key,
            hash: h.finish(),
            root_name: "this".into(),
            declared: TypeId::AUTO,
            initial: TypeId::UNDEFINED,
            cross_functions: false,
            file,
            at: None,
        };
        if self.flow_depth == 0 && self.flow_loop > 0 {
            self.clear_flow_temp(0);
        }
        let mut parts = Vec::new();
        for f in [ff.end, ff.returns] {
            if !f.is_unreachable() {
                let t = self.walk_flow(&w, f);
                parts.push(t);
            }
        }
        if parts.is_empty() {
            return None;
        }
        let t = self.union(&parts);
        if t == TypeId::AUTO
            || self.every_member(t, |m| {
                m == TypeId::AUTO
                    || self
                        .store()
                        .flags(m)
                        .intersects(TypeFlags::NULLABLE | TypeFlags::VOID | TypeFlags::NEVER)
            })
        {
            return None;
        }
        Some(t)
    }

    /// Narrowing of an identifier that refers to a variable or parameter, with the
    /// "used before being assigned" check.
    pub fn narrowed_identifier_type(
        &mut self,
        e: Loc,
        sym: SymId,
        declared: TypeId,
        name: &str,
    ) -> TypeId {
        if declared == TypeId::UNRESOLVED {
            return declared;
        }
        let strict = self.c.opts.strict_null_checks;
        let is_auto = declared == TypeId::AUTO;
        if is_auto && !self.c.opts.no_implicit_any {
            return TypeId::ANY;
        }
        // A computed member name of an interface or type literal is not a read.
        if !is_auto && self.in_type_member_name(e) {
            return declared;
        }
        // A plain assignment target is not a read.
        if let Some(p) = self.c.parent(e)
            && let Node::Assign {
                left,
                op: AssignOp::Assign,
                ..
            } = *self.node(p)
            && left == e.node
        {
            return if is_auto { TypeId::ANY } else { declared };
        }
        // Nor is the variable of `for (x in o)` / `for (x of xs)`.
        if !is_auto
            && let Some(p) = self.c.parent(e)
            && let Node::ForIn { left, .. } | Node::ForOf { left, .. } = *self.node(p)
            && left == e.node
        {
            return declared;
        }
        let Some(decl) = self.c.value_decl(sym) else {
            return declared;
        };
        let declared = self.narrowable_type_for_reference(e, declared);
        // A binding of a destructured union, narrowed by its siblings.
        let declared = self.destructured_binding_type(e, sym).unwrap_or(declared);
        let decl_ast = self.ast(decl.file);
        let is_param = matches!(decl_ast.node(decl.node), Node::Param { .. });
        let is_const = !is_param && self.is_const_declaration(decl);
        let same_file = decl.file == e.file;
        let ref_container = self.flow_container(e);
        let decl_container = if same_file {
            self.flow_container(decl)
        } else {
            None
        };
        let is_outer = !same_file || ref_container != decl_container;
        // A `var` is never narrowed inside closures (observed with tsc 7).
        let cross = same_file
            && !self.is_var_declaration(decl)
            && (is_const
                || !self.is_symbol_assigned(sym, decl.file)
                || (is_outer && self.closure_past_last_assignment(sym, e, decl_container)));
        // An outer `let` without initializer that is never assigned anywhere (compound
        // assignments like `x++` don't count) is still reported when read from a closure.
        let never_assigned_let = is_outer
            && same_file
            && self.is_local_let_declaration(decl)
            && self.is_uninitialized_variable(decl)
            && !self.is_symbol_assigned_definitely(sym, decl.file);
        let assume_init = is_param
            || is_outer && !never_assigned_let
            || !strict && !is_auto
            || self.is_ambient_node(decl)
            || !self.is_flow_assigned_variable(decl)
            || (!is_auto
                && self
                    .store()
                    .flags(declared)
                    .intersects(TypeFlags::ANY_OR_UNKNOWN | TypeFlags::VOID))
            || self.is_non_null_operand(e);
        if !is_auto && self.is_evolving_array_variable(decl) && self.is_unresolved_array(declared) {
            if self.is_evolving_array_operation_target(e) {
                return declared;
            }
            if is_outer {
                self.report_implicit_any_evolving(e, decl, name, "any[]");
                return self.create_array(TypeId::ANY, false);
            }
            return match self.evolving_array_read_type(e, sym, decl) {
                Some((t, true)) => {
                    self.report_implicit_any_evolving(e, decl, name, "any[]");
                    t
                }
                Some((t, false)) => t,
                None => declared,
            };
        }
        // An auto variable assigned `[]` evolves like `let xs = []` from there on.
        if is_auto
            && !is_outer
            && same_file
            && !self.c.file(decl.file).is_js()
            && self.is_assigned_empty_array(sym, decl.file)
            && let Some((t, implicit, evolving, only_evolving)) =
                self.evolving_auto_read(e, sym, decl)
            && evolving
        {
            // `x.push(..)` on an array still evolving on every path keeps it silent; joined
            // with other values it is the union (`boolean[] | number[]` takes `never`).
            if self.is_evolving_array_operation_target(e) {
                return if only_evolving {
                    self.create_array(TypeId::UNRESOLVED, false)
                } else {
                    t
                };
            }
            if implicit {
                self.report_implicit_any_evolving(e, decl, name, "any[]");
            }
            return t;
        }
        if is_auto {
            if is_outer {
                if !same_file {
                    return TypeId::ANY;
                }
                // A closure created after every assignment sees the type at its creation;
                // the variable's type is undeterminable only where `undefined` survives.
                if self.is_symbol_assigned(sym, decl.file)
                    && self.closure_past_last_assignment(sym, e, decl_container)
                {
                    let t = self.flow_type_of_reference_ext(e, declared, TypeId::UNDEFINED, true);
                    if t == TypeId::NEVER || t == TypeId::AUTO {
                        return TypeId::ANY;
                    }
                    if !self.contains_undefined(t) {
                        return t;
                    }
                    self.report_implicit_any_evolving(e, decl, name, "any");
                    return TypeId::ANY;
                }
                // Never assigned: an outer `let` reads as `undefined` quietly, a `var`
                // cannot be determined.
                if self.is_symbol_assigned(sym, decl.file) || !self.is_local_let_declaration(decl) {
                    self.report_implicit_any_evolving(e, decl, name, "any");
                }
                return TypeId::ANY;
            }
            // Not assigned on the way here: `undefined` (`let x; x.a` is TS18048).
            let t = self.flow_type_of_reference_ext(e, declared, TypeId::UNDEFINED, cross);
            if t == TypeId::NEVER || t == TypeId::AUTO {
                return TypeId::ANY;
            }
            return t;
        }
        let initial = if assume_init {
            declared
        } else {
            self.add_undefined_plain(declared)
        };
        let t = self.flow_type_of_reference_ext(e, declared, initial, cross);
        if !assume_init && self.contains_undefined(t) && !self.contains_undefined(declared) {
            // `x ??= v` and friends read `x` without TS2454.
            if self.is_logical_assignment_target(e) {
                return t;
            }
            // tsc reports TS7022 for `var a = { f: a }` (when its type is circular) and
            // TS2304 for a body variable
            // read in a parameter initializer instead.
            // Nor is an imported name (a module importing its own variable) checked.
            if (self.reads_own_unannotated_initializer(e, decl)
                && self
                    .circular
                    .contains(&crate::checker::Resolving::Type(sym)))
                || self.in_parameter_initializer(e)
                || self.in_member_decorator(e)
                || self.c.resolve_ident(e, meaning::VALUE) != Some(sym)
            {
                return declared;
            }
            // The name as declared (`var xx` prints so).
            let shown = match *decl_ast.node(decl.node) {
                Node::Ident { .. } => self.c.text(decl),
                _ => name,
            };
            self.error(
                e,
                dg::VARIABLE_0_IS_USED_BEFORE_BEING_ASSIGNED_2454,
                &[shown],
            );
            return declared;
        }
        // The object of a property or element access narrows through the union constraint
        // of a generic type (`T[K]` with `K extends "a"` and `{ a: X | null }`).
        self.narrowable_type_for_reference(e, t)
    }

    /// `t` with each generic member whose base constraint is a union replaced by that
    /// constraint (and narrowed generic intersections such as `T[K] & {}` by theirs), when
    /// `e` is the object of a property or element access.
    pub(crate) fn narrowable_type_for_reference(&mut self, e: Loc, t: TypeId) -> TypeId {
        let Some(p) = self.c.parent(e) else {
            return t;
        };
        let is_object = match *self.node(p) {
            Node::Member { obj, .. } => obj == e.node,
            // Not when the key is generic (`values[index]` with `index: K` stays `T[K]`).
            Node::Index { obj, index, .. } => {
                obj == e.node && {
                    let it = self.check_expr(Loc::new(e.file, index));
                    !self.is_generic_index_type(it)
                }
            }
            Node::Call { callee, .. } | Node::New { callee, .. } => callee == e.node,
            _ => false,
        };
        // Also a reference whose contextual type has no generic types (an argument for a
        // non-generic parameter, an initializer of an annotated variable): `x` with
        // `T extends A | A[]` passed to `(x: A) => ...` after `!Array.isArray(x)`.
        if !is_object {
            if !self.has_union_constrained_member(t) {
                return t;
            }
            match self.contextual_type(e) {
                Some(ct) if !self.is_generic_type(ct) => {}
                _ => return t,
            }
        }
        let st = self.store();
        let members: Vec<TypeId> = if st.flags(t).contains(TypeFlags::UNION) {
            st.members_of_union(t).to_vec()
        } else {
            vec![t]
        };
        let mut changed = false;
        let mut out = Vec::with_capacity(members.len());
        for m in members {
            let mf = self.store().flags(m);
            let generic_intersection =
                mf.contains(TypeFlags::INTERSECTION) && self.is_generic_type(m);
            if mf.intersects(TypeFlags::INSTANTIABLE) || generic_intersection {
                let c = self.base_constraint(m);
                let cf = self.store().flags(c);
                if c != TypeId::UNRESOLVED
                    && c != TypeId::UNKNOWN
                    && (cf.contains(TypeFlags::UNION)
                        || (generic_intersection
                            && !cf.intersects(TypeFlags::INTERSECTION | TypeFlags::NEVER)))
                {
                    out.push(c);
                    changed = true;
                    continue;
                }
            }
            out.push(m);
        }
        if !changed {
            return t;
        }
        self.union(&out)
    }

    /// Whether `t` or one of its union members is generic with a union base constraint
    /// (`T extends A | A[]`).
    pub(crate) fn has_union_constrained_member(&mut self, t: TypeId) -> bool {
        if !self.some_member(t, |m| {
            self.store().flags(m).intersects(TypeFlags::INSTANTIABLE)
        }) {
            return false;
        }
        self.union_members(t).into_iter().any(|m| {
            self.store().flags(m).intersects(TypeFlags::INSTANTIABLE) && {
                let c = self.base_constraint(m);
                c != TypeId::UNRESOLVED && self.store().flags(c).contains(TypeFlags::UNION)
            }
        })
    }

    /// TS7005 at a read of an auto-typed variable whose type cannot be determined there
    /// (a read from a nested function, or of an evolving array nothing was pushed to), and
    /// TS7034 at its declaration.
    fn report_implicit_any_evolving(&mut self, e: Loc, decl: Loc, name: &str, ty: &str) {
        if !self.c.opts.no_implicit_any || self.c.file(e.file).is_js() {
            return;
        }
        self.error(e, dg::VARIABLE_0_IMPLICITLY_HAS_AN_1_TYPE_7005, &[name, ty]);
        if decl.file == e.file {
            self.error(
                decl,
                dg::VARIABLE_0_IMPLICITLY_HAS_TYPE_1_IN_SOME_LOCATIONS_WHERE_ITS_TYPE_CANNOT_BE_DETE_7034,
                &[name, ty],
            );
        }
    }

    /// `UNRESOLVED[]`, the declared type of `let xs = []`.
    fn is_unresolved_array(&self, t: TypeId) -> bool {
        self.c.is_array_type(t) && self.c.type_args_of(t).first() == Some(&TypeId::UNRESOLVED)
    }

    fn add_undefined_plain(&mut self, t: TypeId) -> TypeId {
        self.union2(t, TypeId::UNDEFINED)
    }

    pub(crate) fn contains_undefined(&self, t: TypeId) -> bool {
        let s = self.store();
        if s.flags(t).contains(TypeFlags::UNION) {
            s.members_of_union(t)
                .iter()
                .any(|&m| s.flags(m).contains(TypeFlags::UNDEFINED))
        } else {
            s.flags(t).contains(TypeFlags::UNDEFINED)
        }
    }

    fn is_non_null_operand(&self, e: Loc) -> bool {
        matches!(
            self.c.parent(e).map(|p| *self.node(p)),
            Some(Node::NonNull { .. })
        )
    }

    /// `let x;` / `let x: T;` (not a `for-in`/`for-of` variable, no `!`).
    /// Whether `e` is inside a computed member name of an interface or type literal.
    fn in_type_member_name(&self, e: Loc) -> bool {
        let ast = self.ast(e.file);
        let parents = self.c.file(e.file).parents();
        let mut cur = parents.parent(e.node);
        let mut in_key = false;
        for _ in 0..64 {
            let Some(n) = cur else { break };
            match *ast.node(n) {
                Node::ComputedKey { .. } => in_key = true,
                Node::Interface { .. } | Node::TypeLiteral { .. } => return in_key,
                Node::FunctionDecl { .. }
                | Node::FunctionExpr { .. }
                | Node::Arrow { .. }
                | Node::ClassDecl { .. }
                | Node::ClassExpr { .. }
                | Node::ObjectLit { .. }
                | Node::TypeQuery { .. } => return false,
                _ => {}
            }
            cur = parents.parent(n);
        }
        false
    }

    /// Whether `decl` is the name of a `var` declarator (or within its binding pattern): a
    /// closure never sees such a variable narrowed (observed, unlike `let` and parameters).
    fn is_var_declaration(&self, decl: Loc) -> bool {
        let mut cur = decl;
        for _ in 0..64 {
            let Some(p) = self.c.parent(cur) else {
                return false;
            };
            match *self.node(p) {
                Node::VarDecl { kind, .. } => return kind == ts_r_ast::VarKind::Var,
                Node::VarDeclarator { .. }
                | Node::ObjectPattern { .. }
                | Node::ArrayPattern { .. }
                | Node::BindingProperty { .. }
                | Node::AssignPattern { .. }
                | Node::Rest { .. } => cur = p,
                _ => return false,
            }
        }
        false
    }

    /// Whether `decl` is the name of a local `let` declarator: not exported and not a
    /// top-level declaration of a script (whose names are global).
    fn is_local_let_declaration(&self, decl: Loc) -> bool {
        let Some(p) = self.c.parent(decl) else {
            return false;
        };
        let Some(list) = self.c.parent(p) else {
            return false;
        };
        let Node::VarDecl {
            kind: ts_r_ast::VarKind::Let,
            modifiers,
            ..
        } = *self.node(list)
        else {
            return false;
        };
        if modifiers.contains(ts_r_ast::Modifiers::EXPORT) {
            return false;
        }
        let script_top = matches!(
            self.c.parent(list).map(|s| *self.node(s)),
            Some(Node::Program { .. })
        ) && !self.c.bound(decl.file).is_module;
        !script_top
    }

    /// Whether `decl` (a binding name) belongs to a `var`/`let`/`const` declaration whose
    /// assignment the flow graph tracks: any declarator (initialized or not, destructured,
    /// a `for...in`/`for...of` head) except `x!: T`. Reads that flow can reach without
    /// passing its assignment are TS2454 (`v1; let v1 = 0;`, `a` after a `try` that
    /// assigns it).
    fn is_flow_assigned_variable(&self, decl: Loc) -> bool {
        let ast = self.ast(decl.file);
        let mut n = decl;
        for _ in 0..256 {
            let Some(p) = self.c.parent(n) else {
                return false;
            };
            match *ast.node(p.node) {
                Node::ObjectPattern { .. }
                | Node::ArrayPattern { .. }
                | Node::BindingProperty { .. }
                | Node::AssignPattern { .. }
                | Node::Rest { .. } => n = p,
                Node::VarDeclarator { definite, .. } => return !definite,
                _ => return false,
            }
        }
        false
    }

    fn is_uninitialized_variable(&self, decl: Loc) -> bool {
        let Some(p) = self.c.parent(decl) else {
            return false;
        };
        let ast = self.ast(decl.file);
        let Node::VarDeclarator {
            name,
            init: None,
            definite: false,
            ..
        } = *ast.node(p.node)
        else {
            return false;
        };
        if name != decl.node {
            return false;
        }
        let Some(list) = self.c.parent(p) else {
            return false;
        };
        match self.c.parent(list).map(|s| *self.node(s)) {
            Some(Node::ForIn { left, .. } | Node::ForOf { left, .. }) => left != list.node,
            _ => true,
        }
    }

    /// The node whose flow `Start` begins the container of `l` (`None` for the file).
    pub fn flow_container(&self, l: Loc) -> Option<NodeId> {
        let ast = self.ast(l.file);
        let parents = self.c.file(l.file).parents();
        let mut prev = l.node;
        let mut cur = parents.parent(l.node);
        while let Some(n) = cur {
            match *ast.node(n) {
                // An immediately invoked function runs inline (the binder flows through it
                // unless it is async or a generator, whose flow starts afresh).
                Node::FunctionExpr { .. } | Node::Arrow { .. }
                    if self.is_called_where_written(Loc::new(l.file, n)) => {}
                Node::FunctionDecl { .. }
                | Node::FunctionExpr { .. }
                | Node::Arrow { .. }
                // A namespace body: its code runs once, when the namespace is evaluated.
                | Node::ModuleBlock { .. } => {
                    return Some(n);
                }
                Node::PropertyDef { value: Some(v), .. } if v == prev => return Some(n),
                // A class property's or a method's computed name belongs to the member.
                Node::PropertyDef { key, .. } | Node::MethodDef { key, .. } if key == prev => {
                    return Some(n);
                }
                _ => {}
            }
            prev = n;
            cur = parents.parent(n);
        }
        None
    }

    /// Whether a variable or parameter is assigned anywhere after its declaration.
    fn is_symbol_assigned(&mut self, s: SymId, file: ts_r_diagnostics::FileId) -> bool {
        if !self.assigned_syms_files.contains(&file) {
            self.assigned_syms_files.insert(file);
            let flow = &self.c.bound(file).flow;
            let ast = self.ast(file);
            let mut found = Vec::new();
            for n in &flow.nodes {
                if n.kind != FlowKind::Assignment {
                    continue;
                }
                let Some(t) = n.node else { continue };
                if !matches!(ast.node(t), Node::Ident { .. }) {
                    continue;
                }
                let l = Loc::new(file, t);
                let is_decl = match self.c.parent(l).map(|p| *self.node(p)) {
                    Some(Node::VarDeclarator { name, .. }) => name == t,
                    Some(Node::Param { .. } | Node::CatchClause { .. }) => true,
                    Some(
                        Node::BindingProperty { .. }
                        | Node::ArrayPattern { .. }
                        | Node::ObjectPattern { .. }
                        | Node::AssignPattern { .. }
                        | Node::Rest { .. },
                    ) => self.c.sym_of_decl(l).is_some(),
                    _ => false,
                };
                if is_decl {
                    continue;
                }
                if let Some(sym) = self.c.resolve_ident(l, meaning::VALUE) {
                    if let Some(p) = self.c.parent(l)
                        && let Node::Assign {
                            left,
                            op: AssignOp::Assign,
                            right,
                        } = *self.node(p)
                        && left == t
                        && let Node::ArrayLit { elements } = *ast.node(skip_parens(ast, right))
                        && ast.list(elements).is_empty()
                    {
                        self.empty_array_assigned_syms.insert(sym);
                    }
                    let definite = !self.is_compound_assignment_target(l);
                    found.push((sym, ast.span(t).start, t, definite));
                }
            }
            for (sym, mut at, node, definite) in found {
                self.assigned_syms.insert(sym);
                if definite {
                    self.definitely_assigned_syms.insert(sym);
                }
                // An assignment in a function nested in the declaring one could run at any
                // time: no closure is past the last assignment.
                if let Some(d) = self.c.value_decl(sym)
                    && d.file == file
                    && self.flow_container(Loc::new(file, node)) != self.flow_container(d)
                {
                    at = u32::MAX;
                }
                let e = self.last_assignment.entry(sym).or_insert(at);
                *e = (*e).max(at);
            }
        }
        self.assigned_syms.contains(&s)
    }

    /// Whether `s` has a plain `s = []` assignment in `file`.
    pub(crate) fn is_assigned_empty_array(
        &mut self,
        s: SymId,
        file: ts_r_diagnostics::FileId,
    ) -> bool {
        self.is_symbol_assigned(s, file) && self.empty_array_assigned_syms.contains(&s)
    }

    /// Whether `s` (a local of `file`) has an assignment anywhere that is not compound:
    /// `x++` or `x |= 1` reads `x` first, so it does not initialize it.
    fn is_symbol_assigned_definitely(&mut self, s: SymId, file: ts_r_diagnostics::FileId) -> bool {
        self.is_symbol_assigned(s, file) && self.definitely_assigned_syms.contains(&s)
    }

    /// Whether the reference `e` is in a function expression or arrow (inside the
    /// declaring container `decl_container`) created after every assignment to `s`: the
    /// closure keeps the narrowing at its creation (`if (!x) x = f(); use(() => x.a)`).
    fn closure_past_last_assignment(
        &mut self,
        s: SymId,
        e: Loc,
        decl_container: Option<NodeId>,
    ) -> bool {
        let ast = self.ast(e.file);
        let mut cur = e;
        let mut outermost = None;
        for _ in 0..64 {
            let Some(c) = self.flow_container(cur) else {
                break;
            };
            if Some(c) == decl_container {
                break;
            }
            if !matches!(ast.node(c), Node::FunctionExpr { .. } | Node::Arrow { .. }) {
                return false;
            }
            outermost = Some(c);
            cur = Loc::new(e.file, c);
        }
        let Some(closure) = outermost else {
            return false;
        };
        let start = ast.span(closure).start;
        self.last_assignment.get(&s).is_some_and(|&at| at < start)
    }

    // ---------------------------------------------------------------------------------
    // The walk
    // ---------------------------------------------------------------------------------

    fn walk_flow(&mut self, w: &Walk, start: FlowId) -> TypeId {
        if self.flow_depth >= MAX_FLOW_DEPTH {
            return w.declared;
        }
        self.flow_depth += 1;
        let t = self.walk_flow_inner(w, start);
        self.flow_depth -= 1;
        t
    }

    fn walk_flow_inner(&mut self, w: &Walk, start: FlowId) -> TypeId {
        let file = w.file;
        let mut flow = start;
        let mut steps = 0u32;
        loop {
            steps += 1;
            if steps > 1_000_000 {
                return w.declared;
            }
            let graph = &self.c.bound(file).flow;
            let Some(node) = graph.nodes.get(flow.index()).copied() else {
                return w.declared;
            };
            match node.kind {
                FlowKind::Unreachable => return TypeId::NEVER,
                FlowKind::Start => {
                    // An immediately invoked function runs inline: the flow before the
                    // call continues through it.
                    // A bare `this` read in an arrow function is the enclosing `this`: its
                    // narrowing continues out of the arrow (`this.p` does not; observed).
                    if node.a != 0
                        && (w.cross_functions
                            || node.node.is_some_and(|f| self.is_iife(Loc::new(file, f)))
                            || (w.key.root == RefRoot::This
                                && w.key.path.is_empty()
                                && node.node.is_some_and(|f| {
                                    matches!(self.ast(file).node(f), Node::Arrow { .. })
                                })))
                    {
                        flow = FlowId(node.a);
                        continue;
                    }
                    return w.initial;
                }
                FlowKind::Assignment => {
                    let Some(target) = node.node else {
                        flow = FlowId(node.a);
                        continue;
                    };
                    match self.assignment_effect(w, Loc::new(file, target)) {
                        AssignEffect::None => {
                            // In a `for (k in obj)` body, `obj` is not nullish, nor is
                            // anything an optional chain `obj` ends tests (`a?.b`: `a`).
                            if let Some(right) = self.for_in_object_of_head(Loc::new(file, target))
                                && (self.is_ref(w, right)
                                    || self.optional_chain_contains_ref_eq(w, right))
                            {
                                let t = self.walk_flow(w, FlowId(node.a));
                                return self.filter_type(t, |ck, m| !ck.is_nullish_member(m));
                            }
                            flow = FlowId(node.a);
                        }
                        AssignEffect::Assigned(t) => return t,
                        AssignEffect::Reset => return w.declared,
                    }
                }
                FlowKind::TrueCondition | FlowKind::FalseCondition => {
                    let Some(cond) = node.node else {
                        flow = FlowId(node.a);
                        continue;
                    };
                    let cl = Loc::new(file, cond);
                    if !self.condition_mentions(cl, &w.root_name) {
                        flow = FlowId(node.a);
                        continue;
                    }
                    let t = self.walk_flow(w, FlowId(node.a));
                    if t == TypeId::NEVER || t == TypeId::SILENT_NEVER {
                        return t;
                    }
                    let assume_true = node.kind == FlowKind::TrueCondition;
                    let r = if node.flags & flow_flags::NULLISH != 0 {
                        self.narrow_nullish(w, t, cl, assume_true)
                    } else {
                        self.narrow(w, t, cl, assume_true, 0)
                    };
                    // Narrowing a type that is still being computed in a loop to `never`
                    // only means the path is not reached yet: the silent `never` reports
                    // nothing when used and adds nothing to the loop's type.
                    if r == TypeId::NEVER && self.flow_loop > 0 {
                        return TypeId::SILENT_NEVER;
                    }
                    return r;
                }
                FlowKind::SwitchClause => {
                    let Some(sw) = node.node else {
                        flow = FlowId(node.a);
                        continue;
                    };
                    let sl = Loc::new(file, sw);
                    let Node::Switch { disc, cases } = *self.node(sl) else {
                        flow = FlowId(node.a);
                        continue;
                    };
                    let start = (node.b & 0xFFFF) as usize;
                    let end = (node.b >> 16) as usize;
                    // `switch (true)`: each clause test is a condition.
                    let ast = self.ast(file);
                    if matches!(
                        *ast.node(skip_parens(ast, disc)),
                        Node::BoolLit { value: true }
                    ) {
                        let tests: Vec<NodeId> = ast
                            .list(cases)
                            .iter()
                            .filter_map(|&c| match *ast.node(c) {
                                Node::SwitchCase { test, .. } => test,
                                _ => None,
                            })
                            .collect();
                        if !tests
                            .iter()
                            .any(|&x| self.condition_mentions(Loc::new(file, x), &w.root_name))
                        {
                            flow = FlowId(node.a);
                            continue;
                        }
                        let t = self.walk_flow(w, FlowId(node.a));
                        if t == TypeId::NEVER {
                            return t;
                        }
                        return self.narrow_switch_true(w, t, sl, start, end);
                    }
                    if !self.condition_mentions(Loc::new(file, disc), &w.root_name) {
                        flow = FlowId(node.a);
                        continue;
                    }
                    let t = self.walk_flow(w, FlowId(node.a));
                    if t == TypeId::NEVER {
                        return t;
                    }
                    return self.narrow_switch(w, t, sl, start, end);
                }
                FlowKind::BranchLabel => {
                    let key: FlowKey = (file.0, flow.0, w.hash, w.declared, w.initial);
                    if let Some(t) = self.flow_cached(&key) {
                        return t;
                    }
                    let antes: Vec<FlowId> =
                        match self.flow_reduce.iter().rev().find(|x| x.0 == flow) {
                            Some((_, list)) => list.clone(),
                            None => graph.antecedents(flow).to_vec(),
                        };
                    if antes.len() == 1 {
                        flow = antes[0];
                        continue;
                    }
                    let mut types = Vec::with_capacity(antes.len());
                    // The implicit `default` of a switch joined with other paths adds
                    // nothing when the switch covers every value of its discriminant
                    // (`clazz` assigned in each case is definitely assigned after it).
                    // As the only path it passes its type through.
                    let mut bypass = None;
                    let mut others = 0;
                    for a in antes {
                        if a.is_unreachable() {
                            continue;
                        }
                        if bypass.is_none() && self.is_implicit_default(file, a) {
                            bypass = Some(a);
                            continue;
                        }
                        others += 1;
                        let t = self.walk_flow(w, a);
                        if t != TypeId::NEVER && t != TypeId::SILENT_NEVER && !types.contains(&t) {
                            types.push(t);
                        }
                    }
                    // The exhaustiveness check (prone to circularity) runs only when the
                    // bypass adds a type.
                    if let Some(a) = bypass {
                        let t = self.walk_flow(w, a);
                        if t != TypeId::NEVER
                            && t != TypeId::SILENT_NEVER
                            && !types.contains(&t)
                            && !(others > 0
                                && graph_switch(self, file, a)
                                    .is_some_and(|sw| self.is_exhaustive_switch(sw)))
                        {
                            types.push(t);
                        }
                    }
                    let t = self.flow_union(w, &types);
                    self.flow_store(key, t);
                    return t;
                }
                FlowKind::LoopLabel => return self.walk_loop(w, flow),
                FlowKind::Call => {
                    let Some(call) = node.node else {
                        flow = FlowId(node.a);
                        continue;
                    };
                    match self.call_effect(w, Loc::new(file, call)) {
                        CallEffect::None => flow = FlowId(node.a),
                        CallEffect::Never => return TypeId::NEVER,
                        CallEffect::Narrow(f) => {
                            let t = self.walk_flow(w, FlowId(node.a));
                            if t == TypeId::NEVER {
                                return t;
                            }
                            return f.apply(self, w, t);
                        }
                    }
                }
                FlowKind::ArrayMutation => flow = FlowId(node.a),
                FlowKind::ReduceLabel => {
                    let Some((target, allowed)) = graph.reduce_target(flow) else {
                        flow = FlowId(node.a);
                        continue;
                    };
                    let allowed = allowed.to_vec();
                    self.flow_reduce.push((target, allowed));
                    let t = self.walk_flow(w, FlowId(node.a));
                    self.flow_reduce.pop();
                    return t;
                }
            }
        }
    }

    fn flow_union(&mut self, w: &Walk, types: &[TypeId]) -> TypeId {
        match types {
            [] => TypeId::NEVER,
            [t] => *t,
            _ => {
                if types.contains(&w.declared) {
                    // The declared type absorbs its narrowings.
                    let all_sub = types
                        .iter()
                        .all(|&t| t == w.declared || self.is_subtype_of_declared(t, w.declared));
                    if all_sub {
                        return w.declared;
                    }
                }
                // A branch type that is not a subset of the declared type (an `in`
                // intersection, a narrowed `unknown`) makes the join subtype-reduce:
                // `object | (object & Record<"x", unknown>)` joins to `object` (tsc 7).
                let reduction = if types.iter().all(|&t| self.is_type_subset_of(t, w.declared)) {
                    crate::unions::Reduction::Literal
                } else {
                    crate::unions::Reduction::Subtype
                };
                let u = self.union_with(types, reduction, ts_r_types::AliasId::NONE);
                // Narrowings of `unknown` that join back to `{} | null | undefined` are
                // `unknown` again.
                if w.declared == TypeId::UNKNOWN && self.is_unknown_like_union(u) {
                    return TypeId::UNKNOWN;
                }
                // Narrowings that join back to every declared member are the declared type
                // (its alias prints).
                self.same_union_or(w.declared, u)
            }
        }
    }

    /// A union with `null`, `undefined` and `{}` among its members.
    pub(crate) fn is_unknown_like_union(&self, t: TypeId) -> bool {
        let s = self.store();
        if !s.flags(t).contains(TypeFlags::UNION) {
            return false;
        }
        let ms = s.members_of_union(t);
        ms.contains(&TypeId::NULL)
            && ms.contains(&TypeId::UNDEFINED)
            && ms.iter().any(|&m| self.is_empty_anonymous_object(m))
    }

    /// `t` is `target` or made only of members of the union `target`.
    fn is_type_subset_of(&self, t: TypeId, target: TypeId) -> bool {
        t == target || self.is_subtype_of_declared(t, target)
    }

    fn is_subtype_of_declared(&self, t: TypeId, declared: TypeId) -> bool {
        let s = self.store();
        if s.flags(declared).contains(TypeFlags::UNION) {
            let dm = s.members_of_union(declared);
            if s.flags(t).contains(TypeFlags::UNION) {
                return s.members_of_union(t).iter().all(|m| dm.contains(m));
            }
            return dm.contains(&t);
        }
        false
    }

    /// Forgets results computed under the current loop assumptions.
    /// Forgets temporary results computed at loop nesting level `level` or deeper.
    pub(crate) fn clear_flow_temp(&mut self, level: u32) {
        if level == 0 {
            self.flow_temp.clear();
            self.loop_node_types.clear();
        } else {
            self.flow_temp.retain(|_, v| v.1 < level);
            self.loop_node_types.retain(|_, v| v.1 < level);
        }
    }

    fn flow_cached(&self, key: &FlowKey) -> Option<TypeId> {
        if self.speculative == 0 && self.flow_loop == 0 {
            return self.flow_cache.get(key).copied();
        }
        self.flow_cache
            .get(key)
            .copied()
            .or_else(|| self.flow_temp.get(key).map(|v| v.0))
    }

    fn flow_store(&mut self, key: FlowKey, t: TypeId) {
        if self.speculative == 0 && self.flow_loop == 0 {
            self.flow_cache.insert(key, t);
        } else {
            let level = self.flow_pending.len() as u32;
            self.flow_temp.insert(key, (t, level));
        }
    }

    fn walk_loop(&mut self, w: &Walk, flow: FlowId) -> TypeId {
        let key: FlowKey = (w.file.0, flow.0, w.hash, w.declared, w.initial);
        if let Some(t) = self.flow_cached(&key) {
            return t;
        }
        if let Some(&(_, t)) = self.flow_pending.iter().rev().find(|x| x.0 == key) {
            return t;
        }
        let antes: Vec<FlowId> = self.c.bound(w.file).flow.antecedents(flow).to_vec();
        let Some((&first, rest)) = antes.split_first() else {
            return w.declared;
        };
        let entry = if first.is_unreachable() {
            TypeId::NEVER
        } else {
            self.walk_flow(w, first)
        };
        if rest.is_empty() {
            return entry;
        }
        self.flow_pending.push((key, entry));
        let level = self.flow_pending.len() as u32;
        self.flow_loop += 1;
        let mut cur = entry;
        for _ in 0..MAX_LOOP_ITERATIONS {
            self.clear_flow_temp(level);
            let mut types = vec![entry];
            for &a in rest {
                if a.is_unreachable() {
                    continue;
                }
                let t = self.walk_flow(w, a);
                if t != TypeId::NEVER && !types.contains(&t) {
                    types.push(t);
                }
            }
            types.retain(|&t| t != TypeId::NEVER && t != TypeId::SILENT_NEVER);
            let next = self.flow_union(w, &types);
            if next == cur {
                break;
            }
            cur = next;
            if let Some(p) = self.flow_pending.iter_mut().rev().find(|x| x.0 == key) {
                p.1 = cur;
            }
        }
        self.flow_loop -= 1;
        self.flow_pending.retain(|x| x.0 != key);
        self.clear_flow_temp(level);
        self.flow_store(key, cur);
        cur
    }

    // ---------------------------------------------------------------------------------
    // Assignments
    // ---------------------------------------------------------------------------------

    fn assignment_effect(&mut self, w: &Walk, target: Loc) -> AssignEffect {
        // `x += v`, `x++`: the reference keeps its narrowed type, and an unassigned
        // variable stays unassigned (tsc 7).
        if self.is_compound_assignment_target(target) {
            return AssignEffect::None;
        }
        let ast = self.ast(target.file);
        // Quick filter by the root name.
        match self.root_name(target) {
            Some(n) if n == w.root_name => {}
            Some(_) => return AssignEffect::None,
            None => {
                // A property of a computed object (`(s.items as T[])[0] = x`) assigns no
                // variable.
                if matches!(
                    *ast.node(target.node),
                    Node::Member { .. } | Node::Index { .. }
                ) {
                    return AssignEffect::None;
                }
                // Patterns and for-heads: does any identifier inside bind/assign our root?
                if matches!(
                    w.key.root,
                    RefRoot::This | RefRoot::Super | RefRoot::ImportMeta
                ) {
                    return AssignEffect::None;
                }
                if !self.condition_mentions(target, &w.root_name) {
                    return AssignEffect::None;
                }
                return AssignEffect::Reset;
            }
        }
        let Some(k) = self.ref_key(target) else {
            return AssignEffect::None;
        };
        if k == w.key {
            // Only a union (or auto) declared type is narrowed by an assignment: skip
            // checking the assigned value, which uncached (speculatively) re-checks a whole
            // chain of initializers per reference.
            if w.declared != TypeId::AUTO
                && !self.store().flags(w.declared).contains(TypeFlags::UNION)
            {
                return AssignEffect::Assigned(w.declared);
            }
            let assigned = self.assigned_type(w, target);
            let _ = ast;
            return AssignEffect::Assigned(self.assignment_reduced(w, assigned));
        }
        // An assignment to a prefix of the reference resets its narrowing.
        if k.root == w.key.root
            && k.path.len() < w.key.path.len()
            && w.key.path.starts_with(&k.path)
        {
            return AssignEffect::Reset;
        }
        AssignEffect::None
    }

    /// Whether `target` is the operand of `++`/`--` or the left side of an arithmetic
    /// compound assignment (`+=`, `**=`...; not `&&=`/`||=`/`??=`), through parentheses.
    pub(crate) fn is_compound_assignment_target(&self, target: Loc) -> bool {
        let ast = self.ast(target.file);
        let mut n = target;
        for _ in 0..64 {
            let Some(p) = self.c.parent(n) else {
                return false;
            };
            match *ast.node(p.node) {
                Node::Paren { .. } => n = p,
                Node::Update { arg, .. } => return arg == n.node,
                Node::Assign { left, op, .. } => {
                    return left == n.node
                        && !matches!(
                            op,
                            AssignOp::Assign
                                | AssignOp::AndAssign
                                | AssignOp::OrAssign
                                | AssignOp::CoalesceAssign
                        );
                }
                _ => return false,
            }
        }
        false
    }

    /// Whether `f` is a function expression or arrow (not async, not a generator) called
    /// where it is written: `(function () {})()`, `(() => x)()`.
    pub(crate) fn is_iife(&self, f: Loc) -> bool {
        let ast = self.ast(f.file);
        match *ast.node(f.node) {
            Node::FunctionExpr { flags, .. } | Node::Arrow { flags, .. }
                if !flags.intersects(ts_r_ast::FnFlags::ASYNC | ts_r_ast::FnFlags::GENERATOR) => {}
            _ => return false,
        }
        self.is_called_where_written(f)
    }

    /// Whether the function expression or arrow `f` (async or not) is the callee of a call,
    /// through parentheses.
    pub(crate) fn is_called_where_written(&self, f: Loc) -> bool {
        let ast = self.ast(f.file);
        let mut n = f;
        for _ in 0..64 {
            let Some(p) = self.c.parent(n) else {
                return false;
            };
            match *ast.node(p.node) {
                Node::Paren { .. } => n = p,
                Node::Call { callee, .. } => return callee == n.node,
                _ => return false,
            }
        }
        false
    }

    /// Whether `e` lies in the initializer of `decl`'s own declarator, which has no type
    /// annotation (`var a = { f: a }`).
    fn reads_own_unannotated_initializer(&self, e: Loc, decl: Loc) -> bool {
        if e.file != decl.file {
            return false;
        }
        let ast = self.ast(decl.file);
        let Some(p) = self.c.parent(decl) else {
            return false;
        };
        let Node::VarDeclarator {
            type_ann: None,
            init: Some(init),
            ..
        } = *ast.node(p.node)
        else {
            return false;
        };
        let (s, i) = (ast.span(e.node), ast.span(init));
        s.start >= i.start && s.end <= i.end
    }

    /// Whether `e` is inside a parameter's initializer.
    /// Whether `e` is in the decorator of a class member or parameter (tsc has no flow
    /// position there: no TS2454).
    fn in_member_decorator(&self, e: Loc) -> bool {
        let ast = self.ast(e.file);
        let parents = self.c.file(e.file).parents();
        let mut cur = parents.parent(e.node);
        while let Some(n) = cur {
            match *ast.node(n) {
                Node::Decorator { .. } => {
                    return parents.parent(n).is_some_and(|m| {
                        matches!(
                            ast.node(m),
                            Node::MethodDef { .. } | Node::PropertyDef { .. } | Node::Param { .. }
                        )
                    });
                }
                Node::FunctionDecl { .. }
                | Node::FunctionExpr { .. }
                | Node::Arrow { .. }
                | Node::ClassDecl { .. }
                | Node::ClassExpr { .. } => return false,
                _ => {}
            }
            cur = parents.parent(n);
        }
        false
    }

    fn in_parameter_initializer(&self, e: Loc) -> bool {
        let ast = self.ast(e.file);
        let parents = self.c.file(e.file).parents();
        let mut prev = e.node;
        let mut cur = parents.parent(e.node);
        while let Some(n) = cur {
            match *ast.node(n) {
                // An immediately invoked function's defaults run inline, in the flow.
                Node::Param { init: Some(i), .. } => {
                    return i == prev
                        && !parents
                            .parent(n)
                            .is_some_and(|f| self.is_iife(Loc::new(e.file, f)));
                }
                Node::FunctionDecl { .. }
                | Node::FunctionExpr { .. }
                | Node::Arrow { .. }
                | Node::ClassDecl { .. }
                | Node::ClassExpr { .. } => return false,
                _ => {}
            }
            prev = n;
            cur = parents.parent(n);
        }
        false
    }

    /// Whether `e` is the left side of `&&=`, `||=` or `??=`, through parentheses.
    fn is_logical_assignment_target(&self, e: Loc) -> bool {
        let ast = self.ast(e.file);
        let mut n = e;
        for _ in 0..64 {
            let Some(p) = self.c.parent(n) else {
                return false;
            };
            match *ast.node(p.node) {
                Node::Paren { .. } => n = p,
                Node::Assign { left, op, .. } => {
                    return left == n.node
                        && matches!(
                            op,
                            AssignOp::AndAssign | AssignOp::OrAssign | AssignOp::CoalesceAssign
                        );
                }
                _ => return false,
            }
        }
        false
    }

    fn assignment_reduced(&mut self, w: &Walk, assigned: TypeId) -> TypeId {
        let declared = w.declared;
        if declared == TypeId::AUTO {
            let t = self.widen_literal(assigned);
            return self.widened_type(t);
        }
        if assigned == TypeId::SILENT_NEVER {
            return assigned;
        }
        if assigned == TypeId::NEVER || self.store().flags(assigned).intersects(TypeFlags::ANY) {
            return declared;
        }
        let s = self.store();
        if !s.flags(declared).contains(TypeFlags::UNION) {
            return declared;
        }
        let assigned_regular = self.regular_of(assigned);
        let mut ms = self.union_members(assigned_regular);
        // A generic member also stands for the members of its union constraint (the
        // declared type of an access object may be that constraint).
        for a in ms.clone() {
            if self.store().flags(a).intersects(TypeFlags::INSTANTIABLE) {
                let c = self.base_constraint(a);
                if self.store().flags(c).contains(TypeFlags::UNION) {
                    ms.extend(self.union_members(c));
                }
            }
        }
        let r = self.filter_type(declared, |ck, d| ms.iter().any(|&a| ck.is_assignable(a, d)));
        if r == TypeId::NEVER {
            return declared;
        }
        // `boolean` members assigned `true`/`false` narrow to the literal. A fresh literal
        // (`let c = true`) stays fresh, so it widens again where literals widen (the
        // inferred return type of `return c` is `boolean`, observed in tsc 7.0.2).
        if (assigned_regular == TypeId::TRUE || assigned_regular == TypeId::FALSE)
            && (r == TypeId::BOOLEAN || r == assigned_regular)
        {
            return assigned;
        }
        r
    }

    /// The object of the `for (const k in obj)` whose declared variable is the assignment
    /// target `target` (an expression head, `for (k in obj)`, narrows nothing).
    fn for_in_object_of_head(&self, target: Loc) -> Option<Loc> {
        let p = self.c.parent(target)?;
        let (stmt, head) = match *self.node(p) {
            Node::VarDeclarator { name, .. } if name == target.node => {
                let list = self.c.parent(p)?;
                (self.c.parent(list)?, list.node)
            }
            _ => return None,
        };
        match *self.node(stmt) {
            Node::ForIn { left, right, .. } if left == head => Some(Loc::new(target.file, right)),
            _ => None,
        }
    }

    /// The type assigned at an assignment flow node's target.
    fn assigned_type(&mut self, w: &Walk, target: Loc) -> TypeId {
        let file = target.file;
        let Some(p) = self.c.parent(target) else {
            return w.declared;
        };
        match *self.node(p) {
            Node::VarDeclarator {
                name,
                init,
                type_ann,
                ..
            } if name == target.node => {
                if let Some(list) = self.c.parent(p)
                    && let Some(stmt) = self.c.parent(list)
                    && let Node::ForIn { left, .. } | Node::ForOf { left, .. } = *self.node(stmt)
                    && left == list.node
                {
                    return w.declared;
                }
                // An unannotated `const` is declared as its initializer's type, so the
                // assignment narrows nothing; re-checking the initializer (uncached while
                // speculating) at every reference multiplies the work.
                if type_ann.is_none()
                    && w.declared != TypeId::AUTO
                    && let Some(list) = self.c.parent(p)
                    && let Node::VarDecl {
                        kind: VarKind::Const | VarKind::Using | VarKind::AwaitUsing,
                        ..
                    } = *self.node(list)
                {
                    return w.declared;
                }
                match init {
                    Some(i) => self.quietly(|ck| ck.check_expr(Loc::new(file, i))),
                    None => w.declared,
                }
            }
            // A target inside a destructuring assignment (`[a, b] = v`, `({ a } = v)`).
            Node::ArrayLit { .. } | Node::Property { .. } | Node::ShorthandProp { .. }
                if self.in_assignment_pattern(target) =>
            {
                self.destructured_target_type(target, 0)
                    .unwrap_or(w.declared)
            }
            Node::Assign { left, op, right } if left == target.node => {
                if op == AssignOp::Assign && self.in_assignment_pattern(p) {
                    // `[a = d] = v`: the element without `undefined`, or the default.
                    return self
                        .destructured_target_type(target, 0)
                        .unwrap_or(w.declared);
                }
                if op == AssignOp::Assign {
                    // `x = []` makes an auto-typed `x` an evolving array (not modelled, so
                    // silent, as `let xs = []` is).
                    if w.declared == TypeId::AUTO
                        && let Node::ArrayLit { elements } = *self.node(Loc::new(
                            file,
                            crate::nodes::skip_parens(self.ast(file), right),
                        ))
                        && self.ast(file).list(elements).is_empty()
                    {
                        return self.create_array(TypeId::UNRESOLVED, false);
                    }
                    self.quietly(|ck| ck.check_expr(Loc::new(file, right)))
                } else {
                    self.quietly(|ck| ck.check_expr(p))
                }
            }
            Node::Paren { .. } => {
                // `(x) = v`: find the enclosing assignment.
                let mut cur = p;
                for _ in 0..16 {
                    let Some(pp) = self.c.parent(cur) else { break };
                    match *self.node(pp) {
                        Node::Paren { .. } => cur = pp,
                        Node::Assign {
                            left,
                            op: AssignOp::Assign,
                            right,
                        } if left == cur.node => {
                            return self.quietly(|ck| ck.check_expr(Loc::new(file, right)));
                        }
                        _ => break,
                    }
                }
                w.declared
            }
            Node::Update { .. } => {
                let t = self.quietly(|ck| ck.check_expr(p));
                if self.store().flags(t).intersects(TypeFlags::BIGINT_LIKE) {
                    TypeId::BIGINT
                } else {
                    TypeId::NUMBER
                }
            }
            Node::ForIn { left, .. } if left == target.node => TypeId::STRING,
            Node::ForOf {
                left,
                right,
                is_await,
                ..
            } if left == target.node => {
                let rt = self.quietly(|ck| ck.check_expr(Loc::new(file, right)));
                self.iterated_type(rt, is_await).unwrap_or(w.declared)
            }
            // A parameter with a default starts without `undefined` unless the default may
            // be `undefined` (`(options = {}) => options.x` under `options?: O`).
            Node::Param {
                pattern,
                init: Some(i),
                ..
            } if pattern == target.node && self.c.opts.strict_null_checks => {
                let it = self.quietly(|ck| ck.check_expr(Loc::new(file, i)));
                if self.contains_undefined(it) {
                    return w.declared;
                }
                let nn = self.filter_type(w.declared, |ck, m| {
                    !ck.store().flags(m).contains(TypeFlags::UNDEFINED)
                });
                if nn == TypeId::NEVER { w.declared } else { nn }
            }
            _ => w.declared,
        }
    }

    /// Whether `n` is an element of an array or object literal that is an assignment
    /// target (`[n] = v`, `({ k: n } = v)`, `for ([n] of xs)`), possibly nested.
    fn in_assignment_pattern(&self, n: Loc) -> bool {
        let mut cur = n;
        let mut in_literal = false;
        for _ in 0..64 {
            let Some(p) = self.c.parent(cur) else {
                return false;
            };
            match *self.node(p) {
                Node::ArrayLit { .. } | Node::ObjectLit { .. } => in_literal = true,
                Node::Paren { .. } | Node::Spread { .. } | Node::ShorthandProp { .. } => {}
                Node::Property { value, .. } if value == cur.node => {}
                Node::Assign {
                    left,
                    op: AssignOp::Assign,
                    ..
                } => {
                    if left == cur.node {
                        // The pattern's root, or a default inside a pattern.
                        if in_literal {
                            return true;
                        }
                    } else {
                        return false;
                    }
                }
                Node::ForOf { left, .. } | Node::ForIn { left, .. } => {
                    return in_literal && left == cur.node;
                }
                _ => return false,
            }
            cur = p;
        }
        false
    }

    /// The type a destructuring assignment gives the target `n` (`None`: not modelled).
    fn destructured_target_type(&mut self, n: Loc, depth: u32) -> Option<TypeId> {
        if depth > 32 {
            return None;
        }
        let p = self.c.parent(n)?;
        let file = n.file;
        match *self.node(p) {
            Node::Paren { .. } => self.destructured_target_type(p, depth + 1),
            Node::Assign {
                left,
                op: AssignOp::Assign,
                right,
            } if left == n.node => {
                let pp = self.c.parent(p)?;
                let is_default = match *self.node(pp) {
                    Node::ArrayLit { .. } => true,
                    Node::Property { value, .. } => value == p.node,
                    _ => false,
                };
                if is_default {
                    let base = self.destructured_target_type(p, depth + 1)?;
                    let d = self.quietly(|ck| ck.check_expr(Loc::new(file, right)));
                    let base = if self.c.opts.strict_null_checks {
                        self.filter_type(base, |ck, m| {
                            !ck.store().flags(m).contains(TypeFlags::UNDEFINED)
                        })
                    } else {
                        base
                    };
                    Some(self.union2(base, d))
                } else {
                    Some(self.quietly(|ck| ck.check_expr(Loc::new(file, right))))
                }
            }
            Node::ForOf {
                left,
                right,
                is_await,
                ..
            } if left == n.node => {
                let rt = self.quietly(|ck| ck.check_expr(Loc::new(file, right)));
                self.iterated_type(rt, is_await)
            }
            Node::ArrayLit { elements } => {
                let idx = self
                    .ast(file)
                    .list(elements)
                    .iter()
                    .position(|&e| e == n.node)?;
                let parent_t = self.destructured_target_type(p, depth + 1)?;
                if self.store().flags(parent_t).intersects(TypeFlags::ANY) {
                    return Some(parent_t);
                }
                Some(self.array_element_at(parent_t, idx))
            }
            Node::Property { key, value, .. } if value == n.node => {
                let obj = self.c.parent(p)?;
                let parent_t = self.destructured_target_type(obj, depth + 1)?;
                let ast = self.ast(file);
                let name = match crate::nodes::key_text(ast, key) {
                    Some(k) => k,
                    None => match *ast.node(key) {
                        Node::ComputedKey { expr } => self.late_bound_name(Loc::new(file, expr))?,
                        _ => return None,
                    },
                };
                Some(self.destructured_property_type(parent_t, &name))
            }
            Node::ShorthandProp { name, init, .. } if name == n.node => {
                let obj = self.c.parent(p)?;
                let parent_t = self.destructured_target_type(obj, depth + 1)?;
                let Node::Ident { name: atom } = *self.node(n) else {
                    return None;
                };
                let text = self.ast(file).atom(atom).to_string();
                let t = self.destructured_property_type(parent_t, &text);
                match init {
                    Some(i) => {
                        let d = self.quietly(|ck| ck.check_expr(Loc::new(file, i)));
                        let base = if self.c.opts.strict_null_checks {
                            self.filter_type(t, |ck, m| {
                                !ck.store().flags(m).contains(TypeFlags::UNDEFINED)
                            })
                        } else {
                            t
                        };
                        Some(self.union2(base, d))
                    }
                    None => Some(t),
                }
            }
            _ => None,
        }
    }

    // ---------------------------------------------------------------------------------
    // Conditions
    // ---------------------------------------------------------------------------------

    /// Whether the expression mentions the identifier `name` (or `this`).
    fn condition_mentions(&mut self, l: Loc, name: &str) -> bool {
        let ast = self.ast(l.file);
        let mut stack = vec![l.node];
        let mut seen = 0u32;
        while let Some(n) = stack.pop() {
            seen += 1;
            if seen > MAX_CONDITION_SCAN {
                return true;
            }
            match *ast.node(n) {
                Node::Ident { name: a } => {
                    if ast.atom(a) == name {
                        return true;
                    }
                    if self
                        .alias_root(Loc::new(l.file, n))
                        .is_some_and(|r| &*r == name)
                    {
                        return true;
                    }
                    // An aliased condition mentions what its initializer does.
                    if let Some(init) = self.condition_alias_init(Loc::new(l.file, n))
                        && init.file == l.file
                    {
                        stack.push(init.node);
                    }
                }
                Node::This {} => {
                    if name == "this" {
                        return true;
                    }
                }
                Node::Super {} => {
                    if name == "super" {
                        return true;
                    }
                }
                Node::MetaProperty {
                    kind: ts_r_ast::MetaPropKind::ImportMeta,
                } => {
                    if name == "import.meta" {
                        return true;
                    }
                }
                Node::FunctionExpr { .. } | Node::Arrow { .. } | Node::ClassExpr { .. } => {}
                Node::Member { obj, .. } => stack.push(obj),
                _ => ast.for_each_child(n, |c| stack.push(c)),
            }
        }
        false
    }

    /// The initializer of the `const` (without type annotation) an identifier refers to,
    /// for aliased conditions.
    fn condition_alias_init(&mut self, l: Loc) -> Option<Loc> {
        let s = self.ident_ref_symbol(l)?;
        if !self
            .c
            .sym_flags(s)
            .intersects(SymbolFlags::BLOCK_SCOPED_VARIABLE)
        {
            return None;
        }
        let d = self.c.value_decl(s)?;
        if d == l {
            return None;
        }
        let ast = self.ast(d.file);
        let p = self.c.parent(d)?;
        let Node::VarDeclarator {
            name,
            type_ann: None,
            init: Some(i),
            ..
        } = *ast.node(p.node)
        else {
            return None;
        };
        if name != d.node || !self.is_const_declaration(d) {
            return None;
        }
        Some(Loc::new(d.file, i))
    }

    /// A reference narrowable through an aliased condition: a `const` variable or a
    /// parameter that is never assigned.
    /// Also (observed, tsc 7): a local `let` that is never assigned, `this`, and a
    /// read-only property (`obj.x`, `this.x`, `outer.obj`, `t[0]` of a read-only tuple) of
    /// a constant reference.
    fn is_constant_reference(&mut self, w: &Walk) -> bool {
        if w.key.path.is_empty() {
            let RefRoot::Sym(s) = w.key.root else {
                return false;
            };
            return self.is_constant_symbol(s) || self.is_unassigned_local_let(s);
        }
        match w.at {
            Some(at) => self.is_constant_reference_expr(at, 0),
            None => false,
        }
    }

    fn is_unassigned_local_let(&mut self, s: SymId) -> bool {
        let Some(d) = self.c.value_decl(s) else {
            return false;
        };
        self.is_local_let_declaration(d) && !self.is_symbol_assigned(s, d.file)
    }

    /// `e` is a constant reference (see `is_constant_reference`).
    fn is_constant_reference_expr(&mut self, e: Loc, depth: u32) -> bool {
        if depth > 16 {
            return false;
        }
        let ast = self.ast(e.file);
        let n = skip_parens(ast, e.node);
        match *ast.node(n) {
            Node::This {} => true,
            Node::Ident { .. } => self
                .ident_ref_symbol(Loc::new(e.file, n))
                .is_some_and(|s| self.is_constant_symbol(s) || self.is_unassigned_local_let(s)),
            Node::Member { obj, prop, .. } => {
                let Some(name) = crate::nodes::ident_text(ast, prop).map(str::to_owned) else {
                    return false;
                };
                self.is_constant_reference_expr(Loc::new(e.file, obj), depth + 1)
                    && self.is_readonly_member_of(Loc::new(e.file, obj), &name)
            }
            Node::Index { obj, index, .. } => {
                let name = match *ast.node(skip_parens(ast, index)) {
                    Node::StringLit { value } => ast.atom(value).to_owned(),
                    Node::NumberLit { value } => ts_r_binder::js_number_to_string(value.get()),
                    _ => return false,
                };
                self.is_constant_reference_expr(Loc::new(e.file, obj), depth + 1)
                    && self.is_readonly_member_of(Loc::new(e.file, obj), &name)
            }
            _ => false,
        }
    }

    /// Property `name` of the type of expression `obj` is read-only (in every member).
    fn is_readonly_member_of(&mut self, obj: Loc, name: &str) -> bool {
        let t = self.quietly(|ck| ck.check_expr(obj));
        let t = self.non_nullable(t);
        let members = self.union_members(t);
        !members.is_empty()
            && members.into_iter().all(|m| {
                let a = self.apparent_type(m);
                match self.property_of_type(a, name) {
                    Some(p) => self.is_readonly_symbol(p),
                    None => false,
                }
            })
    }

    /// A key of a narrowable `obj[key]`: a constant symbol, or a `let`/`var` that is never
    /// assigned after its declaration.
    fn is_constant_key_symbol(&mut self, s: SymId) -> bool {
        if self.is_constant_symbol(s) {
            return true;
        }
        let Some(d) = self.c.value_decl(s) else {
            return false;
        };
        let declarator = matches!(self.node(d), Node::VarDeclarator { .. })
            || self
                .c
                .parent(d)
                .is_some_and(|p| matches!(self.node(p), Node::VarDeclarator { .. }));
        declarator && !self.is_symbol_assigned(s, d.file)
    }

    /// A `const` variable or a parameter that is never assigned.
    fn is_constant_symbol(&mut self, s: SymId) -> bool {
        let Some(d) = self.c.value_decl(s) else {
            return false;
        };
        if self.is_const_declaration(d) {
            return true;
        }
        // A parameter, or a catch clause variable (observed: `const isS = typeof e ===
        // "string"; if (isS) e.toUpperCase()` narrows the unassigned `e` of `catch (e)`),
        // also a binding of a parameter's (or catch variable's) pattern.
        if matches!(self.node(d), Node::Param { .. }) {
            return !self.is_symbol_assigned(s, d.file);
        }
        let mut cur = d;
        for _ in 0..32 {
            let Some(p) = self.c.parent(cur) else {
                return false;
            };
            match self.node(p) {
                Node::Param { .. } | Node::CatchClause { param: Some(_), .. } => {
                    return !self.is_symbol_assigned(s, d.file);
                }
                Node::BindingProperty { .. }
                | Node::ObjectPattern { .. }
                | Node::ArrayPattern { .. }
                | Node::AssignPattern { .. }
                | Node::Rest { .. } => cur = p,
                _ => return false,
            }
        }
        false
    }

    fn is_ref(&mut self, w: &Walk, l: Loc) -> bool {
        let ast = self.ast(l.file);
        let mut n = skip_parens(ast, l.node);
        // `(x = f()) !== null` tests `x`; `(a, x)` tests `x`.
        for _ in 0..16 {
            match *ast.node(n) {
                Node::Assign {
                    left,
                    op:
                        AssignOp::Assign
                        | AssignOp::AndAssign
                        | AssignOp::OrAssign
                        | AssignOp::CoalesceAssign,
                    ..
                } => n = skip_parens(ast, left),
                Node::Seq { exprs } => match ast.list(exprs).last() {
                    Some(&e) => n = skip_parens(ast, e),
                    None => return false,
                },
                _ => break,
            }
        }
        if !matches!(
            ast.node(n),
            Node::Ident { .. }
                | Node::This {}
                | Node::Super {}
                | Node::Member { .. }
                | Node::Index { .. }
                | Node::NonNull { .. }
        ) {
            return false;
        }
        // `(x = f()).p` tests `x.p`.
        let through_assign = self.object_is_assignment(Loc::new(l.file, n));
        match self.root_name_of(Loc::new(l.file, n), through_assign) {
            Some(r) if r == w.root_name => {}
            _ => return false,
        }
        if through_assign {
            return self
                .compute_ref_key(Loc::new(l.file, n), 0, true)
                .is_some_and(|k| k == w.key);
        }
        self.ref_key(Loc::new(l.file, n))
            .is_some_and(|k| k == w.key)
    }

    /// Whether the object chain of an access (`(x = f()).a.b`) ends in an assignment.
    fn object_is_assignment(&self, l: Loc) -> bool {
        let ast = self.ast(l.file);
        let mut n = l.node;
        for _ in 0..64 {
            match *ast.node(n) {
                Node::Paren { expr } | Node::NonNull { expr } => n = expr,
                Node::Member { obj, .. } | Node::Index { obj, .. } => n = obj,
                Node::Assign { .. } => return n != l.node,
                _ => return false,
            }
        }
        false
    }

    /// [`Ck::is_ref`] without seeing through an assignment: equality and `typeof` tests
    /// of `(x = f())?.p` say nothing about `x` itself (observed).
    fn is_ref_not_assigned(&mut self, w: &Walk, l: Loc) -> bool {
        let ast = self.ast(l.file);
        if matches!(ast.node(skip_parens(ast, l.node)), Node::Assign { .. }) {
            return false;
        }
        self.is_ref(w, l)
    }

    /// The root name of the reference the identifier `l` aliases, if any.
    fn alias_root(&mut self, l: Loc) -> Option<Box<str>> {
        if let Some(r) = self.alias_roots.get(&l) {
            return r.clone();
        }
        self.alias_roots.insert(l, None);
        let r = self
            .ident_ref_symbol(l)
            .and_then(|s| self.binding_alias(s))
            .map(|a| a.root);
        self.alias_roots.insert(l, r.clone());
        r
    }

    pub(crate) fn binding_alias(&mut self, s: SymId) -> Option<BindingAlias> {
        if let Some(a) = self.binding_aliases.get(&s) {
            return a.clone();
        }
        self.binding_aliases.insert(s, None);
        let a = self.compute_binding_alias(s);
        self.binding_aliases.insert(s, a.clone());
        a
    }

    fn compute_binding_alias(&mut self, s: SymId) -> Option<BindingAlias> {
        let d = self.c.value_decl(s)?;
        let f = d.file;
        let ast = self.ast(f);
        if !matches!(ast.node(d.node), Node::Ident { .. }) {
            return None;
        }
        let p = self.c.parent(d)?;
        // `const k = ref.prop`
        if let Node::VarDeclarator {
            name,
            init: Some(i),
            type_ann: None,
            ..
        } = *ast.node(p.node)
            && name == d.node
        {
            if !self.is_const_declaration(d) {
                return None;
            }
            let (obj, prop) = match *ast.node(skip_parens(ast, i)) {
                Node::Member { obj, prop, .. } => (obj, key_text(ast, prop)?),
                Node::Index { obj, index, .. } => {
                    let ix = skip_parens(ast, index);
                    if !matches!(
                        ast.node(ix),
                        Node::StringLit { .. } | Node::NumberLit { .. }
                    ) {
                        return None;
                    }
                    (obj, key_text(ast, ix)?)
                }
                _ => return None,
            };
            return self.alias_of_reference(Loc::new(f, obj), prop, None);
        }
        // An element of a pattern: `{ prop: s }`, `{ s }`, `[s]` (no default, not nested).
        let (pattern, prop) = match *ast.node(p.node) {
            Node::BindingProperty {
                key,
                value,
                init: None,
            } => {
                if !(value == Some(d.node) || (value.is_none() && key == d.node)) {
                    return None;
                }
                (self.c.parent(p)?, key_text(ast, key)?)
            }
            Node::ArrayPattern { elements } => {
                let idx = ast.list(elements).iter().position(|&e| e == d.node)?;
                (p, idx.to_string())
            }
            _ => return None,
        };
        let pp = self.c.parent(pattern)?;
        match *ast.node(pp.node) {
            Node::VarDeclarator { name, init, .. } if name == pattern.node => {
                if !self.is_const_declaration(d) {
                    return None;
                }
                // `const { kind } = v`: the binding is `v.kind`.
                if let Some(i) = init
                    && let Some(a) = self.alias_of_reference(
                        Loc::new(f, skip_parens(ast, i)),
                        prop.clone(),
                        Some(pattern),
                    )
                {
                    return Some(a);
                }
                // Any other source (a call, `for (const { k, v } of xs)`): the pattern alone.
                self.pattern_alias(pattern, pp, prop)
            }
            Node::Param {
                pattern: pat,
                rest: false,
                ..
            } if pat == pattern.node => {
                if self.is_symbol_assigned(s, f) {
                    return None;
                }
                self.pattern_alias(pattern, pp, prop)
            }
            _ => None,
        }
    }

    /// An alias whose parent is the pattern itself (it has no reference or symbol of its
    /// own; its first binding names it). Every binding of the pattern must keep its value.
    fn pattern_alias(&mut self, pattern: Loc, owner: Loc, prop: String) -> Option<BindingAlias> {
        let ps = self.first_pattern_binding(pattern)?;
        Some(BindingAlias {
            key: RefKey {
                root: RefRoot::Sym(ps),
                path: Vec::new(),
            },
            // Not an identifier: only aliases name this root.
            root: format!("\u{1}{}", owner.node.index()).into(),
            prop: prop.into(),
            pattern: Some(pattern),
        })
    }

    /// Whether `prop` discriminates the reference of `w` at a point where its type is `t`:
    /// a discriminant of the declared type or of `t` (observed: a reference narrowed by a
    /// type guard to a union narrows by its discriminant; `{ timer: undefined }` never does).
    fn is_discriminant_here(&mut self, w: &Walk, t: TypeId, prop: &str) -> bool {
        self.is_discriminant_property(t, prop) || self.is_discriminant_property(w.declared, prop)
    }

    /// Whether `prop` discriminates the union `t`: the property types of the members that
    /// have it differ, and one of them is a literal type (a unit type, or a union of unit
    /// types only).
    fn is_discriminant_property(&mut self, t: TypeId, prop: &str) -> bool {
        if !self.store().flags(t).contains(TypeFlags::UNION) {
            return false;
        }
        let members = self.store().members_of_union(t).to_vec();
        let mut first: Option<TypeId> = None;
        let mut non_uniform = false;
        let mut literal = false;
        for m in members {
            if !self
                .store()
                .flags(m)
                .intersects(TypeFlags::OBJECT | TypeFlags::INTERSECTION)
            {
                continue;
            }
            let Some(p) = self.property_of_type(m, prop) else {
                continue;
            };
            let pt = self.type_of_symbol(p);
            match first {
                None => first = Some(pt),
                Some(f) if f != pt => non_uniform = true,
                Some(_) => {}
            }
            let s = self.store();
            let unit = |x: TypeId| s.flags(x).intersects(TypeFlags::UNIT);
            if unit(pt)
                || (s.flags(pt).contains(TypeFlags::UNION)
                    && s.members_of_union(pt).iter().all(|&x| unit(x)))
            {
                literal = true;
            }
        }
        non_uniform && literal
    }

    /// The symbol of the first identifier bound directly by `pattern`.
    fn first_pattern_binding(&self, pattern: Loc) -> Option<SymId> {
        let ast = self.ast(pattern.file);
        let ids: Vec<NodeId> = match *ast.node(pattern.node) {
            Node::ObjectPattern { props } => ast
                .list(props)
                .iter()
                .filter_map(|&p| match *ast.node(p) {
                    Node::BindingProperty { key, value, .. } => Some(value.unwrap_or(key)),
                    _ => None,
                })
                .collect(),
            Node::ArrayPattern { elements } => ast.list(elements).to_vec(),
            _ => return None,
        };
        ids.into_iter()
            .filter(|&n| matches!(ast.node(n), Node::Ident { .. }))
            .find_map(|n| self.c.sym_of_decl(Loc::new(pattern.file, n)))
    }

    /// `prop` of the reference `r`, when `r` keeps its value (a const, a parameter that
    /// is never assigned, or `this`).
    fn alias_of_reference(
        &mut self,
        r: Loc,
        prop: String,
        pattern: Option<Loc>,
    ) -> Option<BindingAlias> {
        let key = self.ref_key(r)?;
        let root = self.root_name(r)?;
        // `this` keeps its value too (`const t = this.test.type`).
        let keeps = match key.root {
            RefRoot::Sym(rs) => {
                let rd = self.c.value_decl(rs)?;
                match *self.ast(rd.file).node(rd.node) {
                    Node::Param { .. } => !self.is_symbol_assigned(rs, rd.file),
                    _ => self.is_const_declaration(rd),
                }
            }
            RefRoot::This | RefRoot::ImportMeta => true,
            RefRoot::Super => false,
        };
        if !keeps {
            return None;
        }
        Some(BindingAlias {
            key,
            root,
            prop: prop.into(),
            pattern,
        })
    }

    /// The type of the destructured binding `sym` read at `e`, narrowed through its
    /// parent: conditions on sibling bindings (`const { kind, data } = v` or a parameter
    /// pattern) narrow the parent union, and the binding takes its property of that.
    fn destructured_binding_type(&mut self, e: Loc, sym: SymId) -> Option<TypeId> {
        let a = self.binding_alias(sym)?;
        let pattern = a.pattern?;
        let parent = self.pattern_parent_type(pattern);
        if !self.store().flags(parent).contains(TypeFlags::UNION) {
            return None;
        }
        let flow = self.flow_of_reference(e)?;
        if flow.is_unreachable() {
            return None;
        }
        let mut h = FxHasher::default();
        a.key.hash(&mut h);
        "\u{1}pattern".hash(&mut h);
        let w = Walk {
            key: a.key,
            hash: h.finish(),
            root_name: a.root,
            declared: parent,
            initial: parent,
            // Every alias binding keeps its value, so closures see the narrowing too.
            cross_functions: true,
            file: e.file,
            at: None,
        };
        let nt = self.walk_flow(&w, flow);
        if nt == parent {
            return None;
        }
        // Siblings that rule out every member leave the binding `never`.
        if nt == TypeId::NEVER {
            return Some(TypeId::NEVER);
        }
        let ast = self.ast(pattern.file);
        Some(match ast.node(pattern.node) {
            Node::ArrayPattern { .. } => {
                let idx: usize = a.prop.parse().ok()?;
                self.array_element_at(nt, idx)
            }
            _ => self.destructured_property_type(nt, &a.prop),
        })
    }

    /// `l` is `ref.prop` (or `ref?.prop`, `ref["prop"]`, or a binding aliasing `ref.prop`):
    /// the property name.
    fn discriminant_of(&mut self, w: &Walk, l: Loc) -> Option<String> {
        let ast = self.ast(l.file);
        let n = skip_parens(ast, l.node);
        match *ast.node(n) {
            Node::Ident { .. } => {
                let il = Loc::new(l.file, n);
                if self.alias_root(il).is_none_or(|r| r != w.root_name) {
                    return None;
                }
                let s = self.ident_ref_symbol(il)?;
                let a = self.binding_alias(s)?;
                if a.key != w.key || !self.is_discriminant_property(w.declared, &a.prop) {
                    return None;
                }
                Some(a.prop.to_string())
            }
            Node::Member { obj, prop, .. } => {
                if self.is_ref(w, Loc::new(l.file, obj)) {
                    return key_text(ast, prop);
                }
                None
            }
            Node::Index { obj, index, .. } => {
                let i = skip_parens(ast, index);
                if matches!(
                    ast.node(i),
                    Node::StringLit { .. } | Node::NumberLit { .. } | Node::TemplateLit { .. }
                ) && self.is_ref(w, Loc::new(l.file, obj))
                {
                    return key_text(ast, i);
                }
                None
            }
            _ => None,
        }
    }

    /// Whether `l` is an optional chain (`ref?.a.b`, `ref?.()`) containing the reference as
    /// the object of an optional link.
    fn optional_chain_contains_ref(&mut self, w: &Walk, l: Loc) -> bool {
        self.optional_chain_contains(w, l, true)
    }

    /// [`Ck::optional_chain_contains_ref`] for equality and `typeof` tests, which don't
    /// see the reference through an assignment (`(x = f())?.p === 1` leaves `x` alone).
    fn optional_chain_contains_ref_eq(&mut self, w: &Walk, l: Loc) -> bool {
        self.optional_chain_contains(w, l, false)
    }

    fn optional_chain_contains(&mut self, w: &Walk, l: Loc, through_assign: bool) -> bool {
        let ast = self.ast(l.file);
        let mut n = skip_parens(ast, l.node);
        let mut optional_seen = false;
        for _ in 0..64 {
            match *ast.node(n) {
                Node::Member { obj, optional, .. } | Node::Index { obj, optional, .. } => {
                    if optional && self.chain_ref(w, Loc::new(l.file, obj), through_assign) {
                        return true;
                    }
                    if !optional_seen
                        && !optional
                        && self.chain_ref(w, Loc::new(l.file, obj), through_assign)
                    {
                        return false;
                    }
                    optional_seen |= optional;
                    n = obj;
                }
                Node::Call {
                    callee, optional, ..
                } => {
                    if optional && self.chain_ref(w, Loc::new(l.file, callee), through_assign) {
                        return true;
                    }
                    optional_seen |= optional;
                    n = callee;
                }
                Node::NonNull { expr } | Node::Paren { expr } => n = expr,
                _ => return false,
            }
        }
        false
    }

    fn chain_ref(&mut self, w: &Walk, l: Loc, through_assign: bool) -> bool {
        if through_assign {
            self.is_ref(w, l)
        } else {
            self.is_ref_not_assigned(w, l)
        }
    }

    /// The type predicate a function without a return annotation implies (TypeScript 5.5
    /// behaviour): its body is one returned condition that narrows a parameter `x` to `T`
    /// when true, and every value of `T` makes it true (narrowing `T` by the false branch
    /// leaves `never`). `x => x !== undefined` on `string | undefined` is `x is string`.
    pub(crate) fn inferred_type_predicate(
        &mut self,
        sig: ts_r_types::SigId,
    ) -> Option<ts_r_types::TypePredicate> {
        // A function expression's signature made while checking it against a candidate
        // signature carries that candidate's contextual parameter types. The body is checked
        // with those types and speculatively, so nothing about the parameters is published
        // (asking for them outside the candidate would give an implicit `any`).
        self.push_assumptions();
        let installed = self.install_signature_param_types(sig);
        if installed.is_empty() {
            self.pop_assumptions();
            return self.inferred_type_predicate_inner(sig);
        }
        self.enter_speculation();
        let r = self.inferred_type_predicate_inner(sig);
        self.leave_speculation();
        for p in installed {
            self.param_types.remove(&p);
        }
        self.pop_assumptions();
        r
    }

    /// Puts the signature's parameter types into `param_types` for the unannotated
    /// parameters of its declaration that have none yet; returns the parameters set.
    fn install_signature_param_types(&mut self, sig: ts_r_types::SigId) -> Vec<Loc> {
        let d = self.store().sig(sig);
        let Some(decl) = d.decl.and_then(Loc::unpack) else {
            return Vec::new();
        };
        let tys: Vec<TypeId> = d.params.iter().map(|p| p.ty).collect();
        let ast = self.ast(decl.file);
        if !matches!(
            ast.node(decl.node),
            Node::FunctionExpr { .. } | Node::Arrow { .. }
        ) {
            return Vec::new();
        }
        let Some(parts) = crate::nodes::fn_parts(ast, decl.node) else {
            return Vec::new();
        };
        let params = ast
            .list(parts.params)
            .iter()
            .copied()
            .filter(|&x| !crate::nodes::is_this_param(ast, x));
        let mut out = Vec::new();
        for (p, ty) in params.zip(tys) {
            let pl = Loc::new(decl.file, p);
            if matches!(*ast.node(p), Node::Param { type_ann: None, .. })
                && ty != TypeId::UNRESOLVED
                && !self.param_types.contains_key(&pl)
            {
                self.param_types.insert(pl, ty);
                out.push(pl);
            }
        }
        out
    }

    fn inferred_type_predicate_inner(
        &mut self,
        sig: ts_r_types::SigId,
    ) -> Option<ts_r_types::TypePredicate> {
        let d = self.store().sig(sig);
        let decl = d.decl.and_then(Loc::unpack)?;
        if d.params.is_empty() || d.flags.contains(ts_r_types::SigFlags::HAS_RETURN_TYPE) {
            return None;
        }
        let params: Vec<(SymId, ts_r_types::StrId, TypeId)> =
            d.params.iter().map(|p| (p.sym, p.name, p.ty)).collect();
        let has_rest = d.has_rest();
        let ast = self.ast(decl.file);
        let parts = crate::nodes::fn_parts(ast, decl.node)?;
        if parts.return_type.is_some()
            || parts
                .flags
                .intersects(ts_r_ast::FnFlags::ASYNC | ts_r_ast::FnFlags::GENERATOR)
        {
            return None;
        }
        let body = parts.body?;
        let expr = match *ast.node(body) {
            Node::Block { stmts } => {
                let list = ast.list(stmts);
                if list.len() != 1 {
                    return None;
                }
                match *ast.node(list[0]) {
                    Node::Return { arg: Some(a) } => a,
                    _ => return None,
                }
            }
            _ if parts.is_arrow => body,
            _ => return None,
        };
        let cond = Loc::new(decl.file, expr);
        // Only boolean-returning functions imply predicates (`(x = {}) => x` does not).
        let ct = self.quietly(|ck| ck.check_expr(cond));
        if !self.every_member(ct, |m| {
            self.store().flags(m).intersects(TypeFlags::BOOLEAN_LIKE)
        }) {
            return None;
        }
        let n = params.len() - usize::from(has_rest);
        for (i, &(psym, pname, declared)) in params.iter().enumerate().take(n) {
            if psym.is_none() || self.is_symbol_assigned(psym, decl.file) {
                continue;
            }
            let name = self.store().str(pname).to_owned();
            if !self.condition_mentions(cond, &name) {
                continue;
            }
            if self.store().is_error(declared)
                || self
                    .store()
                    .flags(declared)
                    .intersects(TypeFlags::ANY | TypeFlags::NEVER)
                || declared == TypeId::UNRESOLVED
            {
                continue;
            }
            let key = RefKey {
                root: RefRoot::Sym(psym),
                path: Vec::new(),
            };
            let mut h = FxHasher::default();
            key.hash(&mut h);
            let w = Walk {
                key,
                hash: h.finish(),
                root_name: name.clone().into(),
                declared,
                initial: declared,
                cross_functions: false,
                file: decl.file,
                at: None,
            };
            let t_true = self.narrow(&w, declared, cond, true, 0);
            if t_true == declared || t_true == TypeId::NEVER {
                continue;
            }
            // "Only if": the false branch removes exactly the members the true branch keeps
            // (union partition), or no value of the true type can make the condition false.
            let t_false = self.narrow(&w, declared, cond, false, 0);
            let s = self.store();
            let members = |t: TypeId| -> Vec<TypeId> {
                if s.flags(t).contains(TypeFlags::UNION) {
                    s.members_of_union(t).to_vec()
                } else if t == TypeId::NEVER {
                    Vec::new()
                } else {
                    vec![t]
                }
            };
            let (dm, tm, fm) = (members(declared), members(t_true), members(t_false));
            let partition = dm.len() > 1
                && tm.iter().all(|m| dm.contains(m))
                && dm.iter().all(|m| tm.contains(m) != fm.contains(m));
            if !partition && self.narrow(&w, t_true, cond, false, 0) != TypeId::NEVER {
                continue;
            }
            return Some(ts_r_types::TypePredicate {
                kind: ts_r_types::PredicateKind::Identifier,
                param_index: i as u32,
                param_name: pname,
                ty: Some(t_true),
            });
        }
        None
    }

    fn narrow_nullish(&mut self, w: &Walk, t: TypeId, cond: Loc, assume_true: bool) -> TypeId {
        // TrueCondition of `a ?? b` / `a ??= b`: `a` is not nullish.
        if self.is_ref(w, cond) {
            return if assume_true {
                self.non_nullable(t)
            } else {
                self.filter_type(t, |ck, m| {
                    ck.store().flags(m).intersects(
                        TypeFlags::NULLABLE
                            | TypeFlags::VOID
                            | TypeFlags::ANY_OR_UNKNOWN
                            | TypeFlags::INSTANTIABLE,
                    )
                })
            };
        }
        if assume_true && self.optional_chain_contains_ref(w, cond) {
            return self.non_nullable(t);
        }
        t
    }

    fn narrow(&mut self, w: &Walk, t: TypeId, cond: Loc, assume_true: bool, depth: u32) -> TypeId {
        if depth > 32 {
            return t;
        }
        let file = cond.file;
        let ast = self.ast(file);
        let n = skip_parens(ast, cond.node);
        let at = |x: NodeId| Loc::new(file, x);
        match *ast.node(n) {
            Node::Ident { .. }
            | Node::This {}
            | Node::Member { .. }
            | Node::Index { .. }
            | Node::NonNull { .. } => {
                if self.is_ref(w, at(n)) {
                    return self.narrow_truthiness(t, assume_true);
                }
                // Only a discriminant of the type here narrows (not `{ timer: undefined }`).
                if let Some(prop) = self.discriminant_of(w, at(n))
                    && self.is_discriminant_here(w, t, &prop)
                {
                    let r = self.narrow_by_discriminant(t, &prop, |ck, pt| {
                        ck.narrow_truthiness(pt, assume_true)
                    });
                    if assume_true && self.optional_chain_contains_ref(w, at(n)) {
                        return self.non_nullable(r);
                    }
                    return r;
                }
                if assume_true && self.optional_chain_contains_ref(w, at(n)) {
                    return self.non_nullable(t);
                }
                // An aliased condition (`const isS = typeof x === "string"; if (isS)`)
                // narrows a constant reference as the condition itself, up to five levels.
                if matches!(ast.node(n), Node::Ident { .. })
                    && self.cond_alias_depth < 5
                    && let Some(init) = self.condition_alias_init(at(n))
                    && self.is_constant_reference(w)
                {
                    self.cond_alias_depth += 1;
                    let r = self.narrow(w, t, init, assume_true, depth + 1);
                    self.cond_alias_depth -= 1;
                    return r;
                }
                t
            }
            Node::Call { callee, args, .. } => {
                self.narrow_by_call(w, t, at(n), callee, args, assume_true)
            }
            Node::Unary {
                op: UnaryOp::Not,
                arg,
            } => self.narrow(w, t, at(arg), !assume_true, depth + 1),
            Node::Assign { left, op, right } => {
                // `if (x = f())`, `if (x ??= f())`: the assigned value's truthiness.
                if self.is_ref(w, at(left))
                    && matches!(
                        op,
                        AssignOp::Assign
                            | AssignOp::AndAssign
                            | AssignOp::OrAssign
                            | AssignOp::CoalesceAssign
                    )
                {
                    return self.narrow_truthiness(t, assume_true);
                }
                // `if (x = y)` narrows `y` as the condition `y`; `if (x &&= y)` true means
                // `y` was evaluated and truthy (`||=`/`??=` may not evaluate it).
                if op == AssignOp::Assign || (op == AssignOp::AndAssign && assume_true) {
                    return self.narrow(w, t, at(right), assume_true, depth + 1);
                }
                t
            }
            Node::Binary { left, op, right } => match op {
                BinaryOp::LogicalAnd => {
                    if assume_true {
                        let l = self.narrow(w, t, at(left), true, depth + 1);
                        self.narrow(w, l, at(right), true, depth + 1)
                    } else {
                        let a = self.narrow(w, t, at(left), false, depth + 1);
                        let l = self.narrow(w, t, at(left), true, depth + 1);
                        let b = self.narrow(w, l, at(right), false, depth + 1);
                        self.union_flow_pair(a, b)
                    }
                }
                BinaryOp::LogicalOr => {
                    if assume_true {
                        let a = self.narrow(w, t, at(left), true, depth + 1);
                        let l = self.narrow(w, t, at(left), false, depth + 1);
                        let b = self.narrow(w, l, at(right), true, depth + 1);
                        self.union_flow_pair(a, b)
                    } else {
                        let l = self.narrow(w, t, at(left), false, depth + 1);
                        self.narrow(w, l, at(right), false, depth + 1)
                    }
                }
                BinaryOp::StrictEq | BinaryOp::StrictNotEq | BinaryOp::Eq | BinaryOp::NotEq => {
                    let strict = matches!(op, BinaryOp::StrictEq | BinaryOp::StrictNotEq);
                    let positive = matches!(op, BinaryOp::StrictEq | BinaryOp::Eq);
                    let assume_eq = positive == assume_true;
                    // `isObject(o) === false`: a condition compared with a boolean literal
                    // narrows as the condition (negated for `false`).
                    let (l, r) = (skip_parens(ast, left), skip_parens(ast, right));
                    let lit = match (ast.node(l), ast.node(r)) {
                        (_, &Node::BoolLit { value }) => Some((l, value)),
                        (&Node::BoolLit { value }, _) => Some((r, value)),
                        _ => None,
                    };
                    // (References keep equality narrowing: `x.ok === true` is a discriminant.)
                    if let Some((other, value)) = lit
                        && !matches!(
                            ast.node(other),
                            Node::BoolLit { .. }
                                | Node::Ident { .. }
                                | Node::This {}
                                | Node::Member { .. }
                                | Node::Index { .. }
                                | Node::NonNull { .. }
                        )
                    {
                        return self.narrow(w, t, at(other), value == assume_eq, depth + 1);
                    }
                    // `x.constructor === C` narrows `x` to what `C` constructs (true branch
                    // only).
                    if let Some(ctor) = if self.is_constructor_of_ref(w, at(l)) {
                        Some(r)
                    } else if self.is_constructor_of_ref(w, at(r)) {
                        Some(l)
                    } else {
                        None
                    } {
                        if !assume_eq {
                            return t;
                        }
                        return self.narrow_by_constructor(t, at(ctor));
                    }
                    self.narrow_by_equality(w, t, at(left), at(right), strict, assume_eq)
                }
                BinaryOp::InstanceOf => {
                    if !self.is_ref(w, at(left)) {
                        if assume_true && self.optional_chain_contains_ref(w, at(left)) {
                            return self.non_nullable(t);
                        }
                        return t;
                    }
                    self.narrow_by_instanceof(t, at(right), assume_true)
                }
                BinaryOp::In => {
                    let l = skip_parens(ast, left);
                    let name = match *ast.node(l) {
                        Node::StringLit { .. } | Node::NumberLit { .. } => key_text(ast, l),
                        Node::TemplateLit { parts } => match ast.list(parts) {
                            [p] => match *ast.node(*p) {
                                Node::TemplateElement { cooked, .. } => {
                                    Some(ast.atom(cooked).to_owned())
                                }
                                _ => None,
                            },
                            _ => None,
                        },
                        // `sym in x`, `K.a in x`: a name whose type is a literal or a unique
                        // symbol.
                        Node::Ident { .. } | Node::Member { .. } => {
                            let kt = self.quietly(|ck| ck.check_expr(at(l)));
                            let kt = self.store().regular(kt);
                            if self.store().flags(kt).intersects(
                                TypeFlags::STRING_LITERAL
                                    | TypeFlags::NUMBER_LITERAL
                                    | TypeFlags::UNIQUE_ES_SYMBOL,
                            ) {
                                self.property_name_of_type(kt)
                            } else {
                                None
                            }
                        }
                        _ => None,
                    };
                    if !self.is_ref(w, at(right)) {
                        // `'p' in o` narrows `o.p` (its optionality) too.
                        return match name {
                            Some(nm) if self.is_property_of_ref(w, at(right), &nm) => {
                                self.narrow_by_presence(t, assume_true)
                            }
                            _ => t,
                        };
                    }
                    match name {
                        Some(nm) => self.narrow_by_in(t, &nm, assume_true),
                        None => t,
                    }
                }
                BinaryOp::Coalesce => t,
                _ => t,
            },
            Node::Seq { .. } => t,
            _ => t,
        }
    }

    fn union_flow_pair(&mut self, a: TypeId, b: TypeId) -> TypeId {
        if a == b {
            return a;
        }
        self.union_with(
            &[a, b],
            crate::unions::Reduction::Literal,
            ts_r_types::AliasId::NONE,
        )
    }

    pub fn narrow_truthiness(&mut self, t: TypeId, assume_true: bool) -> TypeId {
        if assume_true {
            if t == TypeId::UNKNOWN {
                // A truthy `unknown` is some non-nullish value: `{}`.
                return if self.c.opts.strict_null_checks {
                    TypeId::EMPTY_OBJECT
                } else {
                    t
                };
            }
            let r = self.remove_definitely_falsy(t);
            self.non_nullable_generic_members(r)
        } else {
            let s = self.store();
            if s.flags(t).intersects(TypeFlags::ANY_OR_UNKNOWN) {
                return t;
            }
            self.filter_falsy_candidates(t)
        }
    }

    /// The members of `t` that can be falsy, with primitives narrowed to their falsy values
    /// for literal-only cases kept as is (tsc keeps `string`, `number` in the false branch
    /// as `string`/`number`, narrowing only literal and boolean members).
    fn filter_falsy_candidates(&mut self, t: TypeId) -> TypeId {
        self.map_type(t, |ck, m| {
            if m == TypeId::BOOLEAN {
                return TypeId::FALSE;
            }
            if ck.truthiness(m).falsy {
                m
            } else {
                TypeId::NEVER
            }
        })
    }

    fn narrow_by_discriminant(
        &mut self,
        t: TypeId,
        prop: &str,
        mut f: impl FnMut(&mut Self, TypeId) -> TypeId,
    ) -> TypeId {
        let s = self.store();
        // A single object (a union already narrowed to one member) is filtered too.
        let members = if s.flags(t).contains(TypeFlags::UNION) {
            s.members_of_union(t).to_vec()
        } else if s.flags(t).contains(TypeFlags::OBJECT)
            && let Some(p) = self.property_of_type(t, prop)
            && {
                let pt = self.type_of_symbol(p);
                self.store().flags(pt).intersects(TypeFlags::UNIT)
            }
        {
            vec![t]
        } else {
            return t;
        };
        let mut keep = Vec::with_capacity(members.len());
        let mut any_prop = false;
        for m in members {
            let pt = if self
                .store()
                .flags(m)
                .intersects(TypeFlags::OBJECT | TypeFlags::INTERSECTION)
            {
                match self.property_of_type(m, prop) {
                    Some(p) => {
                        any_prop = true;
                        Some(self.type_of_symbol(p))
                    }
                    None => Some(TypeId::UNDEFINED),
                }
            } else {
                None
            };
            match pt {
                Some(pt) => {
                    if f(self, pt) != TypeId::NEVER {
                        keep.push(m);
                    }
                }
                None => keep.push(m),
            }
        }
        if !any_prop {
            return t;
        }
        self.union(&keep)
    }

    fn narrow_by_equality(
        &mut self,
        w: &Walk,
        t: TypeId,
        left: Loc,
        right: Loc,
        strict: bool,
        assume_eq: bool,
    ) -> TypeId {
        let ast = self.ast(left.file);
        // `typeof ref === "..."`
        for (a, b) in [(left, right), (right, left)] {
            let an = skip_parens(ast, a.node);
            if let Node::Unary {
                op: UnaryOp::TypeOf,
                arg,
            } = *ast.node(an)
            {
                let bn = skip_parens(ast, b.node);
                let lit = match *ast.node(bn) {
                    Node::StringLit { value } => Some(ast.atom(value).to_owned()),
                    Node::TemplateLit { parts } if ast.list(parts).len() == 1 => match *ast
                        .node(ast.list(parts)[0])
                    {
                        Node::TemplateElement { cooked, .. } => Some(ast.atom(cooked).to_owned()),
                        _ => None,
                    },
                    _ => None,
                };
                let Some(lit) = lit else { return t };
                let argl = Loc::new(a.file, arg);
                if self.is_ref(w, argl) {
                    return self.narrow_by_typeof(t, &lit, assume_eq);
                }
                // `typeof ref?.a === "string"`: the chain reached `a`, so `ref` is not nullish.
                let defined = (assume_eq != (lit == "undefined"))
                    && self.optional_chain_contains_ref_eq(w, argl);
                if let Some(prop) = self.discriminant_of(w, argl) {
                    let r = if self.is_discriminant_here(w, t, &prop) {
                        self.narrow_by_discriminant(t, &prop, |ck, pt| {
                            ck.narrow_by_typeof(pt, &lit, assume_eq)
                        })
                    } else {
                        t
                    };
                    return if defined { self.non_nullable(r) } else { r };
                }
                return if defined { self.non_nullable(t) } else { t };
            }
        }
        for (a, b) in [(left, right), (right, left)] {
            if self.is_ref(w, a) {
                let vt = self.quietly(|ck| ck.check_expr(b));
                return self.narrow_type_by_equality(t, vt, strict, assume_eq);
            }
        }
        for (a, b) in [(left, right), (right, left)] {
            if let Some(prop) = self.discriminant_of(w, a) {
                let vt = self.quietly(|ck| ck.check_expr(b));
                // Only a discriminant of the type here narrows it (`config.x === null`
                // with `config: { x: null }` leaves `config` alone).
                let r = if self.is_discriminant_here(w, t, &prop) {
                    self.narrow_by_discriminant(t, &prop, |ck, pt| {
                        ck.narrow_type_by_equality(pt, vt, strict, assume_eq)
                    })
                } else {
                    t
                };
                if assume_eq
                    && self.optional_chain_contains_ref_eq(w, a)
                    && !self.may_be_undefined(vt, strict)
                {
                    return self.non_nullable(r);
                }
                // `ref?.p !== undefined`: the chain reached `p`, so `ref` is not nullish.
                if !assume_eq
                    && self.is_undefined_like(vt, strict)
                    && self.optional_chain_contains_ref_eq(w, a)
                {
                    return self.non_nullable(r);
                }
                return r;
            }
            if self.optional_chain_contains_ref_eq(w, a) {
                let vt = self.quietly(|ck| ck.check_expr(b));
                if assume_eq && !self.may_be_undefined(vt, strict) {
                    return self.non_nullable(t);
                }
                if !assume_eq && self.is_undefined_like(vt, strict) {
                    return self.non_nullable(t);
                }
                return t;
            }
        }
        t
    }

    fn may_be_undefined(&self, vt: TypeId, strict: bool) -> bool {
        let s = self.store();
        // Type parameters and other generic values do not count (observed:
        // `ref?.by === column` with `column: U` or `U extends string | undefined` narrows).
        let check = |m: TypeId| {
            let f = s.flags(m);
            f.intersects(TypeFlags::UNDEFINED | TypeFlags::VOID | TypeFlags::ANY_OR_UNKNOWN)
                || (!strict && f.contains(TypeFlags::NULL))
        };
        if s.flags(vt).contains(TypeFlags::UNION) {
            s.members_of_union(vt).iter().any(|&m| check(m))
        } else {
            check(vt)
        }
    }

    fn is_undefined_like(&self, vt: TypeId, strict: bool) -> bool {
        let f = self.store().flags(vt);
        f.contains(TypeFlags::UNDEFINED) || (!strict && f.contains(TypeFlags::NULL))
    }

    /// Narrows `t` by `ref === v` (`assume_eq`) or `ref !== v`.
    pub fn narrow_type_by_equality(
        &mut self,
        t: TypeId,
        vt: TypeId,
        strict: bool,
        assume_eq: bool,
    ) -> TypeId {
        let s = self.store();
        let tf = s.flags(t);
        if tf.contains(TypeFlags::ANY) {
            return t;
        }
        let vt = self.regular_of(vt);
        let vf = self.store().flags(vt);
        if vf.intersects(TypeFlags::ANY_OR_UNKNOWN) {
            return t;
        }
        // `== null` / `== undefined` (loose) match both.
        if !strict && vf.intersects(TypeFlags::NULLABLE) {
            return if assume_eq {
                if t == TypeId::UNKNOWN {
                    return self.union2(TypeId::NULL, TypeId::UNDEFINED);
                }
                self.filter_type(t, |ck, m| {
                    ck.store().flags(m).intersects(
                        TypeFlags::NULLABLE
                            | TypeFlags::VOID
                            | TypeFlags::ANY_OR_UNKNOWN
                            | TypeFlags::INSTANTIABLE,
                    )
                })
            } else if t == TypeId::UNKNOWN && self.c.opts.strict_null_checks {
                TypeId::EMPTY_OBJECT
            } else {
                self.non_nullable(t)
            };
        }
        if assume_eq {
            // `x === v` where `x` is `unknown` or has a `{}` member (`!!u` of an `unknown`):
            // a primitive or `object` `v` is the result, any other object type gives
            // `object`; unions, intersections and generic `v` filter as below (observed
            // with tsc 7.0.2).
            if strict
                && (t == TypeId::UNKNOWN
                    || self
                        .union_members(t)
                        .into_iter()
                        .any(|m| self.is_empty_anonymous_object(m)))
            {
                let vf = self.store().flags(vt);
                if vf.intersects(TypeFlags::PRIMITIVE | TypeFlags::NON_PRIMITIVE)
                    || self.is_empty_anonymous_object(vt)
                {
                    return vt;
                }
                if vf.contains(TypeFlags::OBJECT) {
                    return TypeId::OBJECT;
                }
            }
            if t == TypeId::UNKNOWN {
                // `u === x` narrows `unknown` to the type of `x`, unless that is a union or
                // intersection. A loose comparison never does (`u == 42`, `u == E.A`:
                // observed), but for `null`/`undefined` above.
                let narrows = strict
                    && (self.is_unit(vt)
                        || vt == TypeId::BOOLEAN
                        || !self.store().flags(vt).intersects(
                            TypeFlags::UNION_OR_INTERSECTION | TypeFlags::INSTANTIABLE,
                        ));
                return if narrows { vt } else { t };
            }
            let filtered = self.filter_type(t, |ck, m| {
                let mf = ck.store().flags(m);
                // A template literal or string mapping of no type variable is a set of
                // strings, compared like one: `${string}!` is not `undefined`.
                let concrete_pattern = mf
                    .intersects(TypeFlags::TEMPLATE_LITERAL | TypeFlags::STRING_MAPPING)
                    && !ck.is_generic_type(m);
                if !concrete_pattern
                    && mf.intersects(TypeFlags::INSTANTIABLE | TypeFlags::ANY_OR_UNKNOWN)
                {
                    return true;
                }
                // `==` also keeps what coerces to the value: `level == +level` keeps the
                // `string` of `level: string | number` (tsc: `isCoercibleUnderDoubleEquals`
                // in flow.go).
                let coercible = !strict
                    && mf.intersects(
                        TypeFlags::NUMBER | TypeFlags::STRING | TypeFlags::BOOLEAN_LITERAL,
                    )
                    && (vt == TypeId::BOOLEAN
                        || ck.store().flags(vt).intersects(
                            TypeFlags::NUMBER | TypeFlags::STRING | TypeFlags::BOOLEAN,
                        ));
                coercible || ck.is_comparable(m, vt) || ck.is_comparable(vt, m)
            });
            if self.is_unit(vt) {
                // Replace primitives with the literal (`string === "a"` → `"a"`).
                let v = vt;
                return self.map_type(filtered, |ck, m| {
                    if ck.is_unit(m) {
                        return m;
                    }
                    let mf = ck.store().flags(m);
                    if mf.intersects(
                        TypeFlags::STRING
                            | TypeFlags::NUMBER
                            | TypeFlags::BIGINT
                            | TypeFlags::BOOLEAN
                            | TypeFlags::ENUM
                            | TypeFlags::ES_SYMBOL,
                    ) && ck.is_assignable(v, m)
                    {
                        return v;
                    }
                    // A narrowed `unknown` (`{}`) compared to a literal is that literal.
                    if ck.is_empty_anonymous_object(m)
                        && !ck.store().flags(v).intersects(TypeFlags::NULLABLE)
                    {
                        return v;
                    }
                    m
                });
            }
            filtered
        } else {
            if !self.is_unit(vt) {
                return t;
            }
            let v = vt;
            // `unknown !== null` is `{} | undefined` (and the other way round).
            if t == TypeId::UNKNOWN && self.c.opts.strict_null_checks {
                if v == TypeId::NULL {
                    return self.union2(TypeId::EMPTY_OBJECT, TypeId::UNDEFINED);
                }
                if v == TypeId::UNDEFINED {
                    return self.union2(TypeId::EMPTY_OBJECT, TypeId::NULL);
                }
                return t;
            }
            self.map_type(t, |ck, m| {
                if m == TypeId::BOOLEAN {
                    if v == TypeId::TRUE {
                        return TypeId::FALSE;
                    }
                    if v == TypeId::FALSE {
                        return TypeId::TRUE;
                    }
                    return m;
                }
                if ck.is_unit(m) {
                    let r = ck.regular_of(m);
                    if r == v {
                        return TypeId::NEVER;
                    }
                    // An enum member and a literal of its value are the same value
                    // (`e !== "A"` removes `E.A`).
                    let (rf, vf) = (ck.store().flags(r), ck.store().flags(v));
                    let lit = TypeFlags::STRING_LITERAL | TypeFlags::NUMBER_LITERAL;
                    if (rf.contains(TypeFlags::ENUM_LITERAL)
                        != vf.contains(TypeFlags::ENUM_LITERAL))
                        && rf.intersects(lit)
                        && vf.intersects(lit)
                        && ck.is_comparable(r, v)
                        && ck.is_comparable(v, r)
                    {
                        return TypeId::NEVER;
                    }
                    // `void`, `undefined` and an absent optional property (the missing type
                    // under exactOptionalPropertyTypes) are the same value.
                    if v == TypeId::UNDEFINED
                        && (r == TypeId::MISSING || ck.store().flags(m).contains(TypeFlags::VOID))
                    {
                        return TypeId::NEVER;
                    }
                }
                // An intersection with the value as a member is only that value: `T & null`
                // (`typeof x === "object" && x !== null`), a branded `"a" & { b: 1 }`.
                if ck.store().flags(m).contains(TypeFlags::INTERSECTION)
                    && ck
                        .store()
                        .members_of_union(m)
                        .iter()
                        .any(|&x| x == v || (ck.is_unit(x) && ck.regular_of(x) == v))
                {
                    return TypeId::NEVER;
                }
                m
            })
        }
    }

    /// Possible `typeof` results of a type.
    fn typeof_facts(&mut self, t: TypeId, depth: u32) -> u16 {
        if depth > 16 {
            return facts::ALL;
        }
        let s = self.store();
        let f = s.flags(t);
        if f.contains(TypeFlags::UNION) {
            let ms = s.members_of_union(t).to_vec();
            return ms
                .into_iter()
                .fold(0, |acc, m| acc | self.typeof_facts(m, depth + 1));
        }
        if f.contains(TypeFlags::INTERSECTION) {
            // Object members of an intersection with a primitive are tags (`string & {
            // __brand: 1 }` is a string), and object members of one with a function type
            // are that function (`(() => void) & { a: 1 }`).
            let ms = s.members_of_union(t).to_vec();
            let fs: Vec<u16> = ms
                .into_iter()
                .map(|m| self.typeof_facts(m, depth + 1))
                .collect();
            const OBJ: u16 = facts::OBJECT | facts::FUNCTION;
            let has_prim = fs.iter().any(|&f| f != 0 && f & OBJ == 0);
            let has_fn = fs.contains(&facts::FUNCTION);
            return fs.into_iter().fold(facts::ALL, |acc, f| {
                if has_prim && f & !OBJ == 0 {
                    acc
                } else if has_fn && f == facts::OBJECT {
                    acc & OBJ
                } else {
                    acc & f
                }
            });
        }
        if f.intersects(TypeFlags::ANY_OR_UNKNOWN) {
            return facts::ALL;
        }
        if f.contains(TypeFlags::NEVER) {
            return 0;
        }
        if f.intersects(TypeFlags::STRING_LIKE) {
            return facts::STRING;
        }
        if f.contains(TypeFlags::ENUM_LITERAL) {
            return if s.string_value(t).is_some() {
                facts::STRING
            } else {
                facts::NUMBER
            };
        }
        if f.intersects(TypeFlags::NUMBER_LIKE) {
            return facts::NUMBER;
        }
        if f.intersects(TypeFlags::BIGINT_LIKE) {
            return facts::BIGINT;
        }
        if f.intersects(TypeFlags::BOOLEAN_LIKE) {
            return facts::BOOLEAN;
        }
        if f.intersects(TypeFlags::ES_SYMBOL_LIKE) {
            return facts::SYMBOL;
        }
        if f.intersects(TypeFlags::VOID_LIKE) {
            return facts::UNDEFINED;
        }
        if f.contains(TypeFlags::NULL) {
            return facts::OBJECT;
        }
        if f.contains(TypeFlags::NON_PRIMITIVE) {
            return facts::OBJECT | facts::FUNCTION;
        }
        if f.intersects(TypeFlags::INSTANTIABLE) {
            let c = self.base_constraint(t);
            if c == t || c == TypeId::UNKNOWN || c == TypeId::UNRESOLVED {
                return facts::ALL;
            }
            return self.typeof_facts(c, depth + 1);
        }
        if f.contains(TypeFlags::OBJECT) {
            if !self.signatures_of(t, SigKind::Call).is_empty()
                || !self.signatures_of(t, SigKind::Construct).is_empty()
            {
                return facts::FUNCTION;
            }
            if let Some(ft) = self.global_type(GlobalType::Function)
                && (t == ft || self.is_function_interface_ref(t, ft))
            {
                return facts::FUNCTION;
            }
            if self.is_empty_object_type(t) {
                return facts::NON_NULLISH;
            }
            return facts::OBJECT;
        }
        facts::ALL
    }

    fn is_function_interface_ref(&self, t: TypeId, ft: TypeId) -> bool {
        matches!(self.store().object_of(t).map(|o| o.kind), Some(ObjectKind::Reference { target, .. }) if target == ft)
    }

    pub fn narrow_by_typeof(&mut self, t: TypeId, lit: &str, assume_eq: bool) -> TypeId {
        let Some(fact) = typeof_fact(lit) else {
            // Another name (`"Object"`): only an object can be a host object with it, so
            // the true branch keeps the object members (`unknown` is `object`) and the
            // false branch the others.
            if self.store().flags(t).contains(TypeFlags::ANY) {
                return t;
            }
            if t == TypeId::UNKNOWN {
                return if assume_eq { TypeId::OBJECT } else { t };
            }
            return self.filter_type(t, |ck, m| {
                let object = m != TypeId::NULL
                    && ck.typeof_facts(m, 0) & !(facts::OBJECT | facts::FUNCTION) == 0;
                object == assume_eq
            });
        };
        let prim = match fact {
            facts::STRING => Some(TypeId::STRING),
            facts::NUMBER => Some(TypeId::NUMBER),
            facts::BIGINT => Some(TypeId::BIGINT),
            facts::BOOLEAN => Some(TypeId::BOOLEAN),
            facts::SYMBOL => Some(TypeId::SYMBOL),
            facts::UNDEFINED => Some(TypeId::UNDEFINED),
            _ => None,
        };
        if assume_eq {
            if t == TypeId::UNKNOWN {
                return match fact {
                    facts::OBJECT => self.union2(TypeId::OBJECT, TypeId::NULL),
                    facts::FUNCTION => self.global_type(GlobalType::Function).unwrap_or(t),
                    _ => prim.unwrap_or(t),
                };
            }
            if self.store().flags(t).contains(TypeFlags::ANY) {
                return match prim {
                    Some(p) => p,
                    None => t,
                };
            }
            self.map_type(t, |ck, m| {
                let f = ck.typeof_facts(m, 0);
                if f & fact == 0 {
                    // An object type the primitive (or `Function`) is assignable to narrows
                    // to it (`{ toString(): string }` by `"string"` is `string`).
                    // (A generic mapped type only relates to another one.)
                    if ck.store().flags(m).contains(TypeFlags::OBJECT)
                        && fact != facts::UNDEFINED
                        && !ck.is_generic_mapped_type(m)
                    {
                        let p = match fact {
                            facts::FUNCTION => ck.global_type(GlobalType::Function),
                            _ => prim,
                        };
                        if let Some(p) = p
                            && ck.is_assignable(p, m)
                        {
                            return p;
                        }
                    }
                    return TypeId::NEVER;
                }
                if f == fact {
                    return m;
                }
                let mf = ck.store().flags(m);
                if mf.intersects(TypeFlags::ANY_OR_UNKNOWN) {
                    return match fact {
                        facts::OBJECT => ck.union2(TypeId::OBJECT, TypeId::NULL),
                        facts::FUNCTION => ck.global_type(GlobalType::Function).unwrap_or(m),
                        _ => prim.unwrap_or(m),
                    };
                }
                let generic = mf.intersects(TypeFlags::INSTANTIABLE)
                    || mf.contains(TypeFlags::INTERSECTION) && {
                        let ms = ck.store().members_of_union(m).to_vec();
                        ms.iter()
                            .any(|&x| ck.store().flags(x).intersects(TypeFlags::INSTANTIABLE))
                    };
                if generic {
                    return match prim {
                        Some(p) => ck.intersection(&[m, p]),
                        // `typeof t === "object"` on a generic `T` is `(T & null) | (T &
                        // object)` (each part its constraint allows); `"function"` is
                        // `T & Function`.
                        None if fact == facts::OBJECT => {
                            let c = ck.base_constraint(m);
                            let mut parts = Vec::with_capacity(2);
                            for p in [TypeId::NULL, TypeId::OBJECT] {
                                if c == TypeId::UNKNOWN
                                    || c == TypeId::UNRESOLVED
                                    || ck.is_comparable(p, c)
                                {
                                    parts.push(ck.intersection(&[m, p]));
                                }
                            }
                            ck.union(&parts)
                        }
                        None if fact == facts::FUNCTION => {
                            match ck.global_type(GlobalType::Function) {
                                Some(func) => ck.intersection(&[m, func]),
                                None => m,
                            }
                        }
                        None => m,
                    };
                }
                if mf.contains(TypeFlags::NON_PRIMITIVE) {
                    // `object` by `"function"` is `Function`.
                    if fact == facts::FUNCTION
                        && let Some(func) = ck.global_type(GlobalType::Function)
                    {
                        return func;
                    }
                    return m;
                }
                // `{}` and other multi-fact object types.
                match prim {
                    Some(p) => p,
                    None if fact == facts::OBJECT && ck.is_empty_anonymous_object(m) => {
                        TypeId::OBJECT
                    }
                    None if fact == facts::FUNCTION && ck.is_empty_anonymous_object(m) => {
                        ck.global_type(GlobalType::Function).unwrap_or(m)
                    }
                    None => m,
                }
            })
        } else {
            if self.store().flags(t).intersects(TypeFlags::ANY_OR_UNKNOWN) {
                return t;
            }
            self.map_type(t, |ck, m| {
                let f = ck.removable_typeof_facts(m);
                if f != 0 && f & !fact == 0 {
                    TypeId::NEVER
                } else {
                    m
                }
            })
        }
    }

    /// The `typeof` results whose exclusion removes `m`: its `typeof` facts, except that
    /// `object` goes by `"object"` alone (observed: `typeof o !== "object"` on `o: object`
    /// is `never`, though `typeof o === "function"` keeps a `Function`).
    fn removable_typeof_facts(&mut self, m: TypeId) -> u16 {
        if self.store().flags(m).contains(TypeFlags::NON_PRIMITIVE) {
            return facts::OBJECT;
        }
        self.typeof_facts(m, 0)
    }

    fn narrow_by_instanceof(&mut self, t: TypeId, right: Loc, assume_true: bool) -> TypeId {
        let rt = self.quietly(|ck| ck.check_expr(right));
        if self.store().flags(rt).intersects(TypeFlags::ANY_OR_UNKNOWN) {
            return t;
        }
        let Some(candidate) = self.instance_type_of_constructor(rt) else {
            return t;
        };
        if assume_true {
            self.narrow_by_instance_type(t, candidate)
        } else {
            // A union of constructors (`typeof A | typeof B`) does not narrow the false
            // branch (observed).
            if self.store().flags(t).intersects(TypeFlags::ANY_OR_UNKNOWN)
                || self.store().flags(candidate).contains(TypeFlags::UNION)
                || candidate == TypeId::EMPTY_OBJECT
            {
                return t;
            }
            // A member goes when it (a generic one: its constraint) derives from the
            // instance type.
            self.filter_type(t, |ck, m| {
                let mf = ck.store().flags(m);
                if mf.intersects(TypeFlags::ANY_OR_UNKNOWN) {
                    return true;
                }
                let k = if mf.intersects(TypeFlags::INSTANTIABLE) {
                    let k = ck.base_constraint(m);
                    if k == m || k == TypeId::UNKNOWN || k == TypeId::UNRESOLVED {
                        return true;
                    }
                    k
                } else {
                    m
                };
                ck.instance_relation(k, candidate) != Some(true)
            })
        }
    }

    /// The true branch of `x instanceof C` (observed rules). Object members that derive
    /// from the instance type stay (`instance_relation`: subclasses, arrays and tuples for
    /// `Array`, non-class objects by assignability); only with none, instance types that
    /// are subtypes of members replace them (`{}` is `Object`). Primitives always go.
    /// With nothing left, generic members intersect with the instance type; then the
    /// instance type wins if assignable to the whole type (`A` by a structurally
    /// identical class `B` is `B`), the whole type stays if assignable to it (`string`
    /// by `Object`), or they intersect (`(string | C) & A`).
    fn narrow_by_instance_type(&mut self, t: TypeId, candidate: TypeId) -> TypeId {
        if self.store().flags(t).intersects(TypeFlags::ANY_OR_UNKNOWN) {
            // `any` is not narrowed to `Object` or `Function`.
            if self.store().flags(t).contains(TypeFlags::ANY)
                && [GlobalType::Object, GlobalType::Function]
                    .into_iter()
                    .any(|g| self.global_type(g) == Some(candidate))
            {
                return t;
            }
            return candidate;
        }
        // A constructor returning `any` does not narrow.
        if self.store().flags(candidate).contains(TypeFlags::ANY) {
            return t;
        }
        let members = self.union_members(t);
        let candidates = self.union_members(candidate);
        // A generic member relates through its constraint: it stays, or intersects.
        let mut related = Vec::with_capacity(members.len());
        for &m in &members {
            let generic = self.store().flags(m).intersects(TypeFlags::INSTANTIABLE);
            let k = if generic {
                let k = self.base_constraint(m);
                if k == m || k == TypeId::UNKNOWN || k == TypeId::UNRESOLVED {
                    continue;
                }
                k
            } else {
                m
            };
            related.push((m, generic, self.union_members(k)));
        }
        // Members that derive from the instance type stay; only with none, instance types
        // that are subtypes of members replace them.
        let mut out = Vec::new();
        for (m, _, ks) in &related {
            if ks.iter().any(|&k| {
                candidates
                    .iter()
                    .any(|&c| self.instance_relation(k, c) == Some(true))
            }) {
                out.push(*m);
            }
        }
        if out.is_empty() {
            for (m, generic, ks) in &related {
                for &k in ks {
                    for &c in &candidates {
                        if self.instance_relation(k, c) == Some(false) {
                            out.push(if *generic {
                                self.intersection(&[*m, c])
                            } else {
                                c
                            });
                        }
                    }
                }
            }
        }
        if out.is_empty() {
            for &m in &members {
                if self.store().flags(m).intersects(TypeFlags::INSTANTIABLE)
                    || self.intersection_has_generic(m)
                {
                    let c = self.base_constraint(m);
                    if c == m || c == TypeId::UNKNOWN || self.is_comparable(candidate, c) {
                        out.push(self.intersection(&[m, candidate]));
                    }
                }
            }
        }
        if !out.is_empty() {
            return self.union(&out);
        }
        if self.is_assignable(candidate, t) {
            return candidate;
        }
        if self.is_assignable(t, candidate) {
            return t;
        }
        self.intersection(&[t, candidate])
    }

    /// How a member `m` relates to the instance type `c` of `instanceof`: `Some(true)` it
    /// stays, `Some(false)` it becomes `c`, `None` it goes.
    /// Whether `l` is `ref.constructor` or `ref["constructor"]`.
    fn is_constructor_of_ref(&mut self, w: &Walk, l: Loc) -> bool {
        let ast = self.ast(l.file);
        let (obj, name) = match *ast.node(skip_parens(ast, l.node)) {
            Node::Member { obj, prop, .. } if matches!(ast.node(prop), Node::Ident { .. }) => {
                (obj, Some(self.c.text(Loc::new(l.file, prop)).to_owned()))
            }
            Node::Index { obj, index, .. }
                if matches!(ast.node(skip_parens(ast, index)), Node::StringLit { .. }) =>
            {
                (obj, key_text(ast, skip_parens(ast, index)))
            }
            _ => return false,
        };
        name.as_deref() == Some("constructor") && self.is_ref(w, Loc::new(l.file, obj))
    }

    /// `x.constructor === C`: the members of `t` that `C` constructs. A class instance is
    /// constructed by its own class only (not a base, not a same-shaped class); other
    /// members (through their apparent type for primitives) when `C`'s instance type is a
    /// subtype of them.
    fn narrow_by_constructor(&mut self, t: TypeId, ctor: Loc) -> TypeId {
        if self.store().flags(t).intersects(TypeFlags::ANY) {
            return t;
        }
        let ct = self.quietly(|ck| ck.check_expr(ctor));
        let Some(candidate) = self.instance_type_of_constructor(ct) else {
            return t;
        };
        if self.store().flags(candidate).intersects(TypeFlags::ANY) {
            return t;
        }
        let cand_class = self.class_symbol_of_instance(candidate);
        self.filter_type(t, |ck, m| {
            if let Some(ms) = ck.class_symbol_of_instance(m) {
                return cand_class.is_some_and(|cs| {
                    ck.c.first_decl(ms).is_some() && ck.c.first_decl(ms) == ck.c.first_decl(cs)
                });
            }
            if ck
                .store()
                .flags(m)
                .intersects(TypeFlags::NULLABLE | TypeFlags::NEVER)
            {
                return false;
            }
            let am = if ck.store().flags(m).intersects(TypeFlags::PRIMITIVE) {
                ck.apparent_type(m)
            } else {
                m
            };
            ck.is_subtype(candidate, am)
        })
    }

    fn instance_relation(&mut self, m: TypeId, c: TypeId) -> Option<bool> {
        let mf = self.store().flags(m);
        if !mf.intersects(TypeFlags::OBJECT | TypeFlags::INTERSECTION | TypeFlags::NON_PRIMITIVE) {
            return None;
        }
        let classes = self
            .class_symbol_of_instance(m)
            .zip(self.class_symbol_of_instance(c))
            .and_then(|(ms, cs)| self.c.first_decl(ms).zip(self.c.first_decl(cs)));
        if let Some((md, cd)) = classes {
            // Two classes relate by their declarations only.
            if md == cd || self.class_derives_from(md, cd) {
                return Some(true);
            }
            if self.class_derives_from(cd, md) {
                return Some(false);
            }
            return None;
        }
        // An intersection with a generic member derives when one of its members does (a
        // generic one through its constraint): `T & {}` does not derive from `Object`.
        if self.intersection_has_generic(m) {
            let parts = self.store().members_of_union(m).to_vec();
            for p in parts {
                let k = if self.store().flags(p).intersects(TypeFlags::INSTANTIABLE) {
                    self.base_constraint(p)
                } else {
                    p
                };
                if k == TypeId::UNKNOWN || k == TypeId::UNRESOLVED {
                    continue;
                }
                if self.instance_relation(k, c) == Some(true) {
                    return Some(true);
                }
            }
        } else if !self.is_empty_anonymous_object(m) && self.is_derived_from(m, c) {
            return Some(true);
        }
        if self.is_subtype(c, m) {
            return Some(false);
        }
        None
    }

    /// An intersection with an instantiable member (`T & {}`).
    fn intersection_has_generic(&self, m: TypeId) -> bool {
        let s = self.store();
        s.flags(m).contains(TypeFlags::INTERSECTION)
            && s.members_of_union(m)
                .iter()
                .any(|&p| s.flags(p).intersects(TypeFlags::INSTANTIABLE))
    }

    fn is_derived_from(&mut self, m: TypeId, c: TypeId) -> bool {
        // Arrays, readonly arrays and tuples derive from `Array`.
        let s = self.store();
        let mutable_array = match s.object_of(c).map(|o| o.kind) {
            Some(ObjectKind::Reference { target, .. }) => matches!(
                s.object_of(target).map(|o| o.kind),
                Some(ObjectKind::Declared { sym, .. }) if self.c.array_syms[0] == Some(sym)
            ),
            _ => false,
        };
        if mutable_array && self.c.is_array_like_type(m) {
            return true;
        }
        let ms = self.class_symbol_of_instance(m);
        let cs = self.class_symbol_of_instance(c);
        if let (Some(ms), Some(cs)) = (ms, cs)
            && let (Some(md), Some(cd)) = (self.c.first_decl(ms), self.c.first_decl(cs))
        {
            return md == cd || self.class_derives_from(md, cd);
        }
        // An object type that is not a class instance (an interface such as `Error`, an
        // object literal type) never derives from a class, even when structurally
        // identical (observed: `e instanceof TargetClosedError || e.name` keeps `Error`).
        if ms.is_none() && cs.is_some() && self.store().flags(m).contains(TypeFlags::OBJECT) {
            return false;
        }
        self.is_assignable(m, c)
    }

    /// The instance type of a constructor value (for `instanceof`).
    fn instance_type_of_constructor(&mut self, ct: TypeId) -> Option<TypeId> {
        // A `prototype` property that is not `any` wins over construct signatures.
        if let Some(p) = self.property_of_type(ct, "prototype") {
            let t = self.type_of_symbol(p);
            if !self.store().flags(t).intersects(TypeFlags::ANY_OR_UNKNOWN) {
                return Some(t);
            }
        }
        let sigs = self.signatures_of(ct, SigKind::Construct);
        if sigs.is_empty() {
            // A function value without construct signatures (`Function`, a plain or JS
            // function) creates objects of unknown shape: `{}`.
            let is_function = self.global_type(GlobalType::Function) == Some(ct)
                || !self.signatures_of(ct, SigKind::Call).is_empty();
            return is_function.then_some(TypeId::EMPTY_OBJECT);
        }
        let mut types = Vec::new();
        for s in sigs {
            let tps = self
                .store()
                .list_items(self.store().sig(s).type_params)
                .to_vec();
            let sig = if tps.is_empty() {
                s
            } else {
                let anys = vec![TypeId::ANY; tps.len()];
                let m = self.store().mapper(&tps, &anys);
                self.instantiate_signature_full(s, m)
            };
            let r = self.return_type_of(sig);
            if !types.contains(&r) {
                types.push(r);
            }
        }
        Some(self.union(&types))
    }

    /// Narrows `t` to `candidate` (type predicates and assertions).
    pub fn narrow_to_type(&mut self, t: TypeId, candidate: TypeId) -> TypeId {
        self.narrow_to_type_with(t, candidate, true)
    }

    /// Narrows `t` to `candidate`; `predicate` (not `instanceof`) prefers a candidate that
    /// is a strict subtype of a member over the member.
    fn narrow_to_type_with(&mut self, t: TypeId, candidate: TypeId, predicate: bool) -> TypeId {
        let f = self.store().flags(t);
        if f.intersects(TypeFlags::ANY_OR_UNKNOWN) {
            return candidate;
        }
        let members = self.union_members(t);
        let mut out = Vec::new();
        for &m in &members {
            // A deferred conditional relates through its base constraint; one with no
            // concrete constraint (a branch is `T`) relates to nothing here and intersects
            // below (`DeepPartial<T>` narrowed by `Array.isArray` is `DeepPartial<T> & any[]`).
            if self.store().flags(m).contains(TypeFlags::CONDITIONAL) {
                let c = self.base_constraint(m);
                if c == TypeId::UNRESOLVED || c == TypeId::UNKNOWN || self.is_generic_type(c) {
                    continue;
                }
            }
            // Type predicates relate by subtype: a member strictly more specific than the
            // candidate stays, a candidate strictly more specific than the member wins (`{}`
            // narrowed by `value is Record<string, unknown>`), then plain subtypes either
            // way (an `AnyFn` candidate is not a subtype of a generic `T["ctx"]`, though
            // assignable to it).
            if predicate {
                if self.is_strict_subtype(m, candidate) {
                    out.push(m);
                } else if m != candidate
                    && !self.store().flags(m).intersects(TypeFlags::INSTANTIABLE)
                    && self.is_strict_subtype(candidate, m)
                {
                    out.push(candidate);
                } else if self.is_subtype(m, candidate) {
                    out.push(m);
                } else if self.is_subtype(candidate, m) {
                    out.push(candidate);
                } else if self.is_assignable(m, candidate) {
                    // A member only assignable to the candidate still stays
                    // (`(s: string) => number` narrowed by `x is (...a: any[]) => any`).
                    out.push(m);
                }
            } else if self.is_assignable(m, candidate) {
                out.push(m);
            } else if self.is_assignable(candidate, m) {
                out.push(candidate);
            }
        }
        // Only with no member directly related, generic members intersect with the
        // candidate (`T | T[]` narrowed to `any[]` is `T[]`; `T | string` is `T & any[]`).
        if out.is_empty() {
            for &m in &members {
                if self.store().flags(m).intersects(TypeFlags::INSTANTIABLE) {
                    let c = self.base_constraint(m);
                    if c == m || c == TypeId::UNKNOWN || self.is_comparable(candidate, c) {
                        out.push(self.intersection(&[m, candidate]));
                    }
                }
            }
        }
        if !out.is_empty() {
            return self.union(&out);
        }
        // No member relates on its own: the candidate or the whole type when one fits the
        // other (`string | number | readonly X[]` narrowed by `Array.isArray` is `any[]`).
        if predicate {
            if self.is_subtype(candidate, t) {
                return candidate;
            }
            if self.is_assignable(t, candidate) {
                return t;
            }
            if self.is_assignable(candidate, t) {
                return candidate;
            }
            return self.intersection(&[t, candidate]);
        }
        if members.len() > 1 {
            return TypeId::NEVER;
        }
        self.intersection(&[t, candidate])
    }

    /// The object `{ name: unknown }` (printed by tsc as `Record<"name", unknown>`).
    fn in_record(&mut self, name: &str) -> TypeId {
        if let Some(&t) = self.in_records.get(name) {
            return t;
        }
        // The global `Record<"name", unknown>` when the lib has it (tsc prints the alias).
        if let Some(rec) = self.c.global_symbol("Record", meaning::TYPE)
            && self.c.sym_flags(rec).intersects(SymbolFlags::TYPE_ALIAS)
        {
            let local = self.local_type_params(rec);
            if local.len() == 2 {
                let key = self.property_key_type(name);
                let d = self.declared_type_of_symbol(rec);
                let m = self.store().mapper(&local, &[key, TypeId::UNKNOWN]);
                let t = self.instantiate(d, m);
                if !self.store().is_error(t) && t != TypeId::UNRESOLVED {
                    self.in_records.insert(name.to_owned(), t);
                    return t;
                }
            }
        }
        let s = self.store();
        let n = s.intern_str(name);
        let p = self.c.new_synth(
            crate::symbols::SynthSym::new(n, SymbolFlags::PROPERTY, 0).with_type(TypeId::UNKNOWN),
        );
        let t = s.literal_object(
            SymId::NONE,
            ts_r_types::ObjectFlags::ANONYMOUS,
            ts_r_types::ResolvedMembers::new(vec![(n, p)], Vec::new(), Vec::new(), Vec::new()),
            ts_r_types::TypeTraits::NONE,
        );
        self.in_records.insert(name.to_owned(), t);
        t
    }

    fn narrow_by_in(&mut self, t: TypeId, name: &str, assume_true: bool) -> TypeId {
        let s = self.store();
        if s.flags(t).contains(TypeFlags::ANY) {
            return t;
        }
        // `"p" in u` (an error for `unknown`) still narrows `u` to `Record<"p", unknown>`.
        if s.flags(t).contains(TypeFlags::UNKNOWN) {
            if !assume_true {
                return t;
            }
            let rec = self.in_record(name);
            return self.intersection(&[t, rec]);
        }
        let members = if s.flags(t).contains(TypeFlags::UNION) {
            s.members_of_union(t).to_vec()
        } else {
            vec![t]
        };
        // Each member: whether it may have `name` when the check is true (a property or an
        // applicable index signature) and when it is false (no required property).
        let mut presence = Vec::with_capacity(members.len());
        for &m in &members {
            let mf = self.store().flags(m);
            if !mf.intersects(
                TypeFlags::OBJECT
                    | TypeFlags::INTERSECTION
                    | TypeFlags::NON_PRIMITIVE
                    | TypeFlags::INSTANTIABLE,
            ) {
                // Primitives: `in` on them is an error; they stay as they are.
                presence.push(None);
                continue;
            }
            let prop = self.property_of_type(m, name);
            let required =
                prop.is_some_and(|p| !self.c.sym_flags(p).intersects(SymbolFlags::OPTIONAL));
            // A symbol key (`Symbol.iterator in x`) needs a symbol index signature.
            let key = if name.starts_with("__@") {
                TypeId::SYMBOL
            } else {
                TypeId::STRING
            };
            let has_index = prop.is_none() && self.index_info_of(m, key).is_some();
            presence.push(Some((prop.is_some() || has_index, !required)));
        }
        // No member can have it: `"p" in x` gives `x & Record<"p", unknown>`.
        if !presence.iter().any(|p| p.is_some_and(|(yes, _)| yes)) {
            if assume_true && presence.iter().any(Option::is_some) {
                let rec = self.in_record(name);
                return self.intersection(&[t, rec]);
            }
            return t;
        }
        let mut keep = Vec::new();
        for (m, p) in members.into_iter().zip(presence) {
            let k = match p {
                None => true,
                Some((yes, no)) => {
                    if assume_true {
                        yes
                    } else {
                        no
                    }
                }
            };
            if k {
                keep.push(m);
            }
        }
        self.union(&keep)
    }

    /// Whether the reference being narrowed is `obj.name` (or `obj["name"]`).
    fn is_property_of_ref(&mut self, w: &Walk, obj: Loc, name: &str) -> bool {
        if w.key.path.last().is_none_or(|p| &**p != name) {
            return false;
        }
        let ast = self.ast(obj.file);
        let n = skip_parens(ast, obj.node);
        if !matches!(
            ast.node(n),
            Node::Ident { .. } | Node::This {} | Node::Member { .. } | Node::Index { .. }
        ) {
            return false;
        }
        let Some(k) = self.ref_key(Loc::new(obj.file, n)) else {
            return false;
        };
        k.root == w.key.root
            && k.path.len() + 1 == w.key.path.len()
            && k.path.iter().zip(&w.key.path).all(|(a, b)| a == b)
    }

    /// An optional property's type where its presence is known (`'p' in o`): without the
    /// `missing` type when present, only `missing` when absent (`exactOptionalPropertyTypes`).
    fn narrow_by_presence(&mut self, t: TypeId, assume_true: bool) -> TypeId {
        // A declared `undefined` (`p?: T | undefined`) stays either way.
        if !self.some_member(t, |m| m == TypeId::MISSING)
            || self.some_member(t, |m| m == TypeId::UNDEFINED)
        {
            return t;
        }
        if assume_true {
            self.remove_missing(t)
        } else {
            TypeId::MISSING
        }
    }

    fn narrow_by_call(
        &mut self,
        w: &Walk,
        t: TypeId,
        call: Loc,
        callee: NodeId,
        args: ts_r_ast::NodeList,
        assume_true: bool,
    ) -> TypeId {
        let file = call.file;
        let ast = self.ast(file);
        // `ref?.method()` truthy → ref is not nullish (and a `this is T` predicate of
        // `method` still applies: `p?.isBlock()`).
        let t = if assume_true && self.optional_chain_contains_ref(w, call) {
            self.non_nullable(t)
        } else {
            t
        };
        // `o.hasOwnProperty('p')` narrows `o.p` like `'p' in o`.
        if let Node::Member {
            obj,
            prop,
            optional: false,
        } = *ast.node(skip_parens(ast, callee))
            && let [arg] = ast.list(args)
            && matches!(ast.node(prop), Node::Ident { .. })
            && self.c.text(Loc::new(file, prop)) == "hasOwnProperty"
            && matches!(ast.node(*arg), Node::StringLit { .. })
            && let Some(nm) = key_text(ast, *arg)
            && self.is_property_of_ref(w, Loc::new(file, obj), &nm)
        {
            return self.narrow_by_presence(t, assume_true);
        }
        // Type predicates: the reference must appear as an argument or the callee's object.
        let arg_list = ast.list(args).to_vec();
        let mentioned = arg_list
            .iter()
            .any(|&a| self.condition_mentions(Loc::new(file, a), &w.root_name))
            || self.condition_mentions(Loc::new(file, callee), &w.root_name);
        if !mentioned {
            return t;
        }
        let Some(sig) = self.resolved_signature_quiet(call) else {
            return t;
        };
        let Some(pred) = self.predicate_of(sig) else {
            return t;
        };
        match pred.kind {
            PredicateKind::Identifier => {
                let Some(&arg) = arg_list.get(pred.param_index as usize) else {
                    return t;
                };
                if !self.is_ref(w, Loc::new(file, arg)) {
                    return match pred.ty {
                        Some(pt) => self.narrow_by_predicate_on_arg(
                            w,
                            t,
                            Loc::new(file, arg),
                            pt,
                            assume_true,
                        ),
                        None => t,
                    };
                }
                match pred.ty {
                    Some(pt) => self.narrow_by_predicate_type(t, pt, assume_true),
                    None => t,
                }
            }
            PredicateKind::This => {
                let c = skip_parens(ast, callee);
                let obj = match *ast.node(c) {
                    Node::Member { obj, .. } | Node::Index { obj, .. } => obj,
                    _ => return t,
                };
                if !self.is_ref(w, Loc::new(file, obj)) {
                    return t;
                }
                match pred.ty {
                    Some(pt) => self.narrow_by_predicate_type(t, pt, assume_true),
                    None => t,
                }
            }
            _ => t,
        }
    }

    /// A type predicate `pt` on an argument that is not the reference itself.
    fn narrow_by_predicate_on_arg(
        &mut self,
        w: &Walk,
        t: TypeId,
        arg: Loc,
        pt: TypeId,
        assume_true: bool,
    ) -> TypeId {
        // `assertEqual(x.kind, "a" as const)`: a discriminant of the reference.
        let r = if let Some(prop) = self.discriminant_of(w, arg)
            && self.is_discriminant_here(w, t, &prop)
        {
            self.narrow_by_discriminant(t, &prop, |ck, m| {
                ck.narrow_by_predicate_type(m, pt, assume_true)
            })
        } else {
            t
        };
        // `isString(x?.y)` true (or `isNil(x?.y)` false): the branch rules out `undefined`
        // for the argument, so `x` is not nullish.
        if self.optional_chain_contains_ref(w, arg)
            && self.narrow_by_predicate_type(TypeId::UNDEFINED, pt, assume_true) == TypeId::NEVER
        {
            return self.non_nullable(r);
        }
        r
    }

    fn narrow_by_predicate_type(&mut self, t: TypeId, pt: TypeId, assume_true: bool) -> TypeId {
        if self.is_any_kept_by_predicate(t, pt) {
            return t;
        }
        if assume_true {
            self.narrow_to_type(t, pt)
        } else {
            if self.store().flags(t).intersects(TypeFlags::ANY_OR_UNKNOWN) {
                return t;
            }
            self.filter_type(t, |ck, m| !ck.is_assignable(m, pt))
        }
    }

    /// `any` is not narrowed by a predicate (or assertion) whose type is exactly the
    /// global `Object` or `Function` (tsc: checker/flow.go, type predicate narrowing).
    fn is_any_kept_by_predicate(&mut self, t: TypeId, pt: TypeId) -> bool {
        self.store().flags(t).contains(TypeFlags::ANY)
            && [GlobalType::Object, GlobalType::Function]
                .into_iter()
                .any(|g| self.global_type(g) == Some(pt))
    }

    /// The signature resolved for a call, computed quietly.
    fn resolved_signature_quiet(&mut self, call: Loc) -> Option<ts_r_types::SigId> {
        if let Some(&s) = self.resolved_calls.get(&call) {
            return s;
        }
        if self.is_resolving(crate::checker::Resolving::Node(call)) {
            return None;
        }
        self.quietly(|ck| ck.check_expr(call));
        self.resolved_calls.get(&call).copied().flatten()
    }

    fn narrow_switch(&mut self, w: &Walk, t: TypeId, sw: Loc, start: usize, end: usize) -> TypeId {
        let file = sw.file;
        let ast = self.ast(file);
        let Node::Switch { disc, cases } = *ast.node(sw.node) else {
            return t;
        };
        let clauses = ast.list(cases).to_vec();
        let dl = Loc::new(file, disc);
        let dn = skip_parens(ast, disc);
        let t = self.narrow_switch_optional_chain(w, t, dl, &clauses, start, end);
        // What kind of discriminant?
        enum Kind {
            Ref,
            TypeOf,
            Prop(String),
        }
        let kind = if self.is_ref(w, dl) {
            Kind::Ref
        } else if let Node::Unary {
            op: UnaryOp::TypeOf,
            arg,
        } = *ast.node(dn)
        {
            if self.is_ref(w, Loc::new(file, arg)) {
                Kind::TypeOf
            } else {
                return t;
            }
        } else if let Some(p) = self.discriminant_of(w, dl) {
            Kind::Prop(p)
        } else {
            return t;
        };
        if matches!(kind, Kind::TypeOf) {
            return self.narrow_typeof_switch(t, file, &clauses, start, end);
        }
        // The clause tests in range, and whether the range includes `default`.
        let mut tests = Vec::new();
        let mut has_default = start == end;
        for &c in clauses.get(start..end.min(clauses.len())).unwrap_or(&[]) {
            if let Node::SwitchCase { test, .. } = *ast.node(c) {
                match test {
                    Some(x) => tests.push(x),
                    None => has_default = true,
                }
            }
        }
        if has_default {
            // Remove every other clause value.
            let mut others = Vec::new();
            for (i, &c) in clauses.iter().enumerate() {
                if (start..end).contains(&i) {
                    continue;
                }
                if let Node::SwitchCase { test: Some(x), .. } = *ast.node(c) {
                    others.push(x);
                }
            }
            let mut cur = t;
            for x in others {
                cur = self.narrow_switch_one(&kind_ref(&kind), cur, Loc::new(file, x), false);
            }
            // A `default` grouped with cases also matches those cases.
            if start != end && !tests.is_empty() {
                let mut parts = vec![cur];
                for &x in &tests {
                    parts.push(self.narrow_switch_one(
                        &kind_ref(&kind),
                        t,
                        Loc::new(file, x),
                        true,
                    ));
                }
                return self.union(&parts);
            }
            return cur;
        }
        let mut parts = Vec::new();
        for x in tests {
            parts.push(self.narrow_switch_one(&kind_ref(&kind), t, Loc::new(file, x), true));
        }
        return self.union(&parts);

        fn kind_ref(k: &Kind) -> SwitchKind {
            match k {
                Kind::Ref => SwitchKind::Ref,
                Kind::TypeOf => SwitchKind::TypeOf,
                Kind::Prop(p) => SwitchKind::Prop(p.clone()),
            }
        }
    }

    /// `switch (x?.p)` or `switch (typeof x?.p)`: in clauses `start..end` that are all
    /// `case`s whose value can't be `undefined` (`"undefined"` for `typeof`), the chain
    /// was evaluated to the end, so `x` is not nullish (tsc: checker/flow.go, the switch
    /// clause narrowing by optional chain containment).
    fn narrow_switch_optional_chain(
        &mut self,
        w: &Walk,
        t: TypeId,
        disc: Loc,
        clauses: &[NodeId],
        start: usize,
        end: usize,
    ) -> TypeId {
        if !self.c.opts.strict_null_checks || start >= end || end > clauses.len() {
            return t;
        }
        let ast = self.ast(disc.file);
        let typeof_arg = match *ast.node(skip_parens(ast, disc.node)) {
            Node::Unary {
                op: UnaryOp::TypeOf,
                arg,
            } => Some(arg),
            _ => None,
        };
        let is_typeof = if self.optional_chain_contains_ref_eq(w, disc) {
            false
        } else if let Some(arg) = typeof_arg
            && self.optional_chain_contains_ref_eq(w, Loc::new(disc.file, arg))
        {
            true
        } else {
            return t;
        };
        for &c in &clauses[start..end] {
            let Node::SwitchCase { test: Some(x), .. } = *ast.node(c) else {
                return t;
            };
            let xl = Loc::new(disc.file, x);
            let xt = self.quietly(|ck| ck.check_expr(xl));
            let flags = self.store().flags(xt);
            if flags.contains(TypeFlags::NEVER)
                || (!is_typeof && flags.contains(TypeFlags::UNDEFINED))
                || (is_typeof && self.typeof_case_text(xl).as_deref() == Some("undefined"))
            {
                return t;
            }
        }
        self.non_nullable(t)
    }

    /// The text of a `typeof` switch case: a string literal or a template literal without
    /// substitutions (`` case `number`: ``).
    fn typeof_case_text(&self, test: Loc) -> Option<String> {
        let ast = self.ast(test.file);
        match *ast.node(skip_parens(ast, test.node)) {
            Node::StringLit { value } => Some(ast.atom(value).to_owned()),
            Node::TemplateLit { parts } if ast.list(parts).len() == 1 => {
                match *ast.node(ast.list(parts)[0]) {
                    Node::TemplateElement { cooked, .. } => Some(ast.atom(cooked).to_owned()),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// The `typeof` facts the case tests of `clauses` (those `pick` selects) name together
    /// (tests that are not a known `typeof` result add nothing).
    fn typeof_case_facts(
        &self,
        clauses: &[NodeId],
        file: ts_r_diagnostics::FileId,
        pick: impl Fn(usize) -> bool,
    ) -> u16 {
        let ast = self.ast(file);
        let mut set = 0;
        for (i, &c) in clauses.iter().enumerate() {
            if !pick(i) {
                continue;
            }
            if let Node::SwitchCase { test: Some(x), .. } = *ast.node(c)
                && let Some(text) = self.typeof_case_text(Loc::new(file, x))
                && let Some(f) = typeof_fact(&text)
            {
                set |= f;
            }
        }
        set
    }

    /// Removes the members of `t` whose every `typeof` result is in `set` (an `object`
    /// member goes by `"object"` alone).
    fn remove_typeof_facts(&mut self, t: TypeId, set: u16) -> TypeId {
        if set == 0 || self.store().flags(t).intersects(TypeFlags::ANY_OR_UNKNOWN) {
            return t;
        }
        self.map_type(t, |ck, m| {
            let f = ck.removable_typeof_facts(m);
            if f != 0 && f & !set == 0 {
                TypeId::NEVER
            } else {
                m
            }
        })
    }

    /// `switch (typeof x)` clauses `start..end` (observed, tsc 7): a value reaching a case
    /// did not match an earlier case; `default` (and the implicit default after the
    /// switch, `start == end`) removes what every other case matches, together.
    fn narrow_typeof_switch(
        &mut self,
        t: TypeId,
        file: ts_r_diagnostics::FileId,
        clauses: &[NodeId],
        start: usize,
        end: usize,
    ) -> TypeId {
        let ast = self.ast(file);
        let in_range = |i: usize| (start..end).contains(&i);
        let mut has_default = start == end;
        let mut tests = Vec::new();
        for &c in clauses.get(start..end.min(clauses.len())).unwrap_or(&[]) {
            if let Node::SwitchCase { test, .. } = *ast.node(c) {
                match test {
                    Some(x) => tests.push(x),
                    None => has_default = true,
                }
            }
        }
        let mut parts = Vec::new();
        if has_default {
            let others = self.typeof_case_facts(clauses, file, |i| !in_range(i));
            parts.push(self.remove_typeof_facts(t, others));
        }
        if !tests.is_empty() {
            // (Narrowed to the case first: `{}` by a second `case "number"` is `never`.)
            let earlier = self.typeof_case_facts(clauses, file, |i| i < start);
            for x in tests {
                let r = match self.typeof_case_text(Loc::new(file, x)) {
                    Some(lit) => self.narrow_by_typeof(t, &lit, true),
                    None => t,
                };
                parts.push(self.remove_typeof_facts(r, earlier));
            }
        }
        self.union(&parts)
    }

    /// `switch (true)` clauses `start..end`: every earlier clause test is false and one of
    /// the clause tests is true; a range with `default` (or the implicit default after the
    /// switch, `start == end`) has every other test false.
    fn narrow_switch_true(
        &mut self,
        w: &Walk,
        t: TypeId,
        sw: Loc,
        start: usize,
        end: usize,
    ) -> TypeId {
        let file = sw.file;
        let ast = self.ast(file);
        let Node::Switch { cases, .. } = *ast.node(sw.node) else {
            return t;
        };
        let clauses = ast.list(cases).to_vec();
        let mut has_default = start == end;
        let mut tests = Vec::new();
        for &c in clauses.get(start..end.min(clauses.len())).unwrap_or(&[]) {
            if let Node::SwitchCase { test, .. } = *ast.node(c) {
                match test {
                    Some(x) => tests.push(x),
                    None => has_default = true,
                }
            }
        }
        if has_default {
            let mut cur = t;
            for (i, &c) in clauses.iter().enumerate() {
                if (start..end).contains(&i) {
                    continue;
                }
                if let Node::SwitchCase { test: Some(x), .. } = *ast.node(c) {
                    cur = self.narrow(w, cur, Loc::new(file, x), false, 0);
                }
            }
            if tests.is_empty() {
                return cur;
            }
            let mut parts = vec![cur];
            let mut before = t;
            for &c in clauses.get(..start).unwrap_or(&[]) {
                if let Node::SwitchCase { test: Some(x), .. } = *ast.node(c) {
                    before = self.narrow(w, before, Loc::new(file, x), false, 0);
                }
            }
            for &x in &tests {
                parts.push(self.narrow(w, before, Loc::new(file, x), true, 0));
            }
            let r = self.union(&parts);
            return self.same_union_or(t, r);
        }
        let mut before = t;
        for &c in clauses.get(..start).unwrap_or(&[]) {
            if let Node::SwitchCase { test: Some(x), .. } = *ast.node(c) {
                before = self.narrow(w, before, Loc::new(file, x), false, 0);
            }
        }
        let mut parts = Vec::with_capacity(tests.len());
        for x in tests {
            parts.push(self.narrow(w, before, Loc::new(file, x), true, 0));
        }
        let r = self.union(&parts);
        self.same_union_or(t, r)
    }

    /// `t` itself when the union `r` has exactly its members (keeps `t`'s alias).
    fn same_union_or(&self, t: TypeId, r: TypeId) -> TypeId {
        let s = self.store();
        if r != t
            && s.flags(t).contains(TypeFlags::UNION)
            && s.flags(r).contains(TypeFlags::UNION)
            && s.members_of_union(t) == s.members_of_union(r)
        {
            return t;
        }
        r
    }

    fn narrow_switch_one(
        &mut self,
        kind: &SwitchKind,
        t: TypeId,
        test: Loc,
        assume_eq: bool,
    ) -> TypeId {
        match kind {
            SwitchKind::Ref => {
                let vt = self.quietly(|ck| ck.check_expr(test));
                self.narrow_type_by_equality(t, vt, true, assume_eq)
            }
            SwitchKind::TypeOf => match self.typeof_case_text(test) {
                Some(lit) => self.narrow_by_typeof(t, &lit, assume_eq),
                None => t,
            },
            SwitchKind::Prop(p) => {
                let vt = self.quietly(|ck| ck.check_expr(test));
                self.narrow_by_discriminant(t, p, |ck, pt| {
                    ck.narrow_type_by_equality(pt, vt, true, assume_eq)
                })
            }
        }
    }

    // ---------------------------------------------------------------------------------
    // Calls
    // ---------------------------------------------------------------------------------

    fn call_effect(&mut self, w: &Walk, call: Loc) -> CallEffect {
        let kind = self.call_kind(call);
        match kind {
            CallKindCache::Nothing => CallEffect::None,
            CallKindCache::Never => CallEffect::Never,
            CallKindCache::Assert(sig) => {
                let Some(pred) = self.predicate_of(sig) else {
                    return CallEffect::None;
                };
                let ast = self.ast(call.file);
                let Node::Call { callee, args, .. } = *ast.node(call.node) else {
                    return CallEffect::None;
                };
                let target = match pred.kind {
                    PredicateKind::AssertsIdentifier => {
                        ast.list(args).get(pred.param_index as usize).copied()
                    }
                    PredicateKind::AssertsThis => match *ast.node(skip_parens(ast, callee)) {
                        Node::Member { obj, .. } | Node::Index { obj, .. } => Some(obj),
                        _ => None,
                    },
                    _ => None,
                };
                let Some(target) = target else {
                    return CallEffect::None;
                };
                let tl = Loc::new(call.file, target);
                if !self.condition_mentions(tl, &w.root_name) {
                    return CallEffect::None;
                }
                if self.is_ref(w, tl) {
                    return CallEffect::Narrow(Assertion::Ref(pred.ty));
                }
                match pred.ty {
                    None => CallEffect::Narrow(Assertion::Condition(tl)),
                    Some(pt) => CallEffect::Narrow(Assertion::Arg(tl, pt)),
                }
            }
        }
    }

    /// Whether a call statement can affect flow (assertion or `never`-returning call).
    fn call_kind(&mut self, call: Loc) -> CallKindCache {
        if let Some(k) = self.call_kinds.get(&call) {
            return *k;
        }
        self.call_kinds.insert(call, CallKindCache::Nothing);
        let k = self.compute_call_kind(call);
        self.call_kinds.insert(call, k);
        k
    }

    fn call_target_has_explicit_type(&self, s: SymId) -> bool {
        let s = self.c.resolve_alias(s).unwrap_or(s);
        // A mapped type's property (`Readonly<T>`'s) has one when the property it maps does.
        if let Some(x) = self.c.synth_sym(s)
            && x.check & crate::symbols::check_flags::MAPPED != 0
            && let Some(&o) = x.origins.first()
            && o != s
            && self.call_target_has_explicit_type(o)
        {
            return true;
        }
        let flags = self.c.sym_flags(s);
        if !flags.intersects(SymbolFlags::VARIABLE | SymbolFlags::PROPERTY)
            || flags.intersects(SymbolFlags::FUNCTION | SymbolFlags::METHOD | SymbolFlags::CLASS)
        {
            return true;
        }
        self.c.value_decl(s).is_some_and(|d| {
            // The declaration may be the name; its declarator holds the type.
            let d = match *self.node(d) {
                Node::Ident { .. } => self.c.parent(d).unwrap_or(d),
                _ => d,
            };
            matches!(
                *self.node(d),
                Node::VarDeclarator {
                    type_ann: Some(_),
                    ..
                } | Node::Param {
                    type_ann: Some(_),
                    ..
                } | Node::PropertyDef {
                    type_ann: Some(_),
                    ..
                } | Node::PropertySignature {
                    type_ann: Some(_),
                    ..
                }
            )
        })
    }

    fn compute_call_kind(&mut self, call: Loc) -> CallKindCache {
        let ast = self.ast(call.file);
        let Node::Call { callee, .. } = *ast.node(call.node) else {
            return CallKindCache::Nothing;
        };
        let c = skip_parens(ast, callee);
        // Only a dotted name (`f()`, `a.b.f()`, `this.f()`) can end the flow (tsc 7; not
        // `f[0]()`).
        let mut root = c;
        while let Node::Member { obj, .. } = *ast.node(root) {
            root = skip_parens(ast, obj);
        }
        if !matches!(
            ast.node(root),
            Node::Ident { .. } | Node::This {} | Node::Super {}
        ) {
            return CallKindCache::Nothing;
        }
        // Every variable, parameter or property in the dotted target needs an explicit
        // type (`const f = (): never =>` has none). Check the root before resolving members.
        if matches!(ast.node(root), Node::Ident { .. }) {
            let Some(s) = self
                .c
                .resolve_ident(Loc::new(call.file, root), meaning::VALUE)
            else {
                return CallKindCache::Nothing;
            };
            if !self.call_target_has_explicit_type(s) {
                return CallKindCache::Nothing;
            }
        }
        let mut target = c;
        while let Node::Member { obj, .. } = *ast.node(target) {
            let Some(s) = self.property_symbol_of_access(Loc::new(call.file, target)) else {
                return CallKindCache::Nothing;
            };
            if !self.call_target_has_explicit_type(s) {
                return CallKindCache::Nothing;
            }
            target = skip_parens(ast, obj);
        }
        let ct = self.quietly(|ck| ck.check_expr(Loc::new(call.file, c)));
        if self.store().flags(ct).intersects(TypeFlags::ANY_OR_UNKNOWN) || self.store().is_error(ct)
        {
            return CallKindCache::Nothing;
        }
        let sigs = self.signatures_of(ct, SigKind::Call);
        let mut maybe = false;
        for &s in &sigs {
            if let Some(p) = self.predicate_of(s)
                && matches!(
                    p.kind,
                    PredicateKind::AssertsIdentifier | PredicateKind::AssertsThis
                )
            {
                maybe = true;
            }
            if self.declared_return_of(s) == Some(TypeId::NEVER) {
                maybe = true;
            }
        }
        if !maybe {
            return CallKindCache::Nothing;
        }
        // A single non-generic signature is the one the call resolves to; resolving the
        // call would check its arguments, whose flow types may be mid-walk here.
        let single = match sigs.as_slice() {
            [s] if self.store().sig(*s).type_params == ts_r_types::ListId::EMPTY => Some(*s),
            _ => None,
        };
        let Some(sig) = single.or_else(|| self.resolved_signature_quiet(call)) else {
            return CallKindCache::Nothing;
        };
        if let Some(p) = self.predicate_of(sig)
            && matches!(
                p.kind,
                PredicateKind::AssertsIdentifier | PredicateKind::AssertsThis
            )
        {
            // `asserts c` given a constant false condition (`false`, `false && x`) ends
            // the flow (observed: not `!true`, `0`, `null`, a `false` constant, or
            // `asserts c is T`).
            if p.kind == PredicateKind::AssertsIdentifier
                && p.ty.is_none()
                && let Node::Call { args, .. } = *ast.node(call.node)
                && let Some(&a) = ast.list(args).get(p.param_index as usize)
                && constant_condition(ast, a, 0) == Some(false)
            {
                return CallKindCache::Never;
            }
            return CallKindCache::Assert(sig);
        }
        if self.declared_return_of(sig) == Some(TypeId::NEVER) {
            return CallKindCache::Never;
        }
        CallKindCache::Nothing
    }

    /// Whether a flow node is reachable (for unreachable-code and implicit-return checks).
    pub fn is_flow_reachable(&mut self, file: ts_r_diagnostics::FileId, start: FlowId) -> bool {
        let reachable = self.flow_reachable_with(file, start, true, false);
        if self.no_report == 0 && self.speculative == 0 && self.flow_pending.is_empty() {
            self.reachable_flows.insert((file, start), reachable);
        }
        reachable
    }

    /// `cross_functions`: a function's start continues into the flow where the function
    /// is created, as the walk of a reference does for constants and parameters read in a
    /// closure (so a closure created after a `never`-returning call is unreachable too).
    fn flow_reachable_with(
        &mut self,
        file: ts_r_diagnostics::FileId,
        start: FlowId,
        exhaustive_switches: bool,
        cross_functions: bool,
    ) -> bool {
        // A `finally` block's end leaving normally sees only the normal antecedents of the
        // finally entry: each stack item carries the reductions in force (`envs[i]` = the
        // enclosing reduction, the finally label and its allowed antecedents; 0 = none).
        let mut envs: Vec<(u32, FlowId, Vec<FlowId>)> = vec![(0, FlowId::UNREACHABLE, Vec::new())];
        let mut seen = rustc_hash::FxHashSet::default();
        let mut stack = vec![(start, 0u32)];
        while let Some((f, env)) = stack.pop() {
            if f.is_unreachable() || !seen.insert((f, env)) {
                continue;
            }
            if exhaustive_switches
                && !cross_functions
                && env == 0
                && let Some(&reachable) = self.reachable_flows.get(&(file, f))
            {
                if reachable {
                    return true;
                }
                continue;
            }
            if seen.len() > 100_000 {
                return true;
            }
            let graph = &self.c.bound(file).flow;
            let Some(n) = graph.nodes.get(f.index()).copied() else {
                continue;
            };
            match n.kind {
                FlowKind::Unreachable => {}
                FlowKind::Start => {
                    if n.a != 0
                        && (cross_functions
                            || n.node.is_some_and(|fun| self.is_iife(Loc::new(file, fun))))
                    {
                        stack.push((FlowId(n.a), env));
                        continue;
                    }
                    return true;
                }
                FlowKind::BranchLabel | FlowKind::LoopLabel => {
                    let mut e = env;
                    while e != 0 && envs[e as usize].1 != f {
                        e = envs[e as usize].0;
                    }
                    if e != 0 {
                        stack.extend(envs[e as usize].2.iter().map(|&a| (a, env)));
                    } else {
                        stack.extend(graph.antecedents(f).iter().map(|&a| (a, env)));
                    }
                }
                FlowKind::Call => {
                    if let Some(c) = n.node
                        && matches!(self.call_kind(Loc::new(file, c)), CallKindCache::Never)
                    {
                        continue;
                    }
                    stack.push((FlowId(n.a), env));
                }
                FlowKind::TrueCondition | FlowKind::FalseCondition => {
                    // A condition on a value of type `never`... is still reachable.
                    stack.push((FlowId(n.a), env));
                }
                FlowKind::SwitchClause => {
                    // The implicit `default` of a switch that covers every value is dead.
                    let implicit_default = (n.b & 0xFFFF) == (n.b >> 16);
                    if implicit_default
                        && exhaustive_switches
                        && let Some(sw) = n.node
                        && self.is_exhaustive_switch(Loc::new(file, sw))
                    {
                        continue;
                    }
                    stack.push((FlowId(n.a), env));
                }
                FlowKind::ReduceLabel => {
                    if let Some((target, allowed)) = graph.reduce_target(f) {
                        envs.push((env, target, allowed.to_vec()));
                        stack.push((FlowId(n.a), (envs.len() - 1) as u32));
                    } else {
                        stack.push((FlowId(n.a), env));
                    }
                }
                _ => stack.push((FlowId(n.a), env)),
            }
        }
        false
    }

    /// Whether a flow node is the implicit `default` clause of a switch.
    fn is_implicit_default(&self, file: ts_r_diagnostics::FileId, f: FlowId) -> bool {
        let graph = &self.c.bound(file).flow;
        graph.nodes.get(f.index()).is_some_and(|n| {
            matches!(n.kind, FlowKind::SwitchClause)
                && n.node.is_some()
                && (n.b & 0xFFFF) == (n.b >> 16)
        })
    }

    /// A `switch` whose case values cover every member of its discriminant's (unit) type.
    pub(crate) fn is_exhaustive_switch(&mut self, sw: Loc) -> bool {
        // `None` while computing: a nested query settles the switch as not exhaustive, and
        // the outer result is then dropped.
        match self.exhaustive_switches.get(&sw) {
            Some(Some(r)) => return *r,
            Some(None) => {
                self.exhaustive_switches.insert(sw, Some(false));
                return false;
            }
            None => {}
        }
        self.exhaustive_switches.insert(sw, None);
        let r = self.compute_exhaustive_switch(sw);
        let e = self.exhaustive_switches.entry(sw).or_insert(None);
        *e.get_or_insert(r)
    }

    fn compute_exhaustive_switch(&mut self, sw: Loc) -> bool {
        let ast = self.ast(sw.file);
        let Node::Switch { disc, cases } = *ast.node(sw.node) else {
            return false;
        };
        let mut tests = Vec::new();
        for &c in ast.list(cases) {
            match *ast.node(c) {
                Node::SwitchCase { test: Some(t), .. } => tests.push(t),
                Node::SwitchCase { test: None, .. } => return true,
                _ => {}
            }
        }
        // `switch (typeof x)`: exhaustive when every member of `x`'s type has all its
        // `typeof` results among the cases.
        if let Node::Unary {
            op: UnaryOp::TypeOf,
            arg,
        } = *ast.node(skip_parens(ast, disc))
        {
            let clauses = ast.list(cases).to_vec();
            let set = self.typeof_case_facts(&clauses, sw.file, |_| true);
            // (All eight results cover `any` and `unknown` too.)
            if set == facts::ALL {
                return true;
            }
            let xt = self.quietly(|ck| ck.check_expr(Loc::new(sw.file, arg)));
            return self.remove_typeof_facts(xt, set) == TypeId::NEVER;
        }
        let dt = self.quietly(|ck| ck.check_expr(Loc::new(sw.file, disc)));
        let s = self.store();
        let members: Vec<TypeId> = if s.flags(dt).contains(TypeFlags::UNION) {
            s.members_of_union(dt).to_vec()
        } else {
            vec![dt]
        };
        if members.is_empty() || dt == TypeId::NEVER {
            return true;
        }
        if members
            .iter()
            .any(|&m| !self.store().flags(m).intersects(TypeFlags::UNIT))
        {
            return false;
        }
        let mut covered: Vec<TypeId> = Vec::new();
        for t in tests {
            let tt = self.quietly(|ck| ck.check_expr(Loc::new(sw.file, t)));
            // Every clause must be a unit type (`case a === b:` in `switch (true)` is a
            // `boolean` and makes the switch non-exhaustive).
            let f = self.store().flags(tt);
            if tt == TypeId::NEVER {
                continue;
            }
            if f.contains(TypeFlags::UNION) || !f.intersects(TypeFlags::UNIT) {
                return false;
            }
            covered.push(self.store().regular(tt));
        }
        members
            .iter()
            .all(|&m| covered.contains(&self.store().regular(m)))
    }
}

enum SwitchKind {
    Ref,
    TypeOf,
    Prop(String),
}

enum AssignEffect {
    None,
    Assigned(TypeId),
    Reset,
}

/// The value of a condition made of `true`, `false`, `&&` and `||` (through
/// parentheses), if it is one (not `!true`: observed).
fn constant_condition(ast: &ts_r_ast::Ast, e: NodeId, depth: u32) -> Option<bool> {
    if depth > 64 {
        return None;
    }
    match *ast.node(skip_parens(ast, e)) {
        Node::BoolLit { value } => Some(value),
        Node::Binary {
            left,
            op: BinaryOp::LogicalAnd,
            right,
        } => match constant_condition(ast, left, depth + 1) {
            Some(false) => Some(false),
            Some(true) => constant_condition(ast, right, depth + 1),
            None => (constant_condition(ast, right, depth + 1) == Some(false)).then_some(false),
        },
        Node::Binary {
            left,
            op: BinaryOp::LogicalOr,
            right,
        } => match constant_condition(ast, left, depth + 1) {
            Some(true) => Some(true),
            Some(false) => constant_condition(ast, right, depth + 1),
            None => (constant_condition(ast, right, depth + 1) == Some(true)).then_some(true),
        },
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CallKindCache {
    Nothing,
    Never,
    Assert(ts_r_types::SigId),
}

enum CallEffect {
    None,
    Never,
    Narrow(Assertion),
}

enum Assertion {
    /// `asserts x` / `asserts x is T` on the reference itself.
    Ref(Option<TypeId>),
    /// `asserts cond` where `cond` is an expression narrowing the reference.
    Condition(Loc),
    /// `asserts x is T` on an argument that is not the reference (`x.kind`, `x?.y`).
    Arg(Loc, TypeId),
}

impl Assertion {
    fn apply(self, ck: &mut Ck<'_, '_>, w: &Walk, t: TypeId) -> TypeId {
        match self {
            Assertion::Ref(Some(pt)) if ck.is_any_kept_by_predicate(t, pt) => t,
            Assertion::Ref(Some(pt)) => ck.narrow_to_type(t, pt),
            Assertion::Ref(None) => ck.narrow_truthiness(t, true),
            Assertion::Condition(l) => ck.narrow(w, t, l, true, 0),
            Assertion::Arg(l, pt) => ck.narrow_by_predicate_on_arg(w, t, l, pt, true),
        }
    }
}

#[path = "flow/evolving.rs"]
mod evolving;
pub(crate) use evolving::EvPending;

#[path = "flow/query_sound_flow.rs"]
mod query_sound_flow;

/// The switch statement of a switch-clause flow node.
fn graph_switch(ck: &Ck<'_, '_>, file: ts_r_diagnostics::FileId, f: FlowId) -> Option<Loc> {
    let n = ck.c.bound(file).flow.nodes.get(f.index())?;
    Some(Loc::new(file, n.node?))
}
