//! Call and `new` resolution: arity, overloads (a subtype pass then an assignability pass
//! when there are several candidates), type argument inference, and argument errors in
//! tsc's format (TS2345, TS2554/2555, TS2575, TS2769).

use ts_r_ast::{Node, NodeId, Span};
use ts_r_binder::meaning;
use ts_r_types::{ListId, RelationKind, SigFlags, SigId, TupleElement, TypeFlags, TypeId};

use crate::checker::{Ck, GlobalType, Loc, Resolving};
use crate::diag::{Chain, dg};
use crate::infer::InferenceContext;
use crate::members::SigKind;
use crate::relate::Head;

/// One effective argument.
#[derive(Clone, Copy, Debug)]
enum Arg {
    /// An argument expression.
    Expr(NodeId),
    /// An element of a spread tuple argument: the spread node and the element type.
    Elem(NodeId, TypeId),
    /// A spread of a non-tuple (array or iterable): the spread node and the element type.
    Rest(NodeId, TypeId),
}

impl Arg {
    fn node(self) -> NodeId {
        match self {
            Arg::Expr(n) | Arg::Elem(n, _) | Arg::Rest(n, _) => n,
        }
    }
}

/// The arguments of a call-like expression: a call's or `new`'s list, or a tagged
/// template's (its `TemplateStringsArray`, then each substitution).
#[derive(Clone, Copy, Debug)]
pub(crate) enum Args {
    List(ts_r_ast::NodeList),
    Tagged(NodeId),
}

/// Why a candidate failed.
enum Failure {
    /// Argument `i` (with its type) is not related to the parameter type.
    Arg {
        index: usize,
        arg_type: TypeId,
        param: TypeId,
        /// The arguments from `index` on, as one tuple against a rest parameter whose
        /// tuple type has a rest element (`...args: [...string[], number]`).
        tuple: bool,
    },
    /// A spread argument of a non-tuple maps to a non-rest parameter.
    Spread(NodeId),
    /// The call's `this` argument (the receiver, or `void` without one, reported at
    /// `at`) is not related to the signature's `this` parameter.
    This {
        arg_type: TypeId,
        this_type: TypeId,
        at: Loc,
    },
}

/// The arguments a rest parameter typed as a tuple with a rest element takes (see
/// [`Ck::rest_tuple_contexts`]).
struct RestTuple {
    /// The first argument in the tuple (after those the leading elements take).
    start: usize,
    /// The tuple from the rest element on.
    param: TypeId,
    /// Each argument's context, from the rest parameter's position.
    ctx: Vec<TypeId>,
}

/// How [`Ck::check_candidate_args_in`] treats context-sensitive function arguments (flags
/// by argument position).
#[derive(Clone, Copy)]
enum ArgPass<'s> {
    /// Like any other argument.
    Plain,
    /// Not checked: only their parameter type must be one a function could fit.
    Skip(&'s [bool]),
    /// Checked in order; each one reached gets its context fixed.
    Fix(&'s [bool]),
}

/// The outcome of resolving a call whose arguments are only types (decorators).
pub(crate) enum TypedCall {
    /// No signature takes that many arguments.
    Arity,
    /// The chosen signature, instantiated.
    Ok(SigId),
    /// Every candidate failed: the last one tried (instantiated) and its failing argument.
    Arg {
        sig: SigId,
        arg_type: TypeId,
        param: TypeId,
    },
}

impl<'c, 'a> Ck<'c, 'a> {
    /// `(min, max, has_rest)`: the argument counts `sig` accepts.
    pub(crate) fn sig_arity(&self, sig: SigId) -> (usize, usize, bool) {
        self.arity(sig, self.effective_min_args(sig))
    }

    /// Resolves a call at `e` from argument types alone, overloads in order as for calls.
    /// With `optional_last`, a signature that only takes the arguments without the last one
    /// is a candidate too (checked against those).
    pub(crate) fn resolve_typed_call(
        &mut self,
        e: Loc,
        sigs: &[SigId],
        args: &[TypeId],
        optional_last: bool,
    ) -> TypedCall {
        let full: Vec<Arg> = args.iter().map(|&t| Arg::Elem(e.node, t)).collect();
        let short = &full[..full.len().saturating_sub(1)];
        let mut candidates: Vec<(SigId, &[Arg])> = Vec::new();
        for &s in sigs {
            if self.has_correct_arity(s, &full) {
                candidates.push((s, &full));
            } else if optional_last && !full.is_empty() && self.has_correct_arity(s, short) {
                candidates.push((s, short));
            }
        }
        if candidates.is_empty() {
            return TypedCall::Arity;
        }
        let relations: &[RelationKind] = if candidates.len() > 1 {
            &[RelationKind::Subtype, RelationKind::Assignable]
        } else {
            &[RelationKind::Assignable]
        };
        let mut last = TypedCall::Arity;
        for &rel in relations {
            for &(cand, eff) in &candidates {
                let inst = self.instantiate_candidate(e, cand, eff, None);
                match self.check_candidate_args(e, inst, eff, rel) {
                    None => return TypedCall::Ok(inst),
                    Some(Failure::Arg {
                        arg_type, param, ..
                    }) => {
                        last = TypedCall::Arg {
                            sig: inst,
                            arg_type,
                            param,
                        }
                    }
                    Some(Failure::Spread(_) | Failure::This { .. }) => {}
                }
            }
        }
        last
    }

    /// `sig` instantiated by inference from argument types.
    pub(crate) fn instantiate_typed_call(&mut self, e: Loc, sig: SigId, args: &[TypeId]) -> SigId {
        let eff: Vec<Arg> = args.iter().map(|&t| Arg::Elem(e.node, t)).collect();
        self.instantiate_candidate(e, sig, &eff, None)
    }

    /// A call expression; `callee_type` is given for optional-chain links.
    pub fn check_call(&mut self, e: Loc, callee_type: Option<TypeId>) -> TypeId {
        if let Some(t) = self.commonjs_require_type(e) {
            return t;
        }
        let t = self.check_call_inner(e, callee_type);
        // `Symbol()` initializing a `const` (or a `static readonly` property) is that
        // declaration's `unique symbol`, annotated or not.
        if self.store().flags(t).contains(TypeFlags::ES_SYMBOL)
            && let Some(s) = self.unique_symbol_declaration_of_call(e)
        {
            return self.store().unique_symbol(s);
        }
        t
    }

    /// JavaScript `require("m")` (`require` undeclared, or an ambient function or variable):
    /// the module's type, its `module.exports` value or its exports object; `any` when the
    /// module isn't resolved (TS2307 reports that) or has no type here.
    pub(crate) fn commonjs_require_type(&mut self, e: Loc) -> Option<TypeId> {
        if !self.c.file(e.file).is_js() {
            return None;
        }
        let ast = self.ast(e.file);
        let Node::Call { callee, args, .. } = *ast.node(e.node) else {
            return None;
        };
        let [arg] = ast.list(args) else {
            return None;
        };
        let arg = *arg;
        if crate::nodes::ident_text(ast, callee) != Some("require")
            || !matches!(ast.node(arg), Node::StringLit { .. })
        {
            return None;
        }
        if !self.is_commonjs_global(Loc::new(e.file, callee)) {
            return None;
        }
        let Some(sym) = self.required_module(e.file, arg) else {
            return Some(TypeId::ANY);
        };
        // An ES module's `export { x as "module.exports" }` is what `require` returns.
        let sym = match self.c.export_of(sym, "module.exports") {
            Some(e) => self.c.resolve_alias(e).unwrap_or(e),
            None => sym,
        };
        let t = self.type_of_symbol(sym);
        if t == TypeId::UNRESOLVED || self.store().is_error(t) {
            return Some(TypeId::ANY);
        }
        Some(t)
    }

    /// The module symbol of `require(arg)`'s string `arg`, if it resolves to a module.
    pub(crate) fn required_module(
        &self,
        file: ts_r_diagnostics::FileId,
        arg: NodeId,
    ) -> Option<ts_r_types::SymId> {
        let req = self.c.request_of(file, arg)?;
        match self.c.link.module_target(file, req) {
            ts_r_binder::program::ModuleTarget::File(f) => {
                let sf = self.c.file(f);
                sf.bound
                    .is_module
                    .then(|| self.c.sym(f, sf.bound.file_symbol))
            }
            t @ ts_r_binder::program::ModuleTarget::Ambient(_) => self.c.module_target_symbol(t),
            ts_r_binder::program::ModuleTarget::Unresolved => None,
        }
    }

    fn unique_symbol_declaration_of_call(&mut self, e: Loc) -> Option<ts_r_types::SymId> {
        let ast = self.ast(e.file);
        let mut at = e;
        let mut p = self.c.parent(at)?;
        while matches!(ast.node(p.node), Node::Paren { .. }) {
            at = p;
            p = self.c.parent(at)?;
        }
        let decl = match *ast.node(p.node) {
            Node::VarDeclarator {
                name,
                init: Some(i),
                ..
            } if i == at.node
                && matches!(ast.node(name), Node::Ident { .. })
                && self.is_const_declaration(Loc::new(p.file, name)) =>
            {
                Loc::new(p.file, name)
            }
            Node::PropertyDef {
                modifiers,
                value: Some(v),
                ..
            } if v == at.node
                && modifiers.contains(ts_r_ast::Modifiers::STATIC)
                && modifiers.contains(ts_r_ast::Modifiers::READONLY) =>
            {
                p
            }
            _ => return None,
        };
        if !self.is_global_symbol_call(e) {
            return None;
        }
        self.c.sym_of_decl(decl)
    }

    fn check_call_inner(&mut self, e: Loc, callee_type: Option<TypeId>) -> TypeId {
        let ast = self.ast(e.file);
        let Node::Call {
            callee,
            type_args,
            args,
            ..
        } = *ast.node(e.node)
        else {
            return TypeId::ERROR;
        };
        let cl = Loc::new(e.file, callee);
        if matches!(ast.node(callee), Node::Super {}) {
            let st = self.check_expr(cl);
            if st == TypeId::UNRESOLVED || self.store().flags(st).intersects(TypeFlags::ANY) {
                self.check_args_plain(e, Args::List(args));
                return TypeId::VOID;
            }
            // `super<T>(...)`: TS2754, the call checked without them.
            let type_args = match type_args {
                Some(ta) => {
                    self.error(
                        Loc::new(e.file, ta),
                        dg::SUPER_MAY_NOT_USE_TYPE_ARGUMENTS_2754,
                        &[],
                    );
                    None
                }
                None => None,
            };
            // `super(...)`: the base class constructor's construct signatures.
            let ctor = self.super_constructor_type(e);
            match ctor {
                Some(ct) => {
                    let sigs = self.signatures_of(ct, SigKind::Construct);
                    let sigs = self.super_signatures_for_type_args(e, sigs);
                    if sigs.is_empty() {
                        self.check_args_plain(e, Args::List(args));
                    } else {
                        let _ = self.resolve_call(
                            e,
                            cl,
                            &sigs,
                            type_args,
                            Args::List(args),
                            SigKind::Construct,
                        );
                    }
                }
                None => self.check_args_plain(e, Args::List(args)),
            }
            return TypeId::VOID;
        }
        // A CommonJS `require(x)` in a JavaScript file (one argument, `require` not declared)
        // imports a module; its type (the module's exports) is not modelled yet.
        if callee_type.is_none()
            && matches!(*ast.node(callee), Node::Ident { name } if ast.atom(name) == "require")
            && ast.list(args).len() == 1
            && self.c.file(e.file).is_js()
            && self.c.resolve_ident(cl, meaning::VALUE).is_none()
        {
            self.check_args_plain(e, Args::List(args));
            return TypeId::UNRESOLVED;
        }
        let ft = match callee_type {
            Some(t) => t,
            None => self.check_expr(cl),
        };
        if ft == TypeId::SILENT_NEVER {
            return ft;
        }
        self.resolve_call_like(e, cl, ft, type_args, Args::List(args), SigKind::Call)
    }

    /// The base constructor's signatures as the `extends` clause's type arguments
    /// instantiate them (`class C extends CBase<string>`: `super(...)` takes
    /// `CBase<string>`'s parameters); unchanged without type arguments or when none fit.
    fn super_signatures_for_type_args(&mut self, e: Loc, sigs: Vec<SigId>) -> Vec<SigId> {
        let Some(class) = self.enclosing_class(e) else {
            return sigs;
        };
        let ast = self.ast(class.file);
        let (Node::ClassDecl { heritage, .. } | Node::ClassExpr { heritage, .. }) =
            *ast.node(class.node)
        else {
            return sigs;
        };
        let mut arg_nodes = Vec::new();
        for &h in ast.list(heritage) {
            if let Node::HeritageClause {
                kind: ts_r_ast::HeritageKind::Extends,
                types,
            } = *ast.node(h)
                && let Some(&first) = ast.list(types).first()
                && let Node::ExprWithTypeArgs {
                    type_args: Some(ta),
                    ..
                } = *ast.node(first)
                && let Node::TypeArgs { args } = *ast.node(ta)
            {
                arg_nodes = ast.list(args).to_vec();
            }
        }
        if arg_nodes.is_empty() {
            return sigs;
        }
        let args: Vec<TypeId> = arg_nodes
            .iter()
            .map(|&a| self.type_from_node(Loc::new(class.file, a)))
            .collect();
        let mut out = Vec::with_capacity(sigs.len());
        for &sig in &sigs {
            let tps = self
                .store()
                .list_items(self.store().sig(sig).type_params)
                .to_vec();
            if tps.is_empty() || !self.type_arg_count_fits(&tps, args.len()) {
                continue;
            }
            let filled = self.fill_type_args(&tps, &args);
            let m = self.store().mapper(&tps, &filled);
            out.push(self.instantiate_signature_full(sig, m));
        }
        if out.is_empty() { sigs } else { out }
    }

    fn super_constructor_type(&mut self, e: Loc) -> Option<TypeId> {
        let class = self.enclosing_class(e)?;
        let s = self.c.sym_of_decl(class)?;
        self.base_constructor_type(s)
    }

    /// `new C(...)`.
    pub fn check_new(&mut self, e: Loc) -> TypeId {
        let ast = self.ast(e.file);
        let Node::New {
            callee,
            type_args,
            args,
            ..
        } = *ast.node(e.node)
        else {
            return TypeId::ERROR;
        };
        let cl = Loc::new(e.file, callee);
        let ft = self.check_expr(cl);
        // An inaccessible constructor still makes an instance (observed: `c.constructor` is
        // checked); its arguments are not checked against it.
        if !self.constructor_accessible(e, ft) {
            self.resolved_calls.insert(e, None);
            self.check_args_plain(e, Args::List(args));
            let sigs = self.signatures_of(ft, SigKind::Construct);
            return match sigs.first() {
                Some(&sig) if self.store().sig(sig).type_params == ListId::EMPTY => {
                    self.return_type_of(sig)
                }
                _ => TypeId::ERROR,
            };
        }
        self.resolve_call_like(e, cl, ft, type_args, Args::List(args), SigKind::Construct)
    }

    /// The class of a `private` or `protected` constructor among `ft`'s construct
    /// signatures, with whether it is private.
    pub(crate) fn restricted_constructor(&mut self, ft: TypeId) -> Option<(Loc, bool)> {
        if ft == TypeId::UNRESOLVED || self.store().is_error(ft) {
            return None;
        }
        let sigs = self.signatures_of(ft, SigKind::Construct);
        let &sig = sigs.first()?;
        let d = self.store().sig(sig).decl.and_then(Loc::unpack)?;
        if !matches!(
            self.node(d),
            Node::MethodDef {
                kind: ts_r_ast::MethodKind::Constructor,
                ..
            }
        ) {
            return None;
        }
        let m = self.node(d).modifiers();
        let private = m.contains(ts_r_ast::Modifiers::PRIVATE);
        if !private && !m.contains(ts_r_ast::Modifiers::PROTECTED) {
            return None;
        }
        Some((self.enclosing_class(d)?, private))
    }

    /// `new C()` of a class with a `private` constructor outside its declaration, or a
    /// `protected` one outside it and its subclasses: TS2673/TS2674 at the expression.
    fn constructor_accessible(&mut self, e: Loc, ft: TypeId) -> bool {
        let Some((dc, private)) = self.restricted_constructor(ft) else {
            return true;
        };
        let mut cur = self.enclosing_class(e);
        let mut guard = 0;
        while let Some(c) = cur {
            guard += 1;
            if guard > 64 {
                break;
            }
            if c == dc || (!private && self.class_derives_from(c, dc)) {
                return true;
            }
            cur = self.enclosing_class(c);
        }
        if self.reporting(e.file) {
            let name = match self.c.sym_of_decl(dc) {
                Some(s) => {
                    let t = self.declared_type_of_symbol(s);
                    self.type_to_string(t)
                }
                None => String::new(),
            };
            let info = if private {
                dg::CONSTRUCTOR_OF_CLASS_0_IS_PRIVATE_AND_ONLY_ACCESSIBLE_WITHIN_THE_CLASS_DECLARATI_2673
            } else {
                dg::CONSTRUCTOR_OF_CLASS_0_IS_PROTECTED_AND_ONLY_ACCESSIBLE_WITHIN_THE_CLASS_DECLARA_2674
            };
            self.error(e, info, &[&name]);
        }
        false
    }

    /// Whether the callee's root identifier is an unannotated parameter of a function
    /// expression or arrow: its `any` may only be our missing contextual type.
    fn callee_root_is_contextual_param(&mut self, cl: Loc) -> bool {
        let ast = self.ast(cl.file);
        let mut n = cl.node;
        for _ in 0..64 {
            match *ast.node(n) {
                Node::Member { obj, .. } | Node::Index { obj, .. } => n = obj,
                Node::Paren { expr } | Node::NonNull { expr } => n = expr,
                Node::Ident { .. } => break,
                _ => return false,
            }
        }
        let Some(s) = self.c.resolve_ident(Loc::new(cl.file, n), meaning::VALUE) else {
            return false;
        };
        let Some(d) = self.c.value_decl(s) else {
            return false;
        };
        let dast = self.ast(d.file);
        let mut p = d;
        for _ in 0..8 {
            match *dast.node(p.node) {
                // `var a;`: an auto-typed variable whose flow type we may not track.
                Node::VarDeclarator {
                    type_ann: None,
                    init: None,
                    ..
                } => return true,
                Node::Param { type_ann, .. } => {
                    if type_ann.is_some() {
                        return false;
                    }
                    let Some(f) = self.c.parent(p) else {
                        return false;
                    };
                    if !matches!(
                        *dast.node(f.node),
                        Node::FunctionExpr { .. } | Node::Arrow { .. }
                    ) {
                        return false;
                    }
                    // An argument of a call on an `any` callee (also inside array and object
                    // literals) has no contextual type: its parameters are `any` for tsc too.
                    let mut a = f;
                    for _ in 0..32 {
                        let Some(q) = self.c.parent(a) else {
                            break;
                        };
                        if matches!(
                            *dast.node(q.node),
                            Node::Paren { .. }
                                | Node::ArrayLit { .. }
                                | Node::ObjectLit { .. }
                                | Node::Property { .. }
                        ) {
                            a = q;
                            continue;
                        }
                        if let Node::Call { callee, args, .. } = *dast.node(q.node)
                            && dast.list(args).contains(&a.node)
                        {
                            let ct = self.quietly(|ck| ck.check_expr(Loc::new(q.file, callee)));
                            if ct != TypeId::UNRESOLVED
                                && self.store().flags(ct).intersects(TypeFlags::ANY)
                            {
                                return false;
                            }
                        }
                        break;
                    }
                    return true;
                }
                Node::Ident { .. }
                | Node::ObjectPattern { .. }
                | Node::ArrayPattern { .. }
                | Node::BindingProperty { .. }
                | Node::AssignPattern { .. }
                | Node::Rest { .. } => {}
                _ => return false,
            }
            match self.c.parent(p) {
                Some(q) => p = q,
                None => return false,
            }
        }
        false
    }

    fn check_args_plain(&mut self, e: Loc, args: Args) {
        match args {
            Args::List(l) => {
                for &a in self.ast(e.file).list(l) {
                    self.check_expr(Loc::new(e.file, a));
                }
            }
            Args::Tagged(q) => {
                self.check_expr(Loc::new(e.file, q));
            }
        }
    }

    /// Whether a template literal lacks its closing backtick.
    fn template_unterminated(&self, f: ts_r_diagnostics::FileId, q: NodeId) -> bool {
        let t = self.c.text(Loc::new(f, q));
        if t.len() < 2 || !t.ends_with('`') {
            return true;
        }
        let escapes = t[..t.len() - 1]
            .bytes()
            .rev()
            .take_while(|&b| b == b'\\')
            .count();
        escapes % 2 == 1
    }

    pub(crate) fn resolve_call_like(
        &mut self,
        e: Loc,
        cl: Loc,
        ft: TypeId,
        type_args: Option<NodeId>,
        args: Args,
        kind: SigKind,
    ) -> TypeId {
        if ft == TypeId::UNRESOLVED
            || self.store().is_error(ft)
            || self.store().flags(ft).intersects(TypeFlags::ANY)
        {
            self.resolved_calls.insert(e, None);
            self.check_args_plain(e, args);
            if let Some(ta) = type_args {
                self.check_type_args_node(Loc::new(e.file, ta));
                // An `any` callee (not the error type of an unresolved name) takes no type
                // arguments.
                if ft != TypeId::UNRESOLVED
                    && !self.store().is_error(ft)
                    && ft != TypeId::AUTO
                    && !self.callee_root_is_contextual_param(cl)
                {
                    self.error(
                        e,
                        dg::UNTYPED_FUNCTION_CALLS_MAY_NOT_ACCEPT_TYPE_ARGUMENTS_2347,
                        &[],
                    );
                }
            }
            return if self.store().is_error(ft) {
                TypeId::ERROR
            } else if ft == TypeId::UNRESOLVED {
                TypeId::UNRESOLVED
            } else {
                TypeId::ANY
            };
        }
        let nt = self.check_non_null_callee(cl, ft);
        if self.store().is_error(nt) {
            self.check_args_plain(e, args);
            return TypeId::ERROR;
        }
        let mut sigs = self.signatures_of(nt, kind);
        if sigs.is_empty()
            && kind == SigKind::Call
            && let Some(alt) = self.union_of_arrays_method(cl)
        {
            sigs = self.signatures_of(alt, kind);
        }
        // `Function | (() => T)`: the arguments are checked against the members with call
        // signatures, and the call is untyped (`any`).
        if sigs.is_empty()
            && kind == SigKind::Call
            && let Some(rest) = self.union_without_function_members(nt)
        {
            let _ = self.resolve_call_like(e, cl, rest, type_args, args, kind);
            self.resolved_calls.insert(e, None);
            return TypeId::ANY;
        }
        // `new f()` of a value with call signatures only is resolved as a call (TS2350/TS2679,
        // TS7009 under noImplicitAny).
        if sigs.is_empty() && kind == SigKind::Construct {
            let call_sigs = self.signatures_of(nt, SigKind::Call);
            if !call_sigs.is_empty() {
                return self.new_of_callable(e, cl, &call_sigs, type_args, args);
            }
        }
        if sigs.is_empty() {
            self.resolved_calls.insert(e, None);
            self.check_args_plain(e, args);
            let r = self.report_not_callable(e, cl, nt, kind);
            // Untyped calls of `Function`-typed values take no type arguments either.
            if r == TypeId::ANY
                && kind == SigKind::Call
                && let Some(ta) = type_args
            {
                self.check_type_args_node(Loc::new(e.file, ta));
                self.error(
                    e,
                    dg::UNTYPED_FUNCTION_CALLS_MAY_NOT_ACCEPT_TYPE_ARGUMENTS_2347,
                    &[],
                );
            }
            return r;
        }
        if kind == SigKind::Construct
            && sigs
                .iter()
                .all(|&s| self.store().sig(s).flags.contains(SigFlags::ABSTRACT))
        {
            self.error(
                e,
                dg::CANNOT_CREATE_AN_INSTANCE_OF_AN_ABSTRACT_CLASS_2511,
                &[],
            );
        }
        let sigs = self.reorder_function_overloads(sigs);
        let (sig, failed) = self.resolve_call(e, cl, &sigs, type_args, args, kind);
        // The result is the intersection of the overloads' return types (`never` for
        // `number` and `string`; observed), so nothing more is reported about it. (Generic
        // overloads keep the failed candidate's instantiated return type.)
        if failed
            && sigs
                .iter()
                .all(|&s| self.store().sig(s).type_params == ListId::EMPTY)
        {
            let rets: Vec<TypeId> = sigs.iter().map(|&s| self.return_type_of(s)).collect();
            if rets.contains(&TypeId::UNRESOLVED) {
                return TypeId::ERROR;
            }
            return self.intersection(&rets);
        }
        match sig {
            Some(s) => {
                let r = self.return_type_of(s);
                if kind == SigKind::Construct && r == TypeId::VOID {
                    return TypeId::ANY;
                }
                r
            }
            None => TypeId::ERROR,
        }
    }

    /// `new f()` where `f` has call signatures but no construct signatures: the call is
    /// resolved against the call signatures (arity and argument errors as for `f()`).
    /// Without `noImplicitAny`, a declared signature returning something other than `void`
    /// is TS2350 and a `this: void` signature TS2679; with it, TS7009. The result is `any`
    /// when the chosen signature has a declaration, its return type otherwise.
    fn new_of_callable(
        &mut self,
        e: Loc,
        cl: Loc,
        sigs: &[SigId],
        type_args: Option<NodeId>,
        args: Args,
    ) -> TypeId {
        let sigs = self.reorder_function_overloads(sigs.to_vec());
        let (sig, _) = self.resolve_call(e, cl, &sigs, type_args, args, SigKind::Call);
        let Some(sig) = sig else {
            return TypeId::ANY;
        };
        let has_decl = self.store().sig(sig).decl.is_some();
        if !self.c.opts.no_implicit_any {
            if has_decl && self.return_type_of(sig) != TypeId::VOID {
                self.error(
                    e,
                    dg::ONLY_A_VOID_FUNCTION_CAN_BE_CALLED_WITH_THE_NEW_KEYWORD_2350,
                    &[],
                );
            }
            if self.store().sig(sig).this_type == Some(TypeId::VOID) {
                self.error(
                    e,
                    dg::A_FUNCTION_THAT_IS_CALLED_WITH_THE_NEW_KEYWORD_CANNOT_HAVE_A_THIS_TYPE_THAT_IS_V_2679,
                    &[],
                );
            }
        }
        if !has_decl {
            return self.return_type_of(sig);
        }
        if self.c.opts.no_implicit_any {
            self.error(
                e,
                dg::NEW_EXPRESSION_WHOSE_TARGET_LACKS_A_CONSTRUCT_SIGNATURE_IMPLICITLY_HAS_AN_ANY_TY_7009,
                &[],
            );
        }
        TypeId::ANY
    }

    /// A method of a union of array types whose signatures don't combine
    /// (`(string[] | number[]).filter(...)`): the method of an array of the union of the
    /// element types (`(string | number)[]`, readonly when some member is), as tsc does.
    fn union_of_arrays_method(&mut self, cl: Loc) -> Option<TypeId> {
        let ast = self.ast(cl.file);
        let n = crate::nodes::skip_parens(ast, cl.node);
        let Node::Member { obj, prop, .. } = *ast.node(n) else {
            return None;
        };
        let Node::Ident { name } = *ast.node(prop) else {
            return None;
        };
        let name = ast.atom(name).to_owned();
        let ot = self.check_expr(Loc::new(cl.file, obj));
        let ot = self.non_nullable(ot);
        if !self.store().flags(ot).contains(TypeFlags::UNION) {
            return None;
        }
        let members = self.store().members_of_union(ot).to_vec();
        let mut elems = Vec::with_capacity(members.len());
        let mut readonly = false;
        for m in members {
            if !self.c.is_array_like_type(m) {
                return None;
            }
            readonly |= self.is_readonly_array_like(m);
            elems.push(self.element_type_of_array_like(m)?);
        }
        let u = self.union(&elems);
        let arr = self.create_array(u, readonly);
        let p = self.property_of_type(arr, &name)?;
        Some(self.type_of_symbol(p))
    }

    fn check_non_null_callee(&mut self, cl: Loc, t: TypeId) -> TypeId {
        if !self.c.opts.strict_null_checks {
            return t;
        }
        // Calling an `unknown` value is TS18046 (TS2571 for a non-name callee).
        if t == TypeId::UNKNOWN {
            return self.check_non_null(cl, t);
        }
        let (u, n) = self.nullable_facts(t);
        if !u && !n {
            return t;
        }
        let info = match (u, n) {
            (true, true) => dg::CANNOT_INVOKE_AN_OBJECT_WHICH_IS_POSSIBLY_NULL_OR_UNDEFINED_2723,
            (true, false) => dg::CANNOT_INVOKE_AN_OBJECT_WHICH_IS_POSSIBLY_UNDEFINED_2722,
            _ => dg::CANNOT_INVOKE_AN_OBJECT_WHICH_IS_POSSIBLY_NULL_2721,
        };
        self.error(cl, info, &[]);
        let nn = self.non_nullable(t);
        if self
            .store()
            .flags(nn)
            .intersects(TypeFlags::NEVER | TypeFlags::NULLABLE)
        {
            return TypeId::ERROR;
        }
        nn
    }

    /// Overloads of a function declared in several places (a lib file and a `declare
    /// global` block) are tried group by group, later groups first, each group in order
    /// (`setTimeout` with `@types/node` and the DOM lib picks node's overloads first; the
    /// declaration order, which `Parameters<typeof setTimeout>` sees, is unchanged).
    fn reorder_function_overloads(&mut self, sigs: Vec<SigId>) -> Vec<SigId> {
        if sigs.len() < 2 {
            return sigs;
        }
        // Specialized signatures (a parameter annotated with a literal type, `on(event:
        // "connection", ...)`) are tried before the others, each set in order.
        let (lits, others): (Vec<SigId>, Vec<SigId>) = sigs
            .iter()
            .partition(|&&s| self.has_literal_param_annotation(s));
        if !lits.is_empty() && !others.is_empty() {
            let mut out = lits;
            out.extend(self.reorder_function_overloads(others));
            return out;
        }
        let mut parents = Vec::with_capacity(sigs.len());
        for &s in &sigs {
            let Some((f, n)) = self.store().sig(s).decl else {
                return sigs;
            };
            let file = ts_r_diagnostics::FileId(f);
            let Some(node) = ts_r_ast::NodeId::from_raw(n) else {
                return sigs;
            };
            if !matches!(self.ast(file).node(node), Node::FunctionDecl { .. }) {
                return sigs;
            }
            let parent = self.c.file(file).parents().parent(node);
            parents.push((f, parent));
        }
        if parents.windows(2).all(|w| w[0] == w[1]) {
            return sigs;
        }
        let mut out: Vec<SigId> = Vec::with_capacity(sigs.len());
        let mut index = 0;
        for (i, &s) in sigs.iter().enumerate() {
            if i > 0 && parents[i] == parents[i - 1] {
                index += 1;
            } else {
                index = 0;
            }
            out.insert(index, s);
        }
        out
    }

    /// A parameter of `sig`'s declaration is annotated with a literal type (`"a"`, `1`,
    /// `-1`, `true`, `null`), not a union of them.
    fn has_literal_param_annotation(&self, sig: SigId) -> bool {
        let Some((f, n)) = self.store().sig(sig).decl else {
            return false;
        };
        let file = ts_r_diagnostics::FileId(f);
        let Some(node) = ts_r_ast::NodeId::from_raw(n) else {
            return false;
        };
        let ast = self.ast(file);
        let params = match *ast.node(node) {
            Node::MethodSignature { params, .. }
            | Node::CallSignature { params, .. }
            | Node::ConstructSignature { params, .. }
            | Node::FunctionType { params, .. } => params,
            _ => match crate::nodes::fn_parts(ast, node) {
                Some(p) => p.params,
                None => return false,
            },
        };
        ast.list(params).iter().any(|&p| {
            let Node::Param {
                type_ann: Some(t), ..
            } = *ast.node(p)
            else {
                return false;
            };
            match *ast.node(t) {
                Node::StringLit { .. }
                | Node::NumberLit { .. }
                | Node::BigIntLit { .. }
                | Node::BoolLit { .. }
                | Node::NullLit {} => true,
                Node::Unary { arg, .. } => {
                    matches!(
                        ast.node(arg),
                        Node::NumberLit { .. } | Node::BigIntLit { .. }
                    )
                }
                _ => false,
            }
        })
    }

    /// Why `t` has no `kind` signatures: a union says whether none, not all or all of its
    /// members have them (the first member without them nested, as written); other types print as
    /// their apparent type (`Type 'Number' has no call signatures.`).
    pub(crate) fn no_signatures_chain(&mut self, chain: &mut Chain, t: TypeId, kind: SigKind) {
        let call = kind == SigKind::Call;
        let none_info = if call {
            dg::TYPE_0_HAS_NO_CALL_SIGNATURES_2757
        } else {
            dg::TYPE_0_HAS_NO_CONSTRUCT_SIGNATURES_2761
        };
        if self.store().flags(t).contains(TypeFlags::UNION) {
            let members = self.store().members_of_union(t).to_vec();
            let mut without = None;
            let mut with = 0;
            for &m in &members {
                let a = self.apparent_type(m);
                if self.signatures_of(a, kind).is_empty() {
                    without.get_or_insert(m);
                } else {
                    with += 1;
                }
            }
            let ts = self.type_to_string(t);
            match without {
                None => {
                    let info = if call {
                        dg::EACH_MEMBER_OF_THE_UNION_TYPE_0_HAS_SIGNATURES_BUT_NONE_OF_THOSE_SIGNATURES_ARE_C_2758
                    } else {
                        dg::EACH_MEMBER_OF_THE_UNION_TYPE_0_HAS_CONSTRUCT_SIGNATURES_BUT_NONE_OF_THOSE_SIGNA_2762
                    };
                    chain.push_back(info, &[&ts]);
                }
                Some(_) if with == 0 => {
                    let info = if call {
                        dg::NO_CONSTITUENT_OF_TYPE_0_IS_CALLABLE_2755
                    } else {
                        dg::NO_CONSTITUENT_OF_TYPE_0_IS_CONSTRUCTABLE_2759
                    };
                    chain.push_back(info, &[&ts]);
                }
                Some(w) => {
                    let info = if call {
                        dg::NOT_ALL_CONSTITUENTS_OF_TYPE_0_ARE_CALLABLE_2756
                    } else {
                        dg::NOT_ALL_CONSTITUENTS_OF_TYPE_0_ARE_CONSTRUCTABLE_2760
                    };
                    chain.push_back(info, &[&ts]);
                    let ws = self.type_to_string(w);
                    chain.push_back(none_info, &[&ws]);
                }
            }
            return;
        }
        let a = self.apparent_type(t);
        let ts = self.type_to_string(a);
        chain.push_back(none_info, &[&ts]);
    }

    /// For a union whose members without call signatures are all `Function`-like (no
    /// construct signatures either) next to members with call signatures: the union of
    /// the callable members.
    fn union_without_function_members(&mut self, t: TypeId) -> Option<TypeId> {
        if !self.store().flags(t).contains(TypeFlags::UNION) {
            return None;
        }
        let ft = self.global_type(GlobalType::Function)?;
        let members = self.store().members_of_union(t).to_vec();
        let mut callable = Vec::new();
        for m in members {
            if !self.signatures_of(m, SigKind::Call).is_empty() {
                callable.push(m);
            } else if !self.signatures_of(m, SigKind::Construct).is_empty()
                || !self.is_assignable(m, ft)
            {
                return None;
            }
        }
        if callable.is_empty() {
            return None;
        }
        let rest = self.union(&callable);
        // (The callable members must have call signatures together.)
        if rest == t || self.signatures_of(rest, SigKind::Call).is_empty() {
            return None;
        }
        Some(rest)
    }

    fn report_not_callable(&mut self, e: Loc, cl: Loc, t: TypeId, kind: SigKind) -> TypeId {
        match kind {
            SigKind::Call => {
                let ast = self.ast(e.file);
                let tagged = matches!(ast.node(e.node), Node::TaggedTemplate { .. });
                // Two template literals in a row in an array literal (a missing comma) form a
                // tagged template.
                if tagged
                    && matches!(ast.node(cl.node), Node::TemplateLit { .. })
                    && self
                        .c
                        .parent(e)
                        .is_some_and(|p| matches!(ast.node(p.node), Node::ArrayLit { .. }))
                {
                    self.error(cl, dg::IT_IS_LIKELY_THAT_YOU_ARE_MISSING_A_COMMA_TO_SEPARATE_THESE_TWO_TEMPLATE_EXPRESS_2796, &[]);
                    return TypeId::ERROR;
                }
                // Construct signatures come first: `typeof C` is assignable to `Function`
                // but calling it without `new` is still an error (a tag is just not callable).
                let constructable = !self.signatures_of(t, SigKind::Construct).is_empty();
                if !tagged && constructable {
                    let ts = self.type_to_string(t);
                    self.error(
                        cl,
                        dg::VALUE_OF_TYPE_0_IS_NOT_CALLABLE_DID_YOU_MEAN_TO_INCLUDE_NEW_2348,
                        &[&ts],
                    );
                    return TypeId::ERROR;
                }
                // Untyped calls of `Function`-typed values (`never` is not one).
                if !constructable
                    && let Some(ft) = self.global_type(GlobalType::Function)
                    && !self
                        .store()
                        .flags(t)
                        .intersects(TypeFlags::UNION | TypeFlags::NEVER)
                    && self.is_assignable(t, ft)
                {
                    return TypeId::ANY;
                }
                let mut chain = Chain::new();
                // `o.getter()`: TS6234 suggests dropping the call.
                let is_getter = !tagged
                    && matches!(self.ast(cl.file).node(cl.node), Node::Member { .. })
                    && self.property_symbol_of_access(cl).is_some_and(|p| {
                        self.c
                            .sym_flags(p)
                            .contains(ts_r_binder::SymbolFlags::GET_ACCESSOR)
                    });
                if is_getter {
                    chain.push_back(dg::THIS_EXPRESSION_IS_NOT_CALLABLE_BECAUSE_IT_IS_A_GET_ACCESSOR_DID_YOU_MEAN_TO_USE_6234, &[]);
                } else {
                    chain.push_back(dg::THIS_EXPRESSION_IS_NOT_CALLABLE_2349, &[]);
                }
                self.no_signatures_chain(&mut chain, t, kind);
                // A property access callee reports at the name (`o.m()`).
                let ast = self.ast(cl.file);
                let at = match *ast.node(cl.node) {
                    Node::Member { prop, .. } => Loc::new(cl.file, prop),
                    _ => cl,
                };
                self.error_chain(at, &chain);
                TypeId::ERROR
            }
            SigKind::Construct => {
                // (Callees with call signatures are resolved by `new_of_callable`.)
                let mut chain = Chain::new();
                chain.push_back(dg::THIS_EXPRESSION_IS_NOT_CONSTRUCTABLE_2351, &[]);
                self.no_signatures_chain(&mut chain, t, kind);
                self.error_chain(cl, &chain);
                TypeId::ERROR
            }
        }
    }

    fn check_type_args_node(&mut self, l: Loc) {
        let ast = self.ast(l.file);
        if let Node::TypeArgs { args } = *ast.node(l.node) {
            for &a in ast.list(args) {
                self.type_from_node(Loc::new(l.file, a));
            }
        }
    }

    /// The effective arguments (spreads of tuples expanded).
    fn effective_args(&mut self, e: Loc, args: Args) -> Vec<Arg> {
        let ast = self.ast(e.file);
        let mut out = Vec::new();
        let list = match args {
            Args::List(l) => ast.list(l),
            Args::Tagged(q) => {
                // The strings array, then each substitution expression.
                let tsa = self
                    .global_type(GlobalType::TemplateStringsArray)
                    .unwrap_or(TypeId::UNRESOLVED);
                out.push(Arg::Elem(q, tsa));
                return match *ast.node(q) {
                    Node::TemplateLit { parts } => {
                        out.extend(
                            ast.list(parts)
                                .iter()
                                .skip(1)
                                .step_by(2)
                                .map(|&x| Arg::Expr(x)),
                        );
                        out
                    }
                    _ => out,
                };
            }
        };
        for &a in list {
            if let Node::Spread { arg } = *ast.node(a) {
                let t = self.check_expr(Loc::new(e.file, arg));
                if let Some(shape) = self.store().tuple_shape_of(t).cloned() {
                    let targs = self.c.type_args_of_tuple(t).to_vec();
                    let mut ok = true;
                    let mut items = Vec::new();
                    for (i, el) in shape.elements.iter().enumerate() {
                        let ty = targs.get(i).copied().unwrap_or(TypeId::UNRESOLVED);
                        match el.kind {
                            ts_r_types::TupleElement::REQUIRED => items.push(Arg::Elem(a, ty)),
                            // An optional element is an argument that may be `undefined`.
                            ts_r_types::TupleElement::OPTIONAL => {
                                let ot = self.add_optionality(ty);
                                items.push(Arg::Elem(a, ot));
                            }
                            ts_r_types::TupleElement::REST => {
                                let et = self
                                    .element_type_of_array_like(ty)
                                    .unwrap_or(TypeId::UNRESOLVED);
                                items.push(Arg::Rest(a, et));
                            }
                            _ => {
                                ok = false;
                                break;
                            }
                        }
                    }
                    if ok {
                        out.extend(items);
                        continue;
                    }
                }
                // Unlike an array literal's, a call's spread of `never` is not iterable.
                if t == TypeId::NEVER {
                    self.check_iterable_value(Loc::new(e.file, arg), t);
                }
                let et = if self.store().flags(t).intersects(TypeFlags::ANY) {
                    t
                } else {
                    self.spread_element_type(Loc::new(e.file, arg), t)
                };
                out.push(Arg::Rest(a, et));
            } else {
                out.push(Arg::Expr(a));
            }
        }
        out
    }

    /// `(min, count, has_rest)` with a rest parameter of a tuple type counted as its
    /// elements: `(a: number, ...r: [string, boolean?])` takes 2 to 3 arguments. Required
    /// elements before the tuple's own rest make every earlier parameter required too.
    fn arity(&self, sig: SigId, min: usize) -> (usize, usize, bool) {
        let s = self.store();
        let d = s.sig(sig);
        if !d.has_rest() {
            return (min, d.params.len(), false);
        }
        let count = d.params.len() - 1;
        let Some(shape) = d.params.last().and_then(|p| s.tuple_shape_of(p.ty)) else {
            return (min, count, true);
        };
        let open = shape.has_rest();
        let required = shape
            .elements
            .iter()
            .take_while(|e| e.kind < ts_r_types::TupleElement::REST)
            .filter(|e| e.kind == ts_r_types::TupleElement::REQUIRED)
            .count();
        let len = if open {
            shape.fixed_length()
        } else {
            shape.elements.len()
        };
        let min = if required > 0 { count + required } else { min };
        (min, count + len, open)
    }

    /// Whether the rest parameter of `sig` has a generic type rather than an array type.
    fn has_generic_rest(&self, sig: SigId) -> bool {
        let s = self.store();
        let d = s.sig(sig);
        d.has_rest()
            && d.params
                .last()
                .is_some_and(|p| s.flags(p.ty).intersects(TypeFlags::INSTANTIABLE))
    }

    fn has_correct_arity(&self, sig: SigId, args: &[Arg]) -> bool {
        let (min, count, has_rest) = self.arity(sig, self.effective_min_args(sig));
        if let Some(pos) = args.iter().position(|a| matches!(a, Arg::Rest(..))) {
            // A spread of unknown length must start at or after the required parameters and
            // reach a rest parameter or stop before the last parameter (as tsc).
            return pos >= min && (has_rest || pos < count);
        }
        let n = args.len();
        n >= min && (has_rest || n <= count)
    }

    /// Resolves a call against `sigs`; reports errors; records the chosen signature for
    /// contextual typing of the arguments and returns it (the best candidate on failure).
    fn resolve_call(
        &mut self,
        e: Loc,
        cl: Loc,
        sigs: &[SigId],
        type_args: Option<NodeId>,
        args: Args,
        kind: SigKind,
    ) -> (Option<SigId>, bool) {
        if !self.push_resolution(Resolving::Node(e)) {
            return (sigs.first().copied(), false);
        }
        let saved = std::mem::take(&mut self.overload_failed);
        let r = self.resolve_call_inner(e, cl, sigs, type_args, args, kind);
        // A failed call of an overloaded function gets a combined result type; a single
        // signature's failure still returns its type.
        let failed = std::mem::replace(&mut self.overload_failed, saved) && sigs.len() > 1;
        self.pop_resolution();
        self.resolved_calls.insert(e, r);
        // Check the arguments for real (errors inside them, with the final contextual types).
        // Not while speculating: nothing would be reported or kept, and re-checking every
        // callback of a call nested in a speculatively checked callback is exponential.
        if self.speculative > 0 {
            return (r, failed);
        }
        self.check_args_plain(e, args);
        (r, failed)
    }

    fn explicit_type_args(&mut self, l: Option<Loc>) -> Option<Vec<TypeId>> {
        let l = l?;
        let ast = self.ast(l.file);
        let Node::TypeArgs { args } = *ast.node(l.node) else {
            return None;
        };
        let list = ast.list(args).to_vec();
        Some(
            list.into_iter()
                .map(|a| self.type_from_node(Loc::new(l.file, a)))
                .collect(),
        )
    }

    fn resolve_call_inner(
        &mut self,
        e: Loc,
        cl: Loc,
        sigs: &[SigId],
        type_args: Option<NodeId>,
        args: Args,
        kind: SigKind,
    ) -> Option<SigId> {
        let _ = kind;
        let eff = self.effective_args(e, args);
        let explicit = self.explicit_type_args(type_args.map(|t| Loc::new(e.file, t)));
        // An unterminated tagged template may still get more arguments: too few is fine.
        let incomplete = matches!(args, Args::Tagged(q) if self.template_unterminated(e.file, q));
        // An empty `<>` is a grammar error only: the call is resolved as if it had none.
        let explicit = explicit.filter(|x| !x.is_empty());
        // Candidates: correct arity and type argument count.
        let mut candidates: Vec<SigId> = Vec::new();
        let mut any_type_arity_fits = explicit.is_none();
        for &s in sigs {
            let type_arity_fits = explicit
                .as_ref()
                .is_none_or(|ex| self.sig_takes_type_args(s, ex.len()));
            any_type_arity_fits |= type_arity_fits;
            if !type_arity_fits {
                continue;
            }
            let fits = self.has_correct_arity(s, &eff)
                || (incomplete && {
                    let (_, count, has_rest) = self.arity(s, self.effective_min_args(s));
                    has_rest || eff.len() <= count
                });
            if fits {
                candidates.push(s);
            }
        }
        if candidates.is_empty() {
            // No signature takes this many type arguments: that is the error, whatever the
            // value arguments are.
            if !any_type_arity_fits && let (Some(ta), Some(ex)) = (type_args, &explicit) {
                self.report_type_arg_arity_error(e.file, ta, sigs, ex.len());
                // The arguments are still checked against the first signature with the
                // written type arguments (extra ones dropped, missing ones filled).
                let ex = ex.clone();
                return Some(self.instantiate_candidate(e, sigs[0], &eff, Some(&ex)));
            }
            // Otherwise the argument count is wrong for every signature that takes the
            // type arguments.
            if explicit.is_some() {
                let n = explicit.as_ref().map_or(0, |x| x.len());
                let fitting: Vec<SigId> = sigs
                    .iter()
                    .copied()
                    .filter(|&s| self.sig_takes_type_args(s, n))
                    .collect();
                self.report_arity_error(e, cl, &fitting, &eff);
            } else {
                self.report_arity_error(e, cl, sigs, &eff);
            }
            self.overload_failed = true;
            return sigs.first().copied();
        }
        // Explicit type arguments must satisfy the candidate's constraints; a candidate
        // that fails is not tried against the arguments.
        let mut type_arg_failure: Option<SigId> = None;
        if explicit.is_some() {
            let ex = explicit.clone().unwrap_or_default();
            candidates.retain(|&c| {
                let ok = self
                    .type_args_satisfy_constraints(c, &ex, None)
                    .is_none_or(|ok| ok);
                if !ok {
                    type_arg_failure = Some(c);
                }
                ok
            });
            if candidates.is_empty()
                && let Some(c) = type_arg_failure
            {
                let ta = type_args.map(|t| Loc::new(e.file, t));
                if self.reporting(e.file) {
                    self.type_args_satisfy_constraints(c, &ex, ta);
                }
                return Some(self.instantiate_candidate(e, c, &eff, Some(&ex)));
            }
        }
        let relations: &[RelationKind] = if candidates.len() > 1 {
            &[RelationKind::Subtype, RelationKind::Assignable]
        } else {
            &[RelationKind::Assignable]
        };
        // Overloads with context-sensitive function arguments: a candidate first checks the
        // other arguments, and only when they fit checks the functions. The first candidate
        // that checks a function fixes its parameter types for every later candidate
        // (observed: `m((x) => "" + x)` against `(cb: (x: number) => number)` and then
        // `(cb: (x: string) => string)` fits neither).
        let sens: Vec<bool> = if candidates.len() > 1 {
            eff.iter()
                .map(|a| {
                    matches!(*a, Arg::Expr(n)
                        if self.is_context_sensitive_fn_arg(Loc::new(e.file, n)))
                })
                .collect()
        } else {
            Vec::new()
        };
        // Functions nested in the other arguments (`{ cb: (x) => ... }`) are fixed the same
        // way, by the first candidate whose full check reaches them (observed: a later
        // overload's object literal without `cb` leaves `x` as the earlier one typed it).
        let mut nested: Vec<Loc> = Vec::new();
        if candidates.len() > 1 {
            for (a, &s) in eff.iter().zip(&sens) {
                if !s && let Arg::Expr(n) = *a {
                    self.nested_sensitive_fns(Loc::new(e.file, n), 0, &mut nested);
                }
            }
        }
        let fixing = sens.contains(&true) || !nested.is_empty();
        if fixing {
            for (a, _) in eff.iter().zip(&sens).filter(|x| *x.1) {
                self.fixed_arg_contexts.remove(&Loc::new(e.file, a.node()));
            }
            for l in &nested {
                self.fixed_arg_contexts.remove(l);
            }
        }
        let mut last_failures: Vec<(SigId, Failure)> = Vec::new();
        // An instance of a generic rest parameter (`...args: TS`) may take a different
        // number of arguments: the first such instance that does not fit is the arity error
        // reported if no candidate fits otherwise.
        let mut arity_failure: Option<SigId> = None;
        for &rel in relations {
            last_failures.clear();
            for &cand in &candidates {
                let inst = self.instantiate_candidate(e, cand, &eff, explicit.as_deref());
                if inst != cand
                    && self.has_generic_rest(cand)
                    && !self.has_correct_arity(inst, &eff)
                {
                    arity_failure.get_or_insert(inst);
                    continue;
                }
                let failure = if fixing {
                    match self.check_candidate_args_in(e, inst, &eff, rel, ArgPass::Skip(&sens)) {
                        Some(f) => Some(f),
                        None => {
                            // Inferring type arguments checks every argument.
                            if explicit.is_none()
                                && self.store().sig(cand).type_params != ListId::EMPTY
                            {
                                self.fix_arg_contexts(e, cand, inst, &eff, &sens);
                            }
                            self.check_candidate_args_in(e, inst, &eff, rel, ArgPass::Fix(&sens))
                        }
                    }
                } else {
                    self.check_candidate_args(e, inst, &eff, rel)
                };
                match failure {
                    None => return Some(inst),
                    Some(f) => last_failures.push((inst, f)),
                }
            }
        }
        // Report.
        self.overload_failed = true;
        if last_failures.is_empty()
            && let Some(inst) = arity_failure
        {
            self.report_arity_error(e, cl, &[inst], &eff);
            return Some(inst);
        }
        // When every overload fails and some is generic, the call stands for the overload
        // with the most parameters (the first of those), its type arguments inferred from
        // the arguments that are not context sensitive (observed: `unknown` for type
        // parameters only callbacks would infer).
        let picked = (candidates.len() > 1
            && candidates
                .iter()
                .any(|&c| self.store().sig(c).type_params != ListId::EMPTY))
        .then(|| self.longest_candidate(e, &candidates, &eff, explicit.as_deref()));
        if fixing {
            self.fix_failed_arg_contexts(e, &eff, &sens, picked, &mut last_failures);
        }
        // A single generic candidate whose other arguments already fail never infers from
        // its context-sensitive function arguments (observed: `[unknown, unknown]` for
        // `<K, E>(key: (v) => K, el: (v) => E, ..., c?: number)` given a bad `c`).
        if let [cand] = candidates[..]
            && explicit.is_none()
            && self.store().sig(cand).type_params != ListId::EMPTY
        {
            let s: Vec<bool> = eff
                .iter()
                .map(|a| {
                    matches!(*a, Arg::Expr(n)
                        if self.is_context_sensitive_fn_arg(Loc::new(e.file, n)))
                })
                .collect();
            if s.contains(&true) {
                self.skip_sensitive_inference = true;
                let inst = self.instantiate_candidate(e, cand, &eff, None);
                self.skip_sensitive_inference = false;
                if let Some(f) = self.check_candidate_args_in(
                    e,
                    inst,
                    &eff,
                    RelationKind::Assignable,
                    ArgPass::Skip(&s),
                ) {
                    let f = self
                        .check_candidate_args(e, inst, &eff, RelationKind::Assignable)
                        .unwrap_or(f);
                    last_failures = vec![(inst, f)];
                }
            }
        }
        if !self.reporting(e.file) {
            return picked.or(last_failures.last().map(|x| x.0));
        }
        if last_failures.len() == 1 {
            let (inst, f) = last_failures.remove(0);
            self.report_candidate_failure(e, &eff, f);
            return Some(inst);
        }
        let last = self.report_no_overload(e, cl, &eff, last_failures);
        picked.or(last)
    }

    /// The candidate with the most parameters (the first of those), instantiated with type
    /// arguments inferred from the arguments that are not context sensitive.
    fn longest_candidate(
        &mut self,
        e: Loc,
        candidates: &[SigId],
        eff: &[Arg],
        explicit: Option<&[TypeId]>,
    ) -> SigId {
        let mut best = candidates[0];
        let mut most = self.param_count(best).0;
        for &c in &candidates[1..] {
            let n = self.param_count(c).0;
            if n > most {
                best = c;
                most = n;
            }
        }
        self.skip_sensitive_inference = true;
        let inst = self.instantiate_candidate(e, best, eff, explicit);
        self.skip_sensitive_inference = false;
        inst
    }

    /// The context-sensitive functions in the property values and elements of an object or
    /// array literal argument (not the argument itself).
    fn nested_sensitive_fns(&self, l: Loc, depth: u32, out: &mut Vec<Loc>) {
        if depth > 8 || out.len() > 64 {
            return;
        }
        let ast = self.ast(l.file);
        let n = crate::nodes::skip_parens(ast, l.node);
        let kids: Vec<NodeId> = match *ast.node(n) {
            Node::ObjectLit { props } => ast
                .list(props)
                .iter()
                .filter_map(|&p| match *ast.node(p) {
                    Node::Property { value, .. } => Some(value),
                    _ => None,
                })
                .collect(),
            Node::ArrayLit { elements } => ast.list(elements).to_vec(),
            _ => return,
        };
        for k in kids {
            let kl = Loc::new(l.file, k);
            if self.is_context_sensitive_fn_arg(kl) {
                out.push(Loc::new(l.file, crate::nodes::skip_parens(ast, k)));
            } else {
                self.nested_sensitive_fns(kl, depth + 1, out);
            }
        }
    }

    /// The context-sensitive functions nested in argument `l` not fixed yet, if any.
    fn nested_fns_of(&self, l: Loc) -> Option<Vec<Loc>> {
        let ast = self.ast(l.file);
        if !matches!(
            ast.node(crate::nodes::skip_parens(ast, l.node)),
            Node::ObjectLit { .. } | Node::ArrayLit { .. }
        ) {
            return None;
        }
        let mut nested = Vec::new();
        self.nested_sensitive_fns(l, 0, &mut nested);
        nested.retain(|f| !self.fixed_arg_contexts.contains_key(f));
        (!nested.is_empty()).then_some(nested)
    }

    /// Fixes the context of each context-sensitive function nested in argument `l` (not
    /// fixed yet) to the one it has when `l`'s context is `ctx`.
    fn fix_nested_contexts(&mut self, l: Loc, ctx: TypeId) {
        let mut nested = Vec::new();
        self.nested_sensitive_fns(l, 0, &mut nested);
        nested.retain(|f| !self.fixed_arg_contexts.contains_key(f));
        if nested.is_empty() {
            return;
        }
        self.context_overrides.push((l, ctx));
        let fixed: Vec<(Loc, Option<TypeId>)> = nested
            .into_iter()
            .map(|f| (f, self.contextual_type(f)))
            .collect();
        self.context_overrides.pop();
        for (f, c) in fixed {
            self.fixed_arg_contexts.insert(f, c);
        }
    }

    /// Whether `l` is a function expression or arrow (in parentheses or not) with
    /// unannotated parameters.
    fn is_context_sensitive_fn_arg(&self, l: Loc) -> bool {
        let ast = self.ast(l.file);
        let n = crate::nodes::skip_parens(ast, l.node);
        matches!(ast.node(n), Node::Arrow { .. } | Node::FunctionExpr { .. })
            && self.is_context_sensitive_function(Loc::new(l.file, n))
    }

    /// Fixes the context of every context-sensitive function argument not fixed yet to
    /// that of `sig`, the instantiation of generic candidate `cand`. A parameter that is
    /// one of `cand`'s unconstrained type parameters is inferred by the function itself,
    /// so it gives no context yet (observed: `x` is implicitly `any` in
    /// `u6((x) => {}, 1)` with a first overload `<T>(a: T, y: number)`).
    fn fix_arg_contexts(&mut self, e: Loc, cand: SigId, sig: SigId, eff: &[Arg], sens: &[bool]) {
        let tps = self
            .store()
            .list_items(self.store().sig(cand).type_params)
            .to_vec();
        for (i, a) in eff.iter().enumerate() {
            let l = Loc::new(e.file, a.node());
            if !sens[i] || self.fixed_arg_contexts.contains_key(&l) {
                continue;
            }
            let raw = self.param_type_at(cand, i);
            if let Some(r) = raw
                && tps.contains(&r)
                && self.type_param_constraint(r).is_none()
            {
                self.fixed_arg_contexts.insert(l, Some(TypeId::UNKNOWN));
                continue;
            }
            if let Some(ctx) = self
                .first_context_of(l, sig)
                .or_else(|| self.param_type_at(sig, i))
            {
                self.fixed_arg_contexts.insert(l, Some(ctx));
            }
        }
    }

    /// After every overload failed: the last candidate's error comes from checking its
    /// arguments in order (fixing the functions it reaches), and the functions still not
    /// fixed get the context of `picked` (the longest candidate, when some candidate is
    /// generic), else the parameter type all candidates agree on, else none (observed: `x`
    /// is implicitly `any` for overloads taking `(x: string) => void` and
    /// `(x: number) => void`).
    fn fix_failed_arg_contexts(
        &mut self,
        e: Loc,
        eff: &[Arg],
        sens: &[bool],
        picked: Option<SigId>,
        failures: &mut [(SigId, Failure)],
    ) {
        if let Some((inst, f)) = failures.last_mut()
            && let Some(g) = self.check_candidate_args_in(
                e,
                *inst,
                eff,
                RelationKind::Assignable,
                ArgPass::Fix(sens),
            )
        {
            *f = g;
        }
        let insts: Vec<SigId> = failures.iter().map(|x| x.0).collect();
        for (i, a) in eff.iter().enumerate() {
            let l = Loc::new(e.file, a.node());
            if !sens[i] || self.fixed_arg_contexts.contains_key(&l) {
                continue;
            }
            let ctx = if let Some(p) = picked {
                self.param_type_at(p, i)
            } else {
                let mut agreed: Option<TypeId> = None;
                let mut same = true;
                for &s in &insts {
                    match (self.param_type_at(s, i), agreed) {
                        (Some(t), None) => agreed = Some(t),
                        (Some(t), Some(u)) if t == u || self.is_identical(t, u) => {}
                        _ => {
                            same = false;
                            break;
                        }
                    }
                }
                agreed.filter(|_| same)
            };
            self.fixed_arg_contexts.insert(l, ctx);
        }
    }

    /// Whether explicit type arguments satisfy `sig`'s type parameter constraints (`None`
    /// when the signature is not generic). With `report`, each failing argument gets
    /// TS2344 (or the relation's own message) at its node.
    pub(crate) fn type_args_satisfy_constraints(
        &mut self,
        sig: SigId,
        explicit: &[TypeId],
        report: Option<Loc>,
    ) -> Option<bool> {
        let d = self.store().sig(sig);
        if d.type_params == ListId::EMPTY {
            return None;
        }
        let tps = self.store().list_items(d.type_params).to_vec();
        let filled = self.fill_type_args(&tps, explicit);
        let m = self.store().mapper(&tps, &filled);
        let nodes: Vec<NodeId> = match report {
            Some(l) => match *self.ast(l.file).node(l.node) {
                Node::TypeArgs { args } => self.ast(l.file).list(args).to_vec(),
                _ => Vec::new(),
            },
            None => Vec::new(),
        };
        let mut ok = true;
        for (i, &at) in explicit.iter().enumerate().take(tps.len()) {
            let Some(c) = self.type_param_constraint(tps[i]) else {
                continue;
            };
            let c = self.instantiate(c, m);
            if self.store().is_error(at) || at == TypeId::UNRESOLVED || c == TypeId::UNRESOLVED {
                continue;
            }
            if self.is_assignable(at, c) {
                continue;
            }
            ok = false;
            if let (Some(l), Some(&n)) = (report, nodes.get(i)) {
                let (chain, _) = self.relation_error_chain(
                    at,
                    c,
                    RelationKind::Assignable,
                    Some(crate::relate::Head {
                        info: dg::TYPE_0_DOES_NOT_SATISFY_THE_CONSTRAINT_1_2344,
                    }),
                );
                self.error_chain(Loc::new(l.file, n), &chain);
                // Only the first failing argument is reported.
                break;
            }
        }
        Some(ok)
    }

    fn min_type_args(&mut self, tps: &[TypeId]) -> usize {
        let mut min = tps.len();
        while min > 0 && self.type_param_default(tps[min - 1]).is_some() {
            min -= 1;
        }
        min
    }

    /// The arity error for a call with no candidate of the right arity.
    /// Whether `s` accepts `n` explicit type arguments (between its required and total
    /// type parameter counts).
    fn sig_takes_type_args(&mut self, s: SigId, n: usize) -> bool {
        let (min, max) = self.type_arg_bounds(s);
        n >= min && n <= max
    }

    /// The fewest and most type arguments `s` takes.
    fn type_arg_bounds(&mut self, s: SigId) -> (usize, usize) {
        let tps = self
            .store()
            .list_items(self.store().sig(s).type_params)
            .to_vec();
        (self.min_type_args(&tps), tps.len())
    }

    /// TS2558 / TS2743 for a call whose `n` type arguments no signature takes. One signature:
    /// its range. Overloads: the nearest counts below and above `n`; both present gives
    /// TS2743, otherwise TS2558 names whichever exists.
    fn report_type_arg_arity_error(
        &mut self,
        file: ts_r_diagnostics::FileId,
        ta: NodeId,
        sigs: &[SigId],
        n: usize,
    ) {
        let got = n.to_string();
        let (code, first, second, third) = if let [s] = sigs {
            let (min, max) = self.type_arg_bounds(*s);
            let expected = if min == max {
                min.to_string()
            } else {
                format!("{min}-{max}")
            };
            (
                dg::EXPECTED_0_TYPE_ARGUMENTS_BUT_GOT_1_2558,
                expected,
                got,
                None,
            )
        } else {
            let mut below: Option<usize> = None;
            let mut above: Option<usize> = None;
            for &s in sigs {
                let (min, max) = self.type_arg_bounds(s);
                if min > n {
                    above = Some(above.map_or(min, |a| a.min(min)));
                } else if max < n {
                    below = Some(below.map_or(max, |b| b.max(max)));
                }
            }
            match (below, above) {
                (Some(b), Some(a)) => (
                    dg::NO_OVERLOAD_EXPECTS_0_TYPE_ARGUMENTS_BUT_OVERLOADS_DO_EXIST_THAT_EXPECT_EITHER_1_2743,
                    got,
                    b.to_string(),
                    Some(a.to_string()),
                ),
                (b, a) => (
                    dg::EXPECTED_0_TYPE_ARGUMENTS_BUT_GOT_1_2558,
                    b.or(a).unwrap_or(0).to_string(),
                    got,
                    None,
                ),
            }
        };
        let args3;
        let args: &[&str] = match &third {
            Some(t) => {
                args3 = [first.as_str(), second.as_str(), t.as_str()];
                &args3
            }
            None => &[first.as_str(), second.as_str()],
        };
        // Reported over the type arguments themselves, not the brackets.
        let ast = self.ast(file);
        let span = match *ast.node(ta) {
            Node::TypeArgs { args } => match (ast.list(args).first(), ast.list(args).last()) {
                (Some(&a), Some(&b)) => Some(Span::new(
                    self.c.span(Loc::new(file, a)).start,
                    self.c.span(Loc::new(file, b)).end,
                )),
                _ => None,
            },
            _ => None,
        };
        match span {
            Some(sp) => self.error_span(file, sp, code, args),
            None => self.error(Loc::new(file, ta), code, args),
        }
    }

    fn report_arity_error(&mut self, e: Loc, cl: Loc, sigs: &[SigId], eff: &[Arg]) {
        if !self.reporting(e.file) {
            return;
        }
        let n = eff.len();
        // Fewest required and most accepted arguments over the signatures; the largest
        // required count under `n` and the smallest accepted count over it.
        let mut min_all = usize::MAX;
        let mut max_all = 0usize;
        let mut has_rest = false;
        let mut largest_min_below: Option<usize> = None;
        let mut smallest_count_above: Option<usize> = None;
        for &s in sigs {
            let (min, count, rest) = self.arity(s, self.store().sig(s).min_args as usize);
            min_all = min_all.min(min);
            max_all = max_all.max(count);
            has_rest |= rest;
            if min < n && largest_min_below.is_none_or(|b| min > b) {
                largest_min_below = Some(min);
            }
            if count > n && smallest_count_above.is_none_or(|a| count < a) {
                smallest_count_above = Some(count);
            }
        }
        if let Some(pos) = eff.iter().position(|a| matches!(a, Arg::Rest(..))) {
            let node = eff[pos].node();
            self.error(Loc::new(e.file, node), dg::A_SPREAD_ARGUMENT_MUST_EITHER_HAVE_A_TUPLE_TYPE_OR_BE_PASSED_TO_A_REST_PARAMETER_2556, &[]);
            return;
        }
        // Strictly inside the overall range but matching no overload.
        if min_all < n
            && n < max_all
            && let (Some(below), Some(above)) = (largest_min_below, smallest_count_above)
        {
            self.error_span(
                e.file,
                self.call_error_span(e, cl),
                dg::NO_OVERLOAD_EXPECTS_0_ARGUMENTS_BUT_OVERLOADS_DO_EXIST_THAT_EXPECT_EITHER_1_OR_2_2575,
                &[&n.to_string(), &below.to_string(), &above.to_string()],
            );
            return;
        }
        let expected = if has_rest {
            None
        } else if min_all == max_all {
            Some(min_all.to_string())
        } else {
            Some(format!("{min_all}-{max_all}"))
        };
        let ast = self.ast(e.file);
        // Elements of a spread tuple are arguments too: the span starts at the spread.
        let too_few = n < min_all;
        let too_many = !too_few && n > max_all;
        let span = if too_many {
            let first = eff[max_all].node();
            let last = eff[n - 1].node();
            let mut start = ast.span(first).start;
            // A missing argument (`${ ` at the end of a broken template) is empty: its
            // span starts at the next token, past the whitespace.
            if ast.span(first).is_empty() {
                let text = self.c.file(e.file).text.as_bytes();
                while text
                    .get(start as usize)
                    .is_some_and(|b| b.is_ascii_whitespace())
                {
                    start += 1;
                }
            }
            Span::new(start, ast.span(last).end.max(start))
        } else {
            self.call_error_span(e, cl)
        };
        let mark = self.diags.len();
        match expected {
            Some(x) => self.error_span(
                e.file,
                span,
                dg::EXPECTED_0_ARGUMENTS_BUT_GOT_1_2554,
                &[&x, &n.to_string()],
            ),
            None => self.error_span(
                e.file,
                span,
                dg::EXPECTED_AT_LEAST_0_ARGUMENTS_BUT_GOT_1_2555,
                &[&min_all.to_string(), &n.to_string()],
            ),
        }
        if too_few {
            self.relate_missing_argument(mark, sigs, n);
        }
    }

    /// Too few arguments: related information at the first parameter without an argument
    /// ("An argument for 'a' was not provided.", TS6211 for a binding pattern, TS6236 for a
    /// rest parameter), of the signature needing the fewest arguments above `n` (the first
    /// such overload; observed).
    fn relate_missing_argument(&mut self, mark: usize, sigs: &[SigId], n: usize) {
        let mut best: Option<(usize, SigId)> = None;
        for &s in sigs {
            let (min, _, _) = self.arity(s, self.store().sig(s).min_args as usize);
            if min > n && best.is_none_or(|(m, _)| min < m) {
                best = Some((min, s));
            }
        }
        let Some((_, sig)) = best else {
            return;
        };
        let Some((f, node)) = self.store().sig(sig).decl else {
            return;
        };
        let file = ts_r_diagnostics::FileId(f);
        let Some(node) = NodeId::from_raw(node) else {
            return;
        };
        let ast = self.ast(file);
        let Some(parts) = crate::nodes::fn_parts(ast, node) else {
            return;
        };
        let params: Vec<NodeId> = ast
            .list(parts.params)
            .iter()
            .copied()
            .filter(|&p| !crate::nodes::is_this_param(ast, p))
            .collect();
        let Some(&p) = params.get(n) else {
            return;
        };
        let Node::Param { rest, pattern, .. } = *ast.node(p) else {
            return;
        };
        let span = ast.span(p);
        match ast.node(pattern) {
            Node::Ident { name } => {
                let name = ast.atom(*name).to_owned();
                let info = if rest {
                    dg::ARGUMENTS_FOR_THE_REST_PARAMETER_0_WERE_NOT_PROVIDED_6236
                } else {
                    dg::AN_ARGUMENT_FOR_0_WAS_NOT_PROVIDED_6210
                };
                self.relate_last_span(mark, file, span, info, &[&name]);
            }
            _ => self.relate_last_span(
                mark,
                file,
                span,
                dg::AN_ARGUMENT_MATCHING_THIS_BINDING_PATTERN_WAS_NOT_PROVIDED_6211,
                &[],
            ),
        }
    }

    /// tsc's error span for a call: the callee (the name of a member access callee: `m` of
    /// `o.m()` and `o?.m()`, otherwise the whole callee: `o["m"]`, `(o.m)`, `o.m!`); a `new`
    /// expression is the whole expression; other call-likes run from the callee's start to
    /// the end of the call.
    fn call_error_span(&self, e: Loc, cl: Loc) -> Span {
        let ast = self.ast(e.file);
        // `new X()`: the whole expression.
        if matches!(ast.node(e.node), Node::New { .. }) {
            return ast.span(e.node);
        }
        if let Node::Call { callee, .. } = *ast.node(e.node) {
            return match *ast.node(callee) {
                Node::Member { prop, .. } => ast.span(prop),
                _ => ast.span(callee),
            };
        }
        let start = match *ast.node(cl.node) {
            Node::Member { prop, .. } => ast.span(prop).start,
            _ => ast.span(cl.node).start,
        };
        let end = ast.span(e.node).end;
        Span::new(start, end)
    }

    /// Instantiates a generic candidate with explicit or inferred type arguments.
    fn instantiate_candidate(
        &mut self,
        e: Loc,
        sig: SigId,
        eff: &[Arg],
        explicit: Option<&[TypeId]>,
    ) -> SigId {
        let d = self.store().sig(sig);
        if d.type_params == ListId::EMPTY {
            return sig;
        }
        let tps = self.store().list_items(d.type_params).to_vec();
        let args: Vec<TypeId> = match explicit {
            Some(ex) => self.fill_type_args(&tps, ex),
            None => self.infer_call_type_args(e, sig, eff),
        };
        let m = self.store().mapper(&tps, &args);
        self.instantiate_signature_full(sig, m)
    }

    /// A mapper from the type parameters to the types inferred from the call's contextual
    /// type to the declared return type (parameters without inferences stay themselves).
    fn return_context_mapper(
        &mut self,
        ctx: &InferenceContext,
        rctx: Option<&InferenceContext>,
    ) -> Option<ts_r_types::MapperId> {
        let rctx = rctx?;
        let mut src = Vec::new();
        let mut dst = Vec::new();
        for (i, &p) in ctx.params.iter().enumerate() {
            let mut cands = rctx.candidates[i].clone();
            if cands.is_empty() {
                cands = rctx.weak[i].clone();
            }
            if cands.is_empty() {
                continue;
            }
            let t = self.union(&cands);
            if t == TypeId::UNRESOLVED || self.store().is_error(t) {
                continue;
            }
            src.push(p);
            dst.push(t);
        }
        if src.is_empty() {
            return None;
        }
        Some(self.store().mapper(&src, &dst))
    }

    /// `t` with the type parameters that have inferences so far replaced by them (without
    /// fixing); the others stay.
    fn instantiate_with_inferred_so_far(&mut self, ctx: &InferenceContext, t: TypeId) -> TypeId {
        let mut src = Vec::new();
        let mut dst = Vec::new();
        for (j, &p) in ctx.params.iter().enumerate() {
            let it = match ctx.fixed[j] {
                Some(f) => f,
                None if !ctx.candidates[j].is_empty() || !ctx.contra[j].is_empty() => {
                    let mut c2 = ctx.clone();
                    self.inferred_type_at(&mut c2, j)
                }
                None => continue,
            };
            if it == TypeId::UNRESOLVED || self.store().is_error(it) {
                continue;
            }
            src.push(p);
            dst.push(it);
        }
        if src.is_empty() {
            return t;
        }
        let m = self.store().mapper(&src, &dst);
        self.instantiate(t, m)
    }

    /// A parameter type as an argument's contextual type, given the return context mapper.
    /// For a generic mapped return type (`Readonly<T>`) the whole type is instantiated;
    /// otherwise only a type parameter at the top level (`T`, `T | undefined`), and a
    /// resulting `boolean` keeps boolean literals widening (`pair({ x: true }, { x: false })`
    /// against `{ x: boolean }` and `Map` entries in `new Map([...])` are unaffected).
    fn instantiate_return_context(
        &mut self,
        pt: TypeId,
        m: ts_r_types::MapperId,
        full: bool,
    ) -> TypeId {
        if full {
            return self.instantiate(pt, m);
        }
        let is_param =
            |ck: &Self, t: TypeId| ck.store().flags(t).contains(TypeFlags::TYPE_PARAMETER);
        let s = self.store();
        let r = if is_param(self, pt) {
            self.instantiate(pt, m)
        } else if s.flags(pt).contains(TypeFlags::UNION)
            && self.some_member(pt, |x| is_param(self, x))
        {
            self.map_type(pt, |ck, x| {
                if is_param(ck, x) {
                    ck.instantiate(x, m)
                } else {
                    x
                }
            })
        } else {
            return pt;
        };
        let has_true = self.some_member(r, |x| x == TypeId::TRUE);
        let has_false = self.some_member(r, |x| x == TypeId::FALSE);
        if has_true && has_false {
            return self.filter_type(r, |_, x| x != TypeId::TRUE && x != TypeId::FALSE);
        }
        r
    }

    /// Inferences from the call's contextual type to the declared return type, and whether
    /// that contextual type mentions an outer call's type parameters. They have the lowest
    /// priority: a type parameter uses them only when the arguments infer nothing for it
    /// (`const x: Output<R> = invoke()` infers `R`). Inferences mentioning outer type
    /// parameters only contextually type deferred arguments (`new Map(xs.map(x => [k, x]))`
    /// makes the array literal a tuple).
    fn return_context_inference(
        &mut self,
        e: Loc,
        ctx: &InferenceContext,
    ) -> Option<(InferenceContext, bool, InferenceContext)> {
        let ret = ctx.ret?;
        if !self.could_contain_type_params(ret) {
            return None;
        }
        let c = self.contextual_type(e)?;
        if self
            .store()
            .flags(c)
            .intersects(TypeFlags::ANY_OR_UNKNOWN | TypeFlags::VOID)
        {
            return None;
        }
        // The type parameters of enclosing declarations are fixed types here (`return
        // make()` in `function f<R>(): R` infers `R`); only those of an outer call being
        // inferred make the context generic.
        let generic = self.mentions_free_type_params(e, c);
        let mut rctx = InferenceContext::new(&ctx.params);
        rctx.from_return_context = true;
        self.infer_types(&mut rctx, c, ret, 0);
        // A parameter of a returned function (`(self: Effect<A>) => Effect<A>` against
        // `(a: Effect<boolean>) => B`) infers contravariantly; with nothing else, that is
        // what the context implies.
        for j in 0..rctx.params.len() {
            if rctx.candidates[j].is_empty() && rctx.contra[j].len() == 1 {
                let t = rctx.contra[j][0];
                if t != TypeId::UNRESOLVED && !self.store().is_error(t) {
                    rctx.candidates[j].push(t);
                }
            }
        }
        // What the context implies once the outer calls' type parameters still being
        // inferred infer nothing (`pipe(tap({ next(r) {} }))` against `OperatorFunction<V,
        // E, V1, E1>` infers `TValue = V` from the source parameter, not `V1`).
        let rinf = if generic && !self.inferring_params.is_empty() {
            let open = self.inferring_params.clone();
            let mut r = InferenceContext::new(&ctx.params);
            self.infer_types(&mut r, c, ret, 0);
            for j in 0..r.params.len() {
                for list in [&mut r.candidates[j], &mut r.contra[j], &mut r.weak[j]] {
                    let keep: Vec<TypeId> = list
                        .iter()
                        .copied()
                        .filter(|&t| !open.iter().any(|&p| self.type_mentions(t, p)))
                        .collect();
                    *list = keep;
                }
                if r.candidates[j].is_empty() && r.contra[j].len() == 1 {
                    let t = r.contra[j][0];
                    if t != TypeId::UNRESOLVED && !self.store().is_error(t) {
                        r.candidates[j].push(t);
                    }
                }
            }
            r
        } else {
            rctx.clone()
        };
        Some((rctx, generic, rinf))
    }

    /// `t` mentions type parameters other than those of the declarations enclosing `e`
    /// (which are fixed types there): an outer call's type parameters being inferred.
    fn mentions_free_type_params(&mut self, e: Loc, t: TypeId) -> bool {
        if !self.could_contain_type_params(t) {
            return false;
        }
        let mut outer = self.outer_type_params(e);
        // So are those of the generic signature contextually typing an enclosing function
        // (`const f: <R>() => R = () => make()`), unless an outer call is inferring them.
        for f in self.enclosing_function_exprs(e) {
            let Some(sig) = self.contextual_signature(f) else {
                continue;
            };
            let tps = self.store().list_items(self.store().sig(sig).type_params);
            for &tp in tps {
                if !outer.contains(&tp) && !self.inferring_params.contains(&tp) {
                    outer.push(tp);
                }
            }
        }
        if outer.is_empty() {
            return true;
        }
        let (count, overflow) = (self.inst_count, self.inst_overflow);
        let fill = vec![TypeId::UNKNOWN; outer.len()];
        let m = self.store().mapper(&outer, &fill);
        let r = self.instantiate(t, m);
        self.inst_count = count;
        self.inst_overflow = overflow;
        self.could_contain_type_params(r)
    }

    /// The function and arrow expressions enclosing `e`, innermost first.
    fn enclosing_function_exprs(&self, e: Loc) -> Vec<Loc> {
        let ast = self.ast(e.file);
        let parents = self.c.file(e.file).parents();
        let mut out = Vec::new();
        let mut cur = parents.parent(e.node);
        let mut steps = 0;
        while let Some(n) = cur {
            steps += 1;
            if steps > 100_000 {
                break;
            }
            if matches!(ast.node(n), Node::FunctionExpr { .. } | Node::Arrow { .. }) {
                out.push(Loc::new(e.file, n));
            }
            cur = parents.parent(n);
        }
        out
    }

    /// The receiver of a method call (`o` in `o.m()` and `o[k]()`), and whether it is
    /// reached through `?.` (`o?.m()`).
    fn call_receiver(&self, e: Loc) -> Option<(Loc, bool)> {
        let ast = self.ast(e.file);
        let Node::Call { callee, .. } = *ast.node(e.node) else {
            return None;
        };
        // Parentheses and non-null assertions around the callee keep its receiver
        // (`(o.f)()`, `o.f!()`).
        let mut callee = crate::nodes::skip_parens(ast, callee);
        for _ in 0..10_000 {
            let Node::NonNull { expr } = *ast.node(callee) else {
                break;
            };
            callee = crate::nodes::skip_parens(ast, expr);
        }
        match *ast.node(callee) {
            Node::Member { obj, optional, .. } | Node::Index { obj, optional, .. } => {
                Some((Loc::new(e.file, obj), optional))
            }
            _ => None,
        }
    }

    /// A generic rest parameter that is not a plain array (`...args: T`, `...args: [...T]`,
    /// `...args: [string, ...T]`) infers from the arguments in its positions as one tuple
    /// (`f(1, "a")` gives `T = [number, string]`). Returns the first position it covered, or
    /// `usize::MAX` when the arguments infer one by one.
    fn infer_from_rest_args(
        &mut self,
        e: Loc,
        sig: SigId,
        eff: &[Arg],
        ctx: &mut InferenceContext,
    ) -> usize {
        let d = self.store().sig(sig);
        let n = d.params.len();
        if !d.has_rest() || n == 0 || eff.len() < n - 1 {
            return usize::MAX;
        }
        let rt = d.params[n - 1].ty;
        let generic_rest = self.store().flags(rt).intersects(TypeFlags::INSTANTIABLE)
            || self.store().tuple_shape_of(rt).is_some_and(|s| {
                s.elements
                    .iter()
                    .any(|el| el.kind == TupleElement::VARIADIC)
            });
        if !generic_rest || !self.could_contain_type_params(rt) {
            return usize::MAX;
        }
        let start = n - 1;
        // A `const` rest type parameter infers a readonly tuple of literals (mutable when its
        // constraint is a mutable array).
        let const_rest = if self.is_const_type_variable(rt, 0) {
            let c = self.base_constraint(rt);
            let members = match self.store().members_of_union(c) {
                [] => vec![c],
                m => m.to_vec(),
            };
            Some(
                !members
                    .into_iter()
                    .any(|m| self.c.is_array_like_type(m) && !self.is_readonly_array_like(m)),
            )
        } else {
            None
        };
        let mut els = Vec::with_capacity(eff.len() - start);
        for (i, &a) in eff.iter().enumerate().skip(start) {
            match a {
                Arg::Expr(node) => {
                    let l = Loc::new(e.file, node);
                    // Contextually typed by the element type of the rest's constraint
                    // (`...fns: T` with `T extends ((x: number) => void)[]`).
                    // A `const` rest type parameter types each argument by its element
                    // (`T[0]`), which keeps it in a const context.
                    let pt = if self.is_const_type_variable(rt, 0) {
                        let idx = self.store().number_literal((i - start) as f64, false);
                        self.store().indexed_access(rt, idx)
                    } else if self.store().flags(rt).intersects(TypeFlags::INSTANTIABLE) {
                        let c = self.base_constraint(rt);
                        self.element_type_of_array_like(c)
                            .unwrap_or(TypeId::UNKNOWN)
                    } else {
                        self.param_type_at(sig, i).unwrap_or(TypeId::UNKNOWN)
                    };
                    let at = self.speculative_arg_type(l, pt);
                    let at = if const_rest.is_some() || self.is_literal_of_contextual_type(at, pt) {
                        self.regular_of_literal(at)
                    } else {
                        self.widen_literal(at)
                    };
                    els.push(crate::declared::TupleEl {
                        ty: at,
                        kind: TupleElement::REQUIRED,
                        label: None,
                    });
                }
                Arg::Elem(_, t) => els.push(crate::declared::TupleEl {
                    ty: t,
                    kind: TupleElement::REQUIRED,
                    label: None,
                }),
                Arg::Rest(_, t) => {
                    let arr = self.create_array(t, false);
                    els.push(crate::declared::TupleEl {
                        ty: arr,
                        kind: TupleElement::REST,
                        label: None,
                    });
                }
            }
        }
        // A lone spread of an array is that array (`f(...xs)` infers `number[]`).
        let tup = match els[..] {
            [crate::declared::TupleEl { ty, kind, .. }] if kind == TupleElement::REST => ty,
            _ => self.create_tuple(&els, const_rest == Some(true)),
        };
        if tup == TypeId::ERROR {
            return usize::MAX;
        }
        self.infer_types(ctx, tup, rt, 0);
        start
    }

    fn infer_call_type_args(&mut self, e: Loc, sig: SigId, eff: &[Arg]) -> Vec<TypeId> {
        let depth = self.inferring_params.len();
        let mdepth = self.ret_ctx_mappers.len();
        let r = self.infer_call_type_args_inner(e, sig, eff);
        self.inferring_params.truncate(depth);
        self.ret_ctx_mappers.truncate(mdepth);
        r
    }

    /// A contextual return type that is a type parameter of a call being inferred (`() =>
    /// TValue` for `run<TValue>(fn)` in `const t: [A, B] = run(() => [a, b])`), read
    /// through what that call's contextual type implies for it.
    pub(crate) fn instantiate_with_return_context(&mut self, t: TypeId) -> TypeId {
        if self.ret_ctx_mappers.is_empty()
            || !self.some_member(t, |m| {
                self.store().flags(m).contains(TypeFlags::TYPE_PARAMETER)
            })
        {
            return t;
        }
        for m in self.ret_ctx_mappers.clone().into_iter().rev() {
            let r = self.instantiate_return_context(t, m, false);
            if r != t {
                return r;
            }
        }
        t
    }

    fn infer_call_type_args_inner(&mut self, e: Loc, sig: SigId, eff: &[Arg]) -> Vec<TypeId> {
        // (Taken here, so calls nested in the arguments infer as usual.)
        let skip_sensitive = std::mem::take(&mut self.skip_sensitive_inference);
        let mut ctx: InferenceContext = self.inference_context_for(sig);
        let rinf = self.return_context_inference(e, &ctx);
        let (rctx, rctx_generic, rctx_inf) = match rinf {
            Some((r, g, i)) => (Some(r), g, Some(i)),
            None => (None, false, None),
        };
        self.inferring_params.extend(ctx.params.iter().copied());
        let ret_mapper = if rctx_generic {
            None
        } else {
            self.return_context_mapper(&ctx, rctx.as_ref())
        };
        if let Some(m) = ret_mapper {
            self.ret_ctx_mappers.push(m);
        }
        let ret_is_mapped = ctx.ret.is_some_and(|r| {
            self.store()
                .object_of(r)
                .is_some_and(|o| matches!(o.kind, ts_r_types::ObjectKind::Mapped { .. }))
        });
        let literal_ret_ctx = rctx.as_ref().is_some_and(|r| {
            r.candidates.iter().any(|c| !c.is_empty())
                && r.candidates.iter().flatten().all(|&t| {
                    self.every_member(t, |m| {
                        self.store()
                            .flags(m)
                            .intersects(TypeFlags::UNIT | TypeFlags::PRIMITIVE)
                    })
                })
        });
        // A declared `this` parameter infers from the receiver of a method call first
        // (`f.call(undefined, 1)` against `CallableFunction.call<T, A, R>(this: (this: T,
        // ...args: A) => R, ...)`). A parameter the receiver's parameters alone determine
        // is fixed by them (`other.apply(this, arguments)` with `other: () => void` checks
        // `arguments` against `[]`).
        let mut receiver: Option<(TypeId, TypeId)> = None;
        if let Some(tt) = self.store().sig(sig).this_type
            && let Some((recv, optional)) = self.call_receiver(e)
        {
            let rt = self.check_expr(recv);
            // `o?.m()` calls `m` on `o` without `null` and `undefined`.
            let rt = if optional {
                self.non_nullable_strict(rt)
            } else {
                rt
            };
            if rt != TypeId::UNRESOLVED && !self.store().is_error(rt) {
                receiver = Some((rt, tt));
                // A generic receiver infers through its base signature (type parameters
                // as their constraints: `constructFrom.bind(null, d)` returns `Date`).
                let mut tctx = InferenceContext::new(&ctx.params);
                tctx.base_signatures = true;
                self.infer_types(&mut tctx, rt, tt, 0);
                for j in 0..ctx.params.len() {
                    if tctx.candidates[j].is_empty()
                        && let [c] = tctx.contra[j][..]
                        && !self.could_contain_type_params(c)
                    {
                        ctx.fixed[j] = Some(c);
                    }
                }
                let base = std::mem::replace(&mut ctx.base_signatures, true);
                self.infer_types(&mut ctx, rt, tt, 0);
                ctx.base_signatures = base;
            }
        }
        let rest_from = self.infer_from_rest_args(e, sig, eff, &mut ctx);
        // The receiver again, now that the rest arguments imply the arity of `A` in
        // `CallableFunction.bind`'s `(this: T, ...args: [...A, ...B]) => R`.
        if rest_from != usize::MAX
            && let Some((rt, tt)) = receiver
        {
            ctx.clear_visited();
            let base = std::mem::replace(&mut ctx.base_signatures, true);
            self.infer_types(&mut ctx, rt, tt, 0);
            ctx.base_signatures = base;
        }
        let mut deferred: Vec<usize> = Vec::new();
        let mut generic_args: Vec<(TypeId, TypeId)> = Vec::new();
        for (i, &a) in eff.iter().enumerate() {
            if i >= rest_from {
                break;
            }
            let Some(pt) = self.param_type_at(sig, i) else {
                continue;
            };
            match a {
                Arg::Expr(n) => {
                    let l = Loc::new(e.file, n);
                    if self.is_context_sensitive_expr(l) {
                        // The arms of a conditional that are not context sensitive infer
                        // now (`c ? (s) => {} : init` infers from `init`).
                        for arm in self.context_free_conditional_arms(l) {
                            let at = self.speculative_arg_type(arm, pt);
                            self.infer_types(&mut ctx, at, pt, 0);
                        }
                        // So do the elements of an array literal that are not
                        // (`[makeFooer(), { foo: (v) => v }]` infers `Fooer` for `T[]`).
                        if let Some(at) = self.context_free_array_elements_type(l, pt) {
                            self.infer_types(&mut ctx, at, pt, 0);
                        }
                        deferred.push(i);
                        continue;
                    }
                    // Arguments are contextually typed by what the call's contextual type
                    // implies for the return type.
                    // Array literal types are not told apart from other arrays, so their
                    // candidates cannot combine like object literals': keep them widening.
                    let array_lit = matches!(self.ast(e.file).node(n), Node::ArrayLit { .. });
                    // An array literal still sees literal types the return context implies
                    // (`const s: ReadonlySet<"a" | "b"> = new Set(["a"])` infers `"a"`).
                    // A parameter type mentioning a `const` type parameter stays the
                    // contextual type (its literals are in a const context); the return
                    // context only adds inferences.
                    let ret_mapper_here = if ret_mapper.is_some()
                        && ctx.params.clone().into_iter().any(|tp| {
                            self.is_const_type_variable(tp, 0) && self.type_mentions(pt, tp)
                        }) {
                        None
                    } else {
                        ret_mapper
                    };
                    let cty = match ret_mapper_here {
                        Some(m) if array_lit && literal_ret_ctx => self.instantiate(pt, m),
                        Some(m) if ret_is_mapped || !array_lit => {
                            self.instantiate_return_context(pt, m, ret_is_mapped)
                        }
                        // A tuple-like constraint the context implies is an array (`T extends
                        // readonly unknown[] | []` with `T = Post[]`): no tuple.
                        // So is an array of tuples (`[string, number][]`): its elements
                        // stay tuple contexts.
                        Some(m) => {
                            let tuple_like = self.is_tuple_like_context(pt);
                            let r = self.instantiate_return_context(pt, m, false);
                            if r != pt
                                && self.c.is_array_type(r)
                                && (tuple_like || self.is_array_of_tuples(r))
                            {
                                r
                            } else {
                                pt
                            }
                        }
                        _ => pt,
                    };
                    // A generic call argument infers from its contextual type with what the
                    // earlier arguments inferred (`pipe(effect, repeat({ until: x => x }))`
                    // gives `repeat` the context `(a: Effect<boolean>) => B`), also as the
                    // arm of a conditional (`c ? orElse(d) : orThrow()`).
                    // Type parameters with no inferences yet are seen as what the call's
                    // contextual type implies (`const n: number[] = first(mk(r => r(1)))`
                    // gives `mk` the context `number[]`).
                    let cty = if self.is_generic_call_arg(Loc::new(e.file, n)) {
                        let c = self.instantiate_with_inferred_so_far(&ctx, cty);
                        match ret_mapper {
                            Some(m) if self.could_contain_type_params(c) => self.instantiate(c, m),
                            _ => c,
                        }
                    } else {
                        cty
                    };
                    // A function argument that is not context sensitive anywhere inside is
                    // typed once, by its first contextual type (inner calls see the
                    // parameters still being inferred): the final check sees it again.
                    let fun = crate::nodes::skip_parens(self.ast(e.file), n);
                    if self.speculative == 0
                        && matches!(
                            self.ast(e.file).node(fun),
                            Node::Arrow { .. } | Node::FunctionExpr { .. }
                        )
                        && !self.contains_context_sensitive_function(Loc::new(e.file, fun))
                    {
                        self.first_arg_context.entry((l, sig)).or_insert(cty);
                    }
                    let mut at = self.speculative_arg_type(l, cty); // A literal of a literal contextual type is not widened later
                    // (`const w: Wrap<"foo"> = wrap("foo")` infers `"foo"`).
                    if self
                        .store()
                        .flags(cty)
                        .intersects(TypeFlags::LITERAL | TypeFlags::UNION)
                        && self.is_literal_of_contextual_type(at, cty)
                    {
                        at = self.regular_of_literal(at);
                    }
                    // A generic function whose contextual parameter types mention the
                    // call's type parameters is inferred from after the other arguments
                    // (`map(identity, "")` instantiates `identity` with `S = string`).
                    if self.is_generic_function_for(at, pt, &ctx.params) {
                        generic_args.push((at, pt));
                        continue;
                    }
                    self.infer_types(&mut ctx, at, pt, 0);
                }
                Arg::Elem(_, t) | Arg::Rest(_, t) => self.infer_types(&mut ctx, t, pt, 0),
            }
        }
        // Generic function arguments see the inferences so far (nothing is fixed).
        for (at, pt) in generic_args {
            self.infer_types(&mut ctx, at, pt, 0);
        }
        if skip_sensitive {
            deferred.clear();
        }
        for i in deferred {
            let Arg::Expr(n) = eff[i] else { continue };
            let Some(pt) = self.param_type_at(sig, i) else {
                continue;
            };
            // Type parameters the contextual parameter types mention are fixed now (their
            // inference so far, `unknown` without candidates); the others map to what is
            // known so far without fixing.
            let tps = ctx.params.clone();
            let mut mentioned = vec![false; tps.len()];
            let ptn = self.non_nullable(pt);
            let mut pt_calls = self.signatures_of(ptn, SigKind::Call);
            let callable = !pt_calls.is_empty();
            // Functions nested in an array or object literal argument fix what their own
            // contextual signatures mention (`[cb => cb(null, 1)]` against `Task<T>[]`).
            let nested = !matches!(
                self.ast(e.file)
                    .node(crate::nodes::skip_parens(self.ast(e.file), n)),
                Node::Arrow { .. } | Node::FunctionExpr { .. }
            );
            if nested {
                pt_calls.clear();
                // (A homomorphic mapped type infers by reversing it: nothing to fix.)
                // (Only when every element or property is such a function: the others would
                // infer first.)
                if self.is_generic_mapped_type(ptn)
                    || !self.nested_context_sigs(Loc::new(e.file, n), pt, 0, &mut pt_calls)
                {
                    pt_calls.clear();
                }
                // A function returning a type parameter that a later one's parameters mention
                // infers it before that one is typed (intra-expression inference, not
                // modelled here).
                let mut returned = vec![false; tps.len()];
                for &cs in &pt_calls.clone() {
                    let d = self.store().sig(cs);
                    let ptys: Vec<TypeId> = d.params.iter().map(|p| p.ty).collect();
                    let rt = self.return_type_of(cs);
                    for (j, &tp) in tps.iter().enumerate() {
                        if returned[j] && ptys.iter().any(|&t| self.type_mentions(t, tp)) {
                            pt_calls.clear();
                            break;
                        }
                    }
                    if pt_calls.is_empty() {
                        break;
                    }
                    for (j, &tp) in tps.iter().enumerate() {
                        if self.type_mentions(rt, tp) {
                            returned[j] = true;
                        }
                    }
                }
            }
            // Only the positions where a function argument has a parameter without a type
            // annotation take a contextual type (`(set: string[], part) => ..` leaves the
            // `U` of `prev: U` to be inferred from the body as well).
            let untyped = if nested {
                None
            } else {
                self.untyped_param_positions(Loc::new(e.file, n))
            };
            for cs in pt_calls {
                let n = self.store().sig(cs).params.len();
                for k in 0..n {
                    if untyped.as_ref().is_some_and(|u| !u.at(k)) {
                        continue;
                    }
                    let Some(ty) = self.param_type_at(cs, k) else {
                        continue;
                    };
                    for (j, &tp) in tps.iter().enumerate() {
                        if !mentioned[j] && self.type_mentions(ty, tp) {
                            mentioned[j] = true;
                        }
                    }
                }
            }
            ctx.fixing = true;
            for (j, &m) in mentioned.iter().enumerate() {
                if m {
                    // With nothing inferred from the arguments, a parameter fixes to what
                    // the call's contextual type implies (`const b: Box<number> =
                    // mk(r => r(1))`). A generic context counts with only the inferences
                    // that don't mention the outer call's open type parameters (`pipe(
                    // filter((v, i) => ..), ..)` against `Fn<T, A>` fixes `filter`'s `T`).
                    let implied = if rctx_generic {
                        rctx_inf.as_ref()
                    } else {
                        rctx.as_ref()
                    };
                    if ctx.fixed[j].is_none()
                        && ctx.candidates[j].is_empty()
                        && ctx.contra[j].is_empty()
                        && ctx.weak[j].is_empty()
                        && let Some(r) = implied
                        && !r.candidates[j].is_empty()
                        && r.candidates[j].iter().all(|&t| {
                            t != TypeId::UNRESOLVED
                                && !self.store().is_error(t)
                                && (!rctx_generic || !self.mentions_free_type_params(e, t))
                        })
                    {
                        ctx.candidates[j] = r.candidates[j].clone();
                    }
                    let _ = self.inferred_type_at(&mut ctx, j);
                }
            }
            ctx.fixing = false;
            // Nothing inferred yet: what the call's contextual type implies. A constrained
            // parameter without either maps to its constraint seen through those
            // (`O extends RO<A>` with `A` implied by the context).
            let mut seen = ctx.clone();
            let mut implied = vec![false; tps.len()];
            // An implication that mentions an outer call's type parameters still being
            // inferred only applies while this call has no inferences and no defaults
            // (`pipeThrough(new TransformStream({ transform(c, ctl) {} }))` leaves
            // `TransformStream<I = any, O = any>` at its defaults; `new Map(xs.map(x =>
            // [k, x]))` makes the array literal a tuple).
            let open_ok = rctx_generic
                && !(0..tps.len()).any(|j| {
                    ctx.fixed[j].is_some()
                        || !ctx.candidates[j].is_empty()
                        || !ctx.contra[j].is_empty()
                        || !ctx.weak[j].is_empty()
                        || rctx_inf
                            .as_ref()
                            .is_some_and(|r| !r.candidates[j].is_empty())
                })
                && !tps.iter().any(|&p| self.type_param_default(p).is_some());
            for (j, imp) in implied.iter_mut().enumerate() {
                if seen.fixed[j].is_none()
                    && seen.candidates[j].is_empty()
                    && seen.weak[j].is_empty()
                    && seen.contra[j].is_empty()
                {
                    let inferred: Vec<TypeId> = rctx_inf
                        .as_ref()
                        .map(|r| r.candidates[j].clone())
                        .unwrap_or_default();
                    let rc: Vec<TypeId> = if inferred.is_empty() {
                        rctx.as_ref()
                            .map(|r| r.candidates[j].clone())
                            .unwrap_or_default()
                    } else {
                        inferred
                    };
                    if !rc.is_empty()
                        && rc
                            .iter()
                            .all(|&t| t != TypeId::UNRESOLVED && !self.store().is_error(t))
                        && (!rctx_generic
                            || open_ok
                            || !rc.iter().any(|&t| self.mentions_free_type_params(e, t)))
                    {
                        seen.candidates[j] = rc;
                        *imp = true;
                    }
                }
            }
            for (j, &imp) in implied.iter().enumerate() {
                if imp {
                    let t = self.union(&seen.candidates[j]);
                    seen.fixed[j] = Some(t);
                }
            }
            let cur: Vec<TypeId> = (0..tps.len())
                .map(|j| {
                    if let Some(t) = ctx.fixed[j] {
                        t
                    } else if let Some(t) = seen.fixed[j].filter(|_| implied[j]) {
                        t
                    } else if ctx.candidates[j].is_empty()
                        && ctx.weak[j].is_empty()
                        && ctx.contra[j].is_empty()
                    {
                        // A parameter with a default is seen as its default (`new
                        // TransformStream({ transform(c, ctl) {} })` types `ctl` with `O = any`),
                        // but not in a callback's return type: `then(v => Promise.reject())`
                        // returns `Promise<never>`, not `Promise<T>` (`TResult1 = T`).
                        if implied.iter().any(|&x| x)
                            && self.type_param_constraint(tps[j]).is_some()
                            || (!callable && self.type_param_default(tps[j]).is_some())
                        {
                            let mut c2 = seen.clone();
                            self.inferred_type_at(&mut c2, j)
                        } else {
                            tps[j]
                        }
                    } else {
                        let mut c2 = seen.clone();
                        self.inferred_type_at(&mut c2, j)
                    }
                })
                .collect();
            let m = self.store().mapper(&tps, &cur);
            let ct = self.instantiate(pt, m);
            let at = self.speculative_arg_type(Loc::new(e.file, n), ct);
            self.infer_types(&mut ctx, at, pt, 0);
        }
        // With a generic context, only inferences that don't mention type parameters count
        // (`(a: Effect<boolean>) => B` still implies `A = boolean`).
        if let Some(r) = rctx_inf {
            for j in 0..ctx.params.len() {
                if ctx.fixed[j].is_none()
                    && ctx.candidates[j].is_empty()
                    && ctx.contra[j].is_empty()
                    && ctx.weak[j].is_empty()
                {
                    let mut cands: Vec<TypeId> = Vec::new();
                    // Lower-priority candidates count when there is nothing else
                    // (`const p: Promise<void> = q.then()` against `Promise<TResult1 |
                    // TResult2>` infers `void` for both).
                    let from = if r.candidates[j].is_empty() {
                        &r.weak[j]
                    } else {
                        &r.candidates[j]
                    };
                    for &t in from {
                        if t != TypeId::UNRESOLVED
                            && !self.store().is_error(t)
                            && (!rctx_generic || !self.mentions_free_type_params(e, t))
                        {
                            cands.push(t);
                        }
                    }
                    // An inference from the context that breaks the constraint gives way to
                    // the default (`R extends Date = D` against `D | number`).
                    let fits = cands.is_empty()
                        || match self.type_param_constraint(ctx.params[j]) {
                            Some(c) => {
                                let m = self.inference_mapper(&ctx, j);
                                let ic = self.instantiate(c, m);
                                let u = self.union(&cands);
                                let u = self.widened_type(u);
                                self.is_assignable(u, ic)
                            }
                            None => true,
                        };
                    if !cands.is_empty() && fits {
                        ctx.candidates[j] = cands;
                        ctx.top_level[j] = r.top_level[j];
                        ctx.nested[j] = r.nested[j];
                    }
                }
            }
        }
        (0..ctx.params.len())
            .map(|j| self.inferred_type_at(&mut ctx, j))
            .collect()
    }

    /// Whether `t` is an array whose elements are tuples.
    pub(crate) fn is_array_of_tuples(&self, t: TypeId) -> bool {
        self.c.is_array_type(t)
            && self.c.type_args_of(t).first().is_some_and(|&el| {
                self.every_member(el, |m| self.store().tuple_shape_of(m).is_some())
            })
    }

    /// The contextual call signatures of the context-sensitive functions in the array and
    /// object literal `l` (the elements and property values of `l` typed by `ct`); `false`
    /// when some element or property is not one of them (nor such a literal).
    fn nested_context_sigs(
        &mut self,
        l: Loc,
        ct: TypeId,
        depth: u32,
        out: &mut Vec<SigId>,
    ) -> bool {
        if depth > 8 || out.len() > 64 {
            return false;
        }
        let ast = self.ast(l.file);
        let n = crate::nodes::skip_parens(ast, l.node);
        match *ast.node(n) {
            Node::Arrow { .. } | Node::FunctionExpr { .. } => {
                let t = self.non_nullable(ct);
                out.extend(self.signatures_of(t, SigKind::Call));
            }
            Node::ArrayLit { elements } => {
                for (i, &x) in ast.list(elements).to_vec().iter().enumerate() {
                    let xl = Loc::new(l.file, x);
                    if matches!(ast.node(x), Node::Spread { .. })
                        || !self.is_context_sensitive_expr(xl)
                    {
                        return false;
                    }
                    if let Some(t) = self.contextual_element_type(ct, i, false, None)
                        && !self.nested_context_sigs(xl, t, depth + 1, out)
                    {
                        return false;
                    }
                }
            }
            Node::ObjectLit { props } => {
                for &p in ast.list(props).to_vec().iter() {
                    let (key, value) = match *ast.node(p) {
                        Node::Property { key, value, .. } | Node::MethodDef { key, value, .. } => {
                            (key, value)
                        }
                        _ => return false,
                    };
                    let vl = Loc::new(l.file, value);
                    if !self.is_context_sensitive_expr(vl) {
                        return false;
                    }
                    if let Some(t) = self.contextual_member_type(ct, Loc::new(l.file, key))
                        && !self.nested_context_sigs(vl, t, depth + 1, out)
                    {
                        return false;
                    }
                }
            }
            _ => return false,
        }
        true
    }

    /// `at` has a single generic call signature and the parameter type `pt` has a call
    /// signature whose parameters mention one of `tps`.
    fn is_generic_function_for(&mut self, at: TypeId, pt: TypeId, tps: &[TypeId]) -> bool {
        if !self.store().flags(at).contains(TypeFlags::OBJECT)
            || !self
                .store()
                .flags(pt)
                .intersects(TypeFlags::OBJECT | TypeFlags::UNION)
        {
            return false;
        }
        let ssigs = self.signatures_of(at, SigKind::Call);
        if ssigs.len() != 1 || self.store().sig(ssigs[0]).type_params == ListId::EMPTY {
            return false;
        }
        for cs in self.signatures_of(pt, SigKind::Call) {
            if self.store().sig(cs).type_params != ListId::EMPTY {
                continue;
            }
            let n = self.store().sig(cs).params.len();
            for k in 0..n {
                let Some(ty) = self.param_type_at(cs, k) else {
                    continue;
                };
                if tps.iter().any(|&tp| self.type_mentions(ty, tp)) {
                    return true;
                }
            }
        }
        false
    }

    /// Function expressions with unannotated parameters (directly or in object/array
    /// literals) are checked after other arguments.
    pub fn is_context_sensitive_expr(&self, l: Loc) -> bool {
        let ast = self.ast(l.file);
        let n = crate::nodes::skip_parens(ast, l.node);
        match *ast.node(n) {
            Node::FunctionExpr { .. } | Node::Arrow { .. } => {
                self.is_context_sensitive_function(Loc::new(l.file, n))
            }
            Node::ObjectLit { props } => ast.list(props).iter().any(|&p| match *ast.node(p) {
                Node::Property { value, .. } => {
                    self.is_context_sensitive_expr(Loc::new(l.file, value))
                }
                Node::MethodDef { value, .. } => {
                    self.is_context_sensitive_function(Loc::new(l.file, value))
                }
                _ => false,
            }),
            Node::ArrayLit { elements } => ast
                .list(elements)
                .iter()
                .any(|&x| self.is_context_sensitive_expr(Loc::new(l.file, x))),
            Node::Conditional { cons, alt, .. } => {
                self.is_context_sensitive_expr(Loc::new(l.file, cons))
                    || self.is_context_sensitive_expr(Loc::new(l.file, alt))
            }
            _ => false,
        }
    }

    /// Whether an argument is a call or `new` expression, or a conditional with one as an
    /// arm: its contextual type sees what the earlier arguments inferred.
    fn is_generic_call_arg(&self, l: Loc) -> bool {
        let ast = self.ast(l.file);
        let mut stack = vec![l.node];
        let mut steps = 0;
        while let Some(n) = stack.pop() {
            steps += 1;
            if steps > 1000 {
                return false;
            }
            match *ast.node(crate::nodes::skip_parens(ast, n)) {
                Node::Call { .. } | Node::New { .. } => return true,
                Node::Conditional { cons, alt, .. } => {
                    stack.push(cons);
                    stack.push(alt);
                }
                _ => {}
            }
        }
        false
    }

    /// For an array literal argument `l` of the array parameter type `pt`: the array of the
    /// union of the types of its elements that are not context sensitive (none if all are,
    /// or `l` has spreads).
    fn context_free_array_elements_type(&mut self, l: Loc, pt: TypeId) -> Option<TypeId> {
        let ast = self.ast(l.file);
        let Node::ArrayLit { elements } = *ast.node(crate::nodes::skip_parens(ast, l.node)) else {
            return None;
        };
        if !self.c.is_array_type(pt) {
            return None;
        }
        let et = *self.c.type_args_of(pt).first()?;
        let mut parts = Vec::new();
        for &x in ast.list(elements).to_vec().iter() {
            let xl = Loc::new(l.file, x);
            if matches!(self.ast(l.file).node(x), Node::Spread { .. }) {
                return None;
            }
            if self.is_context_sensitive_expr(xl) {
                continue;
            }
            parts.push(self.speculative_arg_type(xl, et));
        }
        if parts.is_empty() {
            return None;
        }
        let u = self.union(&parts);
        Some(self.create_array(u, false))
    }

    /// The arms of a (nested) conditional expression that are not context sensitive.
    fn context_free_conditional_arms(&self, l: Loc) -> Vec<Loc> {
        let mut out = Vec::new();
        let mut stack = vec![l.node];
        while let Some(n) = stack.pop() {
            let ast = self.ast(l.file);
            let n = crate::nodes::skip_parens(ast, n);
            match *ast.node(n) {
                Node::Conditional { cons, alt, .. } => {
                    stack.push(alt);
                    stack.push(cons);
                }
                _ if n != l.node && !self.is_context_sensitive_expr(Loc::new(l.file, n)) => {
                    out.push(Loc::new(l.file, n));
                }
                _ => {}
            }
        }
        out
    }

    /// Whether a function expression or arrow inside `l` (not `l` itself) has unannotated
    /// parameters.
    fn contains_context_sensitive_function(&self, l: Loc) -> bool {
        let ast = self.ast(l.file);
        let mut stack = ast.children(l.node);
        let mut steps = 0;
        while let Some(n) = stack.pop() {
            steps += 1;
            if steps > 100_000 {
                return true;
            }
            if matches!(ast.node(n), Node::Arrow { .. } | Node::FunctionExpr { .. })
                && self.is_context_sensitive_function(Loc::new(l.file, n))
            {
                return true;
            }
            ast.for_each_child(n, |c| stack.push(c));
        }
        false
    }

    /// Whether the type of `l` cannot depend on its contextual type (names, literals, member
    /// and element accesses, operators over such operands), so it can be checked once and
    /// cached instead of speculatively per candidate.
    fn is_context_free_expr(&self, l: Loc, depth: u32) -> bool {
        if depth > 32 {
            return false;
        }
        let ast = self.ast(l.file);
        let at = |n| Loc::new(l.file, n);
        match *ast.node(l.node) {
            Node::Ident { .. }
            | Node::This {}
            | Node::NullLit {}
            | Node::BoolLit { .. }
            | Node::NumberLit { .. }
            | Node::BigIntLit { .. }
            | Node::StringLit { .. } => true,
            Node::Paren { expr } | Node::NonNull { expr } => {
                self.is_context_free_expr(at(expr), depth + 1)
            }
            Node::Unary { arg, .. } => self.is_context_free_expr(at(arg), depth + 1),
            Node::Member { obj, .. } => self.is_context_free_expr(at(obj), depth + 1),
            Node::Index { obj, index, .. } => {
                self.is_context_free_expr(at(obj), depth + 1)
                    && self.is_context_free_expr(at(index), depth + 1)
            }
            Node::Binary { left, right, .. } => {
                self.is_context_free_expr(at(left), depth + 1)
                    && self.is_context_free_expr(at(right), depth + 1)
            }
            _ => false,
        }
    }

    /// The contextual type a function argument that is not context sensitive first had
    /// while the type arguments of `sig` (an instantiation) were inferred.
    pub(crate) fn first_context_of(&self, l: Loc, sig: SigId) -> Option<TypeId> {
        let (target, _) = self.store().sig(sig).target?;
        self.first_arg_context.get(&(l, target)).copied()
    }

    fn speculative_arg_type(&mut self, l: Loc, ctx: TypeId) -> TypeId {
        // A function argument fixed by an earlier overload keeps that context (`None`: no
        // context, so no memo either).
        let (ctx, memo) = match self.fixed_arg_contexts.get(&l) {
            Some(&Some(c)) => (c, true),
            Some(None) => (ctx, false),
            None => (ctx, true),
        };
        if self.is_context_free_expr(l, 0) {
            let t = self.quietly(|ck| ck.check_expr(l));
            // A generic reference with a union constraint passed to a non-generic
            // parameter reads (and narrows) through its constraint.
            let ast = self.ast(l.file);
            if matches!(ast.node(l.node), Node::Ident { .. })
                && self.has_union_constrained_member(t)
                && !self.is_generic_type(ctx)
            {
                return self.quietly(|ck| ck.check_expr_with_context(l, ctx));
            }
            return t;
        }
        let key = if memo {
            self.speculative_fn_key(l, ctx)
        } else {
            None
        };
        if let Some(k) = key
            && let Some(&t) = self.spec_fn_cache.get(&k)
        {
            return t;
        }
        let overflow = self.spec_overflow;
        self.enter_speculation();
        self.no_report += 1;
        let t = self.check_expr_with_context(l, ctx);
        self.no_report -= 1;
        self.leave_speculation();
        if let Some(k) = key
            && !self.spec_overflow
            && !overflow
        {
            self.spec_fn_cache.insert(k, t);
        }
        t
    }

    /// The memo key of a function expression argument checked speculatively against `ctx`:
    /// its type depends only on `ctx` and on the parameter types of the functions around
    /// it. Without the memo, nested callbacks of generic calls
    /// (`g((a) => g((b) => g((c) => ...))))`) are re-checked by every inference pass of every
    /// enclosing call: exponential in the nesting depth. `None` when the argument is not a
    /// function or an enclosing function's parameter has no fixed type yet.
    fn speculative_fn_key(&self, l: Loc, ctx: TypeId) -> Option<(Loc, TypeId, u64)> {
        use std::hash::{Hash, Hasher};
        let ast = self.ast(l.file);
        if !matches!(
            ast.node(crate::nodes::skip_parens(ast, l.node)),
            Node::Arrow { .. } | Node::FunctionExpr { .. }
        ) {
            return None;
        }
        let mut h = rustc_hash::FxHasher::default();
        let parents = self.c.file(l.file).parents();
        let mut cur = parents.parent(l.node);
        while let Some(n) = cur {
            let node = ast.node(n);
            if let Node::Arrow { .. } | Node::FunctionExpr { .. } = node {
                let f = Loc::new(l.file, n);
                if let Some(parts) = crate::nodes::fn_parts(ast, n) {
                    for &p in ast.list(parts.params) {
                        if matches!(*ast.node(p), Node::Param { type_ann: None, .. })
                            && !crate::nodes::is_this_param(ast, p)
                        {
                            let t = *self.param_types.get(&Loc::new(l.file, p))?;
                            (p, t).hash(&mut h);
                        }
                    }
                }
                // A function expression's `this` comes from its contextual type.
                if matches!(node, Node::FunctionExpr { .. }) {
                    let o = self.context_overrides.iter().rev().find(|x| x.0 == f);
                    o.map(|x| x.1).hash(&mut h);
                }
            }
            cur = parents.parent(n);
        }
        Some((l, ctx, h.finish()))
    }

    fn check_candidate_args(
        &mut self,
        e: Loc,
        sig: SigId,
        eff: &[Arg],
        rel: RelationKind,
    ) -> Option<Failure> {
        self.check_candidate_args_in(e, sig, eff, rel, ArgPass::Plain)
    }

    /// [`Self::check_candidate_args`] for an overloaded call with context-sensitive function
    /// arguments (`sens`, by position). `ArgPass::Skip` checks only the other arguments
    /// (a function argument only needs a parameter type that a function could fit);
    /// `ArgPass::Fix` checks all of them in order, fixing each function argument's context
    /// to this candidate's as it is reached unless an earlier candidate fixed it.
    fn check_candidate_args_in(
        &mut self,
        e: Loc,
        sig: SigId,
        eff: &[Arg],
        rel: RelationKind,
        pass: ArgPass<'_>,
    ) -> Option<Failure> {
        if let Some(f) = self.check_this_arg(e, sig, rel) {
            return Some(f);
        }
        let (count, has_rest) = self.param_count(sig);
        let min = self.store().sig(sig).min_args as usize;
        let rest_tuple = if has_rest && eff.len() > count {
            self.rest_tuple_contexts(sig, eff, count)
        } else {
            None
        };
        // A rest parameter whose type is neither an array nor a tuple (`never`, a union of
        // tuples, an array subtype) takes its arguments as one tuple.
        let spread_rest = if has_rest && rest_tuple.is_none() {
            self.non_array_rest_type(sig)
                .filter(|_| !eff.iter().skip(count).any(|a| matches!(a, Arg::Rest(..))))
        } else {
            None
        };
        let mut tuple_els: Vec<TypeId> = Vec::new();
        for (i, &a) in eff.iter().enumerate() {
            let rest_ctx = match (&rest_tuple, spread_rest) {
                (Some(r), _) if i >= count => Some(r.ctx[i - count]),
                // The element's contextual type is the union of each member's element,
                // without `unknown` absorbing the others: `K` from `[K, number] | [unknown,
                // string]` stays generic, so a `keyof T` argument keeps its type.
                (None, Some(rt)) if i >= count => {
                    let name = (i - count).to_string();
                    Some(
                        self.quietly(|ck| ck.contextual_property_type(rt, &name))
                            .unwrap_or(TypeId::ANY),
                    )
                }
                _ => None,
            };
            let tuple_ctx = rest_ctx.filter(|_| match &rest_tuple {
                Some(r) => i >= r.start,
                None => spread_rest.is_some(),
            });
            let Some(pt) = rest_ctx.or_else(|| self.param_type_at(sig, i)) else {
                if let Arg::Rest(n, _) = a {
                    return Some(Failure::Spread(n));
                }
                continue;
            };
            // A spread's elements meet an optional parameter with its `undefined`.
            let pt = if matches!(a, Arg::Rest(..)) && i >= min && i < count {
                self.add_optionality(pt)
            } else {
                pt
            };
            let sensitive = match pass {
                ArgPass::Plain => false,
                ArgPass::Skip(s) | ArgPass::Fix(s) => s.get(i).copied().unwrap_or(false),
            };
            if sensitive && let (ArgPass::Skip(_), Arg::Expr(n)) = (pass, a) {
                if self.function_may_fit(pt) {
                    if tuple_ctx.is_some() {
                        tuple_els.push(TypeId::ANY);
                    }
                    continue;
                }
                let at = self.speculative_arg_type(Loc::new(e.file, n), pt);
                return Some(Failure::Arg {
                    index: i,
                    arg_type: at,
                    param: pt,
                    tuple: false,
                });
            }
            // Without its nested context-sensitive functions (each `any`), as tsc's first
            // pass sees an object literal argument.
            let nested = match (pass, a) {
                (ArgPass::Skip(_), Arg::Expr(n)) => self.nested_fns_of(Loc::new(e.file, n)),
                _ => None,
            };
            let at = match (a, nested) {
                (Arg::Expr(n), Some(nested)) => {
                    let l = Loc::new(e.file, n);
                    let ctx = self.first_context_of(l, sig).unwrap_or(pt);
                    let saved =
                        std::mem::replace(&mut self.placeholder_fns, nested.into_iter().collect());
                    self.push_assumptions();
                    self.enter_speculation();
                    self.no_report += 1;
                    let t = self.check_expr_with_context(l, ctx);
                    self.no_report -= 1;
                    self.leave_speculation();
                    self.pop_assumptions();
                    self.placeholder_fns = saved;
                    t
                }
                (Arg::Expr(n), None) => {
                    let l = Loc::new(e.file, n);
                    let ctx = self.first_context_of(l, sig).unwrap_or(pt);
                    if sensitive && !self.fixed_arg_contexts.contains_key(&l) {
                        self.fixed_arg_contexts.insert(l, Some(ctx));
                    }
                    if !sensitive && matches!(pass, ArgPass::Fix(_)) {
                        self.fix_nested_contexts(l, ctx);
                    }
                    self.speculative_arg_type(l, ctx)
                }
                // A spread of unknown length into fixed parameters passed the arity check
                // (it starts past the required ones): its elements meet this parameter.
                (Arg::Elem(_, t) | Arg::Rest(_, t), _) => t,
            };
            if tuple_ctx.is_some() {
                let et = self.rest_arg_element_type(at, pt);
                tuple_els.push(et);
                continue;
            }
            if !self.is_type_related(at, pt, rel) {
                return Some(Failure::Arg {
                    index: i,
                    arg_type: at,
                    param: pt,
                    tuple: false,
                });
            }
            // Spreads into a rest parameter are each checked by position, like tsc.
            if let Arg::Rest(..) = a
                && !has_rest
            {
                break;
            }
        }
        let spread = rest_tuple
            .map(|r| (r.start, r.param))
            .or(spread_rest.map(|rt| (count, rt)));
        if let Some((start, param)) = spread {
            let els: Vec<crate::declared::TupleEl> = tuple_els
                .iter()
                .map(|&ty| crate::declared::TupleEl {
                    ty,
                    kind: ts_r_types::TupleElement::REQUIRED,
                    label: None,
                })
                .collect();
            let tt = self.create_tuple(&els, false);
            if !self.is_type_related(tt, param, rel) {
                return Some(Failure::Arg {
                    index: start,
                    arg_type: tt,
                    param,
                    tuple: true,
                });
            }
        }
        None
    }

    /// The type of `sig`'s rest parameter when it is not an array or tuple (nor `any`, an
    /// error, or still generic): such a rest parameter is related to its arguments as a
    /// tuple.
    fn non_array_rest_type(&self, sig: SigId) -> Option<TypeId> {
        let s = self.store();
        let d = s.sig(sig);
        if !d.has_rest() {
            return None;
        }
        let t = d.params.last()?.ty;
        if self.c.is_array_like_type(t)
            || s.is_error(t)
            || t == TypeId::UNRESOLVED
            || s.flags(t).intersects(TypeFlags::ANY | TypeFlags::INSTANTIABLE)
            // (Combined signatures' intersected tuples are taken by position, like tsc's
            // parameters of a union's call.)
            || s.flags(t).contains(TypeFlags::INTERSECTION)
        {
            return None;
        }
        Some(t)
    }

    /// For a rest parameter whose tuple type has a rest element (`[...string[], number]`,
    /// no generic variadic part) reached by plain arguments: the rest type and each
    /// argument's context (leading elements by position, trailing ones from the end, the
    /// rest element's type between).
    fn rest_tuple_contexts(&mut self, sig: SigId, eff: &[Arg], count: usize) -> Option<RestTuple> {
        let rt = self.store().sig(sig).params.last()?.ty;
        let shape = self.store().tuple_shape_of(rt)?.clone();
        let ri = shape
            .elements
            .iter()
            .position(|el| el.kind == ts_r_types::TupleElement::REST)?;
        if shape
            .elements
            .iter()
            .any(|el| el.kind == ts_r_types::TupleElement::VARIADIC)
            || eff[count..].iter().any(|a| matches!(a, Arg::Rest(..)))
        {
            return None;
        }
        let targs = self.c.type_args_of_tuple(rt).to_vec();
        let trail = shape.elements.len() - ri - 1;
        let m = eff.len() - count;
        if m < ri + trail {
            return None;
        }
        let mut ctx = Vec::with_capacity(m);
        for k in 0..m {
            let (idx, from_rest) = if k < ri {
                (k, false)
            } else if m - k <= trail {
                (ri + 1 + (k - (m - trail)), false)
            } else {
                (ri, true)
            };
            let t = targs.get(idx).copied().unwrap_or(TypeId::ANY);
            // (A rest element's type argument is its element type.)
            let t = if from_rest {
                t
            } else if shape.elements[idx].kind == ts_r_types::TupleElement::OPTIONAL {
                self.add_optionality(t)
            } else {
                t
            };
            ctx.push(t);
        }
        // Leading fixed elements are parameters of their own; the tuple is the rest.
        let param = if ri == 0 {
            rt
        } else {
            let mut els = Vec::with_capacity(shape.elements.len() - ri);
            for (j, el) in shape.elements.iter().enumerate().skip(ri) {
                let t = targs.get(j).copied().unwrap_or(TypeId::ANY);
                let ty = if el.kind == ts_r_types::TupleElement::REST {
                    self.create_array(t, false)
                } else {
                    t
                };
                els.push(crate::declared::TupleEl {
                    ty,
                    kind: el.kind,
                    label: el.label,
                });
            }
            self.create_tuple(&els, false)
        };
        Some(RestTuple {
            start: count + ri,
            param,
            ctx,
        })
    }

    /// An argument's type as an element of the tuple its rest arguments make: a literal stays
    /// one where the context has a primitive (or a type parameter), else it widens
    /// (observed: `["a", string]` for `"a", "b"` against `[...string[], () => void]`).
    fn rest_arg_element_type(&mut self, t: TypeId, ctx: TypeId) -> TypeId {
        let members = if self.store().flags(ctx).contains(TypeFlags::UNION) {
            self.store().members_of_union(ctx).to_vec()
        } else {
            vec![ctx]
        };
        let mut keeps = false;
        for m in members {
            let m = if self.store().flags(m).contains(TypeFlags::TYPE_PARAMETER) {
                self.base_constraint(m)
            } else {
                m
            };
            keeps |= self.some_member(m, |x| {
                self.store().flags(x).intersects(
                    TypeFlags::PRIMITIVE
                        | TypeFlags::INDEX
                        | TypeFlags::TEMPLATE_LITERAL
                        | TypeFlags::STRING_MAPPING,
                )
            });
        }
        if keeps {
            self.store().regular(t)
        } else {
            self.widen_literal(t)
        }
    }

    /// A call's `this` argument against `sig`'s declared `this` parameter (checked before
    /// the arguments): the receiver of a property or element access callee (`o.f()`,
    /// `(o.f)()`, without `null` and `undefined` after `?.`), else `void`, reported at the
    /// receiver or the whole call.
    fn check_this_arg(&mut self, e: Loc, sig: SigId, rel: RelationKind) -> Option<Failure> {
        let tt = self.store().sig(sig).this_type?;
        if tt == TypeId::VOID || !matches!(self.ast(e.file).node(e.node), Node::Call { .. }) {
            return None;
        }
        let (arg_type, at) = match self.call_receiver(e) {
            // (`super.m()` passes the current `this`; not modelled.)
            Some((recv, _)) if matches!(self.ast(e.file).node(recv.node), Node::Super {}) => {
                return None;
            }
            Some((recv, optional)) => {
                let rt = if self.in_optional_chain(recv) {
                    self.check_chain(recv).0
                } else {
                    self.check_expr(recv)
                };
                let rt = if optional {
                    self.non_nullable_strict(rt)
                } else {
                    rt
                };
                (rt, recv)
            }
            None => (TypeId::VOID, e),
        };
        if arg_type == TypeId::UNRESOLVED
            || self.store().is_error(arg_type)
            || self.is_type_related(arg_type, tt, rel)
        {
            return None;
        }
        Some(Failure::This {
            arg_type,
            this_type: tt,
            at,
        })
    }

    /// Whether a not-yet-checked function argument could fit parameter type `pt`: some
    /// member is `any`, `unknown`, a type parameter, `object`, `Function`, or an object
    /// type whose properties are all `length` or a class's `prototype` (signatures don't
    /// matter yet). Observed: an overload whose parameter there is `string`,
    /// `{ foo: any }`, `{ foo?: string }`, `{ prototype: {} }` or `BooleanConstructor` is
    /// passed over before the function is checked; `Function`, `T`, `typeof C`,
    /// `{ length: number }` or `(x: number) => void` checks it.
    fn function_may_fit(&mut self, pt: TypeId) -> bool {
        let function = self.global_type(GlobalType::Function);
        self.union_members(pt).into_iter().any(|m| {
            let f = self.store().flags(m);
            if f.intersects(
                TypeFlags::ANY_OR_UNKNOWN | TypeFlags::INSTANTIABLE | TypeFlags::NON_PRIMITIVE,
            ) || m == TypeId::UNRESOLVED
                || self.store().is_error(m)
                || Some(m) == function
            {
                return true;
            }
            if !f.intersects(TypeFlags::OBJECT | TypeFlags::INTERSECTION) {
                return false;
            }
            self.properties_of(m)
                .into_iter()
                .all(|(n, p)| self.store().str(n) == "length" || self.is_class_prototype(p))
        })
    }

    /// `elaborate_literal_error` for an argument, contextually typed by the parameter it
    /// failed against (the call is still being resolved, so the nested values it re-checks
    /// would otherwise lose their context: `{ a: () => "bar" }` against `{ a: () => "foo" }`
    /// reports `"bar"`, not `string`).
    fn elaborate_argument(&mut self, x: Loc, arg_type: TypeId, param: TypeId) -> bool {
        self.context_overrides.push((x, param));
        let r = self.elaborate_literal_error(x, arg_type, param);
        self.context_overrides.pop();
        r
    }

    fn report_candidate_failure(&mut self, e: Loc, eff: &[Arg], f: Failure) {
        match f {
            Failure::Spread(n) => {
                self.error(Loc::new(e.file, n), dg::A_SPREAD_ARGUMENT_MUST_EITHER_HAVE_A_TUPLE_TYPE_OR_BE_PASSED_TO_A_REST_PARAMETER_2556, &[]);
            }
            Failure::This {
                arg_type,
                this_type,
                at,
            } => {
                let (chain, _) = self.this_arg_error_chain(arg_type, this_type);
                self.error_chain(at, &chain);
            }
            Failure::Arg {
                index,
                arg_type,
                param,
                tuple,
            } => {
                // (A tuple of no arguments is reported at the call.)
                let mut node = eff.get(index).map_or(e, |a| Loc::new(e.file, a.node()));
                let expr = match eff.get(index) {
                    Some(&Arg::Expr(n)) if !tuple => {
                        let n = crate::nodes::effective_check_node(self.ast(e.file), n);
                        node = Loc::new(e.file, n);
                        Some(node)
                    }
                    _ => None,
                };
                if let Some(x) = expr
                    && self.elaborate_argument(x, arg_type, param)
                {
                    return;
                }
                let (chain, at) = self.relation_error_chain(
                    arg_type,
                    param,
                    RelationKind::Assignable,
                    Some(Head {
                        info: dg::ARGUMENT_OF_TYPE_0_IS_NOT_ASSIGNABLE_TO_PARAMETER_OF_TYPE_1_2345,
                    }),
                );
                let mark = self.diags.len();
                self.error_chain(at.unwrap_or(node), &chain);
                if let Some(x) = expr {
                    let x = Loc::new(x.file, crate::nodes::skip_parens(self.ast(x.file), x.node));
                    self.relate_call_hint(mark, x, arg_type, param);
                }
            }
        }
    }

    /// TS2684 with the relation's elaboration.
    fn this_arg_error_chain(
        &mut self,
        arg_type: TypeId,
        this_type: TypeId,
    ) -> (Chain, Option<Loc>) {
        self.relation_error_chain(
            arg_type,
            this_type,
            RelationKind::Assignable,
            Some(Head {
                info: dg::THE_THIS_CONTEXT_OF_TYPE_0_IS_NOT_ASSIGNABLE_TO_METHOD_S_THIS_OF_TYPE_1_2684,
            }),
        )
    }

    /// TS2769. tsc 7 reports only the last overload's error, elaborated like a single
    /// signature's (into object and array literal arguments), under "No overload matches
    /// this call. / The last overload gave the following error.".
    fn report_no_overload(
        &mut self,
        e: Loc,
        cl: Loc,
        eff: &[Arg],
        failures: Vec<(SigId, Failure)>,
    ) -> Option<SigId> {
        let best = failures.last().map(|x| x.0);
        let Some((_, f)) = failures.into_iter().last() else {
            let mut chain = Chain::new();
            chain.push_back(dg::NO_OVERLOAD_MATCHES_THIS_CALL_2769, &[]);
            let span = self.call_error_span(e, cl);
            self.push_nested_chain(e.file, span, &chain);
            return best;
        };
        let mut hint = None;
        let (inner, at) = match f {
            Failure::Spread(n) => {
                let mut c = Chain::new();
                c.push_back(dg::A_SPREAD_ARGUMENT_MUST_EITHER_HAVE_A_TUPLE_TYPE_OR_BE_PASSED_TO_A_REST_PARAMETER_2556, &[]);
                (c, Loc::new(e.file, n))
            }
            Failure::This {
                arg_type,
                this_type,
                at,
            } => (self.this_arg_error_chain(arg_type, this_type).0, at),
            Failure::Arg {
                index,
                arg_type,
                param,
                tuple,
            } => {
                let mut node = eff.get(index).map_or(e, |a| Loc::new(e.file, a.node()));
                if let Some(&Arg::Expr(n)) = eff.get(index)
                    && !tuple
                {
                    node = Loc::new(
                        e.file,
                        crate::nodes::effective_check_node(self.ast(e.file), n),
                    );
                }
                if let Some(Arg::Expr(_)) = eff.get(index)
                    && !tuple
                    && self.reporting(e.file)
                {
                    let n = node.node;
                    let mark = self.diags.len();
                    if self.elaborate_argument(Loc::new(e.file, n), arg_type, param) {
                        // Re-head what the elaboration reported.
                        for d in &mut self.diags[mark..] {
                            d.message = wrap_last_overload(&d.message);
                            d.code = 2769;
                            d.category = ts_r_diagnostics::Category::Error;
                        }
                        if let Some(b) = best {
                            self.relate_last_overload(mark, b);
                        }
                        return best;
                    }
                    self.diags.truncate(mark);
                }
                let (c, at) = self.relation_error_chain(
                    arg_type,
                    param,
                    RelationKind::Assignable,
                    Some(Head {
                        info: dg::ARGUMENT_OF_TYPE_0_IS_NOT_ASSIGNABLE_TO_PARAMETER_OF_TYPE_1_2345,
                    }),
                );
                if let Some(&Arg::Expr(n)) = eff.get(index)
                    && !tuple
                {
                    let x = crate::nodes::skip_parens(self.ast(e.file), n);
                    hint = Some((Loc::new(e.file, x), arg_type, param));
                }
                (c, at.unwrap_or(node))
            }
        };
        let mut chain = Chain::new();
        chain.push_back(dg::NO_OVERLOAD_MATCHES_THIS_CALL_2769, &[]);
        chain.nested.push((chain.items.len(), 1));
        chain.push_back(dg::THE_LAST_OVERLOAD_GAVE_THE_FOLLOWING_ERROR_2770, &[]);
        push_nested(&mut chain, &inner, 2);
        let mark = self.diags.len();
        self.error_chain_nested(at, &chain);
        if let Some((x, arg_type, param)) = hint {
            self.relate_call_hint(mark, x, arg_type, param);
        }
        if let Some(b) = best {
            self.relate_last_overload(mark, b);
        }
        best
    }

    /// "The last overload is declared here." at signature `sig`'s declaration (a function,
    /// method or accessor at its name; a signature member as a whole), for the diagnostic
    /// reported since `mark`.
    pub(crate) fn relate_last_overload(&mut self, mark: usize, sig: SigId) {
        let Some((f, n)) = self.store().sig(sig).decl else {
            return;
        };
        let Some(node) = NodeId::from_raw(n) else {
            return;
        };
        let at = Loc::new(ts_r_diagnostics::FileId(f), node);
        self.relate_last(mark, at, dg::THE_LAST_OVERLOAD_IS_DECLARED_HERE_2771, &[]);
    }

    /// For a function expression or arrow argument in a TypeScript file, which parameter
    /// positions have no type annotation (and so take their contextual type). `None` for
    /// other arguments.
    fn untyped_param_positions(&self, arg: Loc) -> Option<UntypedParams> {
        if self.c.file(arg.file).is_js() {
            return None;
        }
        let ast = self.ast(arg.file);
        let fun = crate::nodes::skip_parens(ast, arg.node);
        if !matches!(
            ast.node(fun),
            Node::Arrow { .. } | Node::FunctionExpr { .. }
        ) {
            return None;
        }
        let parts = crate::nodes::fn_parts(ast, fun)?;
        let mut fixed = Vec::new();
        let mut rest = None;
        for &p in ast.list(parts.params) {
            let Node::Param {
                rest: is_rest,
                pattern,
                type_ann,
                ..
            } = *ast.node(p)
            else {
                return None;
            };
            if fixed.is_empty()
                && rest.is_none()
                && matches!(*ast.node(pattern), Node::Ident { name } if ast.atom(name) == "this")
            {
                continue;
            }
            if is_rest {
                rest = Some(type_ann.is_none());
                break;
            }
            fixed.push(type_ann.is_none());
        }
        Some(UntypedParams { fixed, rest })
    }
}

/// Parameter positions of a function argument without type annotations.
struct UntypedParams {
    fixed: Vec<bool>,
    rest: Option<bool>,
}

impl UntypedParams {
    fn at(&self, k: usize) -> bool {
        match self.fixed.get(k) {
            Some(&u) => u,
            None => self.rest.unwrap_or(false),
        }
    }
}

/// `message` (a rendered chain) under the two TS2769 head lines.
pub(crate) fn wrap_last_overload(message: &str) -> String {
    let mut out = String::from(
        "No overload matches this call.\n  The last overload gave the following error.",
    );
    for line in message.lines() {
        out.push_str("\n    ");
        out.push_str(line);
    }
    out
}

/// Appends `inner`'s items to `chain` as nested items (`level` = indentation of the first).
fn push_nested(chain: &mut Chain, inner: &Chain, level: usize) {
    for (i, (code, text)) in inner.items.iter().enumerate() {
        let l = inner.nested.iter().find(|x| x.0 == i).map_or(i, |x| x.1);
        chain.nested.push((chain.items.len(), level + l));
        chain.items.push((*code, text.clone()));
    }
}

impl<'c, 'a> Ck<'c, 'a> {
    /// Reports a chain whose items carry explicit indentation levels.
    pub fn error_chain_nested(&mut self, l: Loc, chain: &Chain) {
        let span = self.error_span_of(l);
        self.push_nested_chain(l.file, span, chain);
    }

    pub fn push_nested_chain(&mut self, file: ts_r_diagnostics::FileId, span: Span, chain: &Chain) {
        self.push_chain(file, span, chain);
    }
}
