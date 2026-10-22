//! JSX elements and fragments: the element's `JSX` namespace, its tag, the attributes type
//! its attributes are checked against, children, and the element's type. Observed from
//! tsc 7 (probes and the `jsx` conformance baselines):
//!
//! - **Namespace.** Under `jsx: react` / `preserve` the namespace is the `JSX` export of
//!   the factory's root (`React`, `jsxFactory`, a `@jsx` pragma) in scope, else the global
//!   `JSX`. Under `react-jsx` / `react-jsxdev` it is the `JSX` export of the runtime module
//!   (`jsx_runtime.rs`), else the global `JSX`; a `JSX` namespace local to a module is
//!   never used.
//! - **Factory.** Under `jsx: react` each opening tag (at its name) and fragment (at `<>`)
//!   needs the factory root in scope (TS2874); a fragment also needs the fragment factory's
//!   (TS2879). With an `@jsx` pragma a fragment needs an `@jsxFrag` pragma (else React's
//!   factories in scope and TS17017) whose root is in scope (`null` needs nothing). Under
//!   the automatic runtime a runtime module that cannot be found is reported once per
//!   file, at its first JSX element (TS2875), and elements have the error type.
//! - **Intrinsic tags** (lowercase, dashed or namespaced) look up their props in
//!   `JSX.IntrinsicElements` (a property, else its string index). Without that interface
//!   noImplicitAny reports TS7026 at the opening and closing tags; an unknown tag is TS2339
//!   on `'JSX.IntrinsicElements'` (always printed so) at both tags. The props are used as
//!   they are (no `IntrinsicAttributes`).
//! - **Value tags** are expressions (closing tags are checked too: TS2304 twice). A tag with
//!   call signatures is a function component: its props are its first parameter (`{}`
//!   without one). Else construct signatures make a class component: with
//!   `JSX.ElementAttributesProperty` declared, its single property names the instance
//!   property holding the props (no property: the instance type itself; a missing instance
//!   property is TS2607 when there are attributes), else the first constructor parameter.
//!   Neither kind is TS2604. Props go through `JSX.LibraryManagedAttributes<typeof Tag, P>`
//!   and are intersected with `IntrinsicAttributes` (and `IntrinsicClassAttributes<Inst>`
//!   for classes) in that order, when declared.
//! - **Overloads**: the first signature whose props the attributes satisfy (a subtype pass,
//!   then an assignability pass, as for calls); when none does, the last one's errors are
//!   reported under TS2769 "The last overload gave the following error.".
//! - **Generic components** infer their type arguments from the attributes that are not
//!   context sensitive; explicit type arguments of the wrong count are TS2558.
//! - **Children** are an attribute when `JSX.ElementChildrenAttribute` names one (always
//!   `children` under the automatic runtime): one child is its type, several an array of
//!   their union. Whitespace-only text containing a line break is not a child; text is
//!   `string`. An explicit attribute of that name next to children is TS2710. A spread
//!   child (`{...x}`) whose type is not an array (only `any` is exempt, not the error
//!   type) is TS2609 at its `{`.
//! - **Validity** (TS2786 at the tag name): with `JSX.ElementType` the tag's type must be
//!   assignable to it; else a function component's return type to `JSX.Element | null`
//!   and a class component's instance type to `JSX.ElementClass`.
//! - The element's type is `JSX.Element` (the error type without it).

use rustc_hash::FxHashMap;
use ts_r_ast::{Node, NodeId, NodeList, Span};
use ts_r_binder::meaning;
use ts_r_config::JsxEmit;
use ts_r_diagnostics::FileId;
use ts_r_types::{ListId, RelationKind, SigId, StrId, SymId, TypeFlags, TypeId};

use crate::checker::{Ck, Loc, Resolving};
use crate::diag::{Chain, dg};
use crate::members::SigKind;

/// What a JSX element's attributes are checked against.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct JsxTarget {
    /// The type the attributes must be assignable to (`None`: not checked).
    pub props: Option<TypeId>,
    /// The children prop, when children are an attribute.
    pub children: Option<StrId>,
}

/// The resolution of a JSX opening element.
#[derive(Clone, Debug, Default)]
pub(crate) struct JsxResolved {
    pub target: JsxTarget,
    /// The chosen (instantiated) signature of a value tag and whether it is a construct
    /// signature.
    pub sig: Option<(SigId, bool)>,
    /// The tag's type (value tags).
    pub tag_type: Option<TypeId>,
    /// Several candidates and none matched: errors are reported under TS2769.
    pub no_overload: bool,
}

/// Per-file facts about JSX.
#[derive(Clone, Debug, Default)]
pub(crate) struct JsxFileInfo {
    /// The file uses the automatic runtime (`Some(dev)`).
    pub automatic: Option<bool>,
    /// The automatic runtime module's symbol.
    pub runtime: Option<SymId>,
    /// The runtime specifier when it could not be found.
    pub missing_runtime: Option<String>,
    /// The runtime resolved to JavaScript outside the program: `(specifier, file, in a
    /// package with a package id)` for TS7016.
    pub untyped_runtime: Option<(String, String, bool)>,
    /// The first JSX element or fragment in source order.
    pub first: Option<NodeId>,
}

/// The JSX state of a worker.
#[derive(Default)]
pub(crate) struct JsxState {
    pub files: FxHashMap<FileId, JsxFileInfo>,
    pub resolved: FxHashMap<Loc, JsxResolved>,
    /// Props types assumed for openings while their candidates are tried (innermost last).
    pub assumed: Vec<(Loc, TypeId)>,
}

impl<'c, 'a> Ck<'c, 'a> {
    // -----------------------------------------------------------------------------
    // The namespace
    // -----------------------------------------------------------------------------

    fn jsx_file_info(&mut self, file: FileId) -> JsxFileInfo {
        if let Some(i) = self.jsx.files.get(&file) {
            return i.clone();
        }
        let sf = self.c.file(file);
        let ast = &sf.ast;
        let mut info = JsxFileInfo {
            first: (0..ast.node_count() as u32)
                .filter_map(NodeId::from_raw)
                .filter(|&n| {
                    matches!(
                        ast.node(n),
                        Node::JsxElement { .. } | Node::JsxFragment { .. }
                    )
                })
                .min_by_key(|&n| ast.span(n).start),
            ..JsxFileInfo::default()
        };
        info.automatic = crate::jsx_runtime::file_runtime(self.c.opts.jsx, ast, &sf.text);
        if let Some(dev) = info.automatic {
            let source = crate::jsx_runtime::import_source_pragma(ast, &sf.text)
                .unwrap_or(&self.c.opts.jsx_import_source);
            let spec = crate::jsx_runtime::runtime_module_name(source, dev);
            match self.jsx_find_module(file, &spec) {
                Some(found) => {
                    info.runtime = found;
                    let path = &self.c.file(file).path;
                    if found.is_none()
                        && let Some(Some(r)) = self.c.resolutions.get(&(path.clone(), spec.clone()))
                        && !r.included
                        && matches!(
                            r.extension,
                            ts_r_resolver::Extension::Js
                                | ts_r_resolver::Extension::Jsx
                                | ts_r_resolver::Extension::Mjs
                                | ts_r_resolver::Extension::Cjs
                        )
                    {
                        info.untyped_runtime = Some((spec, r.resolved.clone(), r.package));
                    }
                }
                None => info.missing_runtime = Some(spec),
            }
        }
        self.jsx.files.insert(file, info.clone());
        info
    }

    /// The module `spec` imported from `file`: `Some(None)` when it exists but has no
    /// module symbol, `None` when it cannot be found.
    fn jsx_find_module(&self, file: FileId, spec: &str) -> Option<Option<SymId>> {
        use ts_r_binder::program::ModuleTarget;
        if let Some(i) = self.c.link.find_ambient_module(spec) {
            return Some(self.c.module_target_symbol(ModuleTarget::Ambient(i)));
        }
        let path = &self.c.file(file).path;
        let r = self
            .c
            .resolutions
            .get(&(path.clone(), spec.to_owned()))?
            .as_ref()?;
        let target = self
            .c
            .files
            .iter()
            .position(|f| f.path == r.resolved)
            .map(|i| ModuleTarget::File(FileId(i as u32)));
        Some(target.and_then(|t| self.c.module_target_symbol(t)))
    }

    /// The `JSX` namespace that types the JSX at `l` (module doc).
    pub(crate) fn jsx_ns(&mut self, l: Loc) -> Option<SymId> {
        let info = self.jsx_file_info(l.file);
        let ns_of = |ck: &Self, s: SymId| {
            ck.c.resolve_alias(s)
                .and_then(|s| ck.c.export_of(s, "JSX"))
                .and_then(|s| ck.c.resolve_alias(s))
                .filter(|&s| ck.c.has_meaning(s, meaning::NAMESPACE))
        };
        let local = if info.automatic.is_some() {
            info.runtime.and_then(|m| ns_of(self, m))
        } else {
            let root = self.jsx_factory_root(l.file);
            self.jsx_lookup(l, &root).and_then(|s| ns_of(self, s))
        };
        local.or_else(|| {
            self.c
                .global_symbol("JSX", meaning::ALL)
                .and_then(|s| self.c.resolve_alias(s))
        })
    }

    /// The root of the file's JSX factory: a `@jsx` pragma's, else `jsxFactory`'s or
    /// `reactNamespace`'s, else `React`.
    fn jsx_factory_root(&self, file: FileId) -> String {
        let sf = self.c.file(file);
        match crate::jsx_runtime::leading_pragma(&sf.ast, &sf.text, "@jsx") {
            Some(p) => p.split('.').next().unwrap_or(p).to_owned(),
            None => self.c.opts.jsx_factory_roots[0].clone(),
        }
    }

    /// A name in scope at `l` (as the JSX factory is looked up).
    fn jsx_lookup(&self, l: Loc, name: &str) -> Option<SymId> {
        let sf = self.c.file(l.file);
        self.c
            .lookup_in_scope_chain(l, name, meaning::ALL)
            .or_else(|| {
                sf.bound
                    .is_module
                    .then(|| self.c.sym(l.file, sf.bound.file_symbol))
                    .and_then(|m| self.c.export_of(m, name))
            })
            .or_else(|| self.c.lookup_in_merged_namespaces(l, name, meaning::ALL))
            .or_else(|| self.c.global_symbol(name, meaning::ALL))
    }

    /// A type-meaning member of the JSX namespace.
    fn jsx_ns_symbol(&self, ns: Option<SymId>, name: &str) -> Option<SymId> {
        let s = self.c.export_of(ns?, name)?;
        let s = self.c.resolve_alias(s)?;
        self.c.has_meaning(s, meaning::TYPE).then_some(s)
    }

    /// The (non-generic) type `JSX.<name>`.
    fn jsx_ns_type(&mut self, ns: Option<SymId>, name: &str) -> Option<TypeId> {
        let s = self.jsx_ns_symbol(ns, name)?;
        // A generic member (`type ElementType<P = any>`) is referenced with its defaults.
        let t = if self.local_type_params(s).is_empty() {
            self.declared_type_of_symbol(s)
        } else {
            self.type_from_symbol_reference(s, &[])
        };
        (t != TypeId::UNRESOLVED && !self.store().is_error(t)).then_some(t)
    }

    /// The children prop name (module doc).
    fn jsx_children_prop(&mut self, file: FileId, ns: Option<SymId>) -> Option<StrId> {
        if self.jsx_file_info(file).automatic.is_some() {
            return Some(self.store().intern_str("children"));
        }
        let eca = self.jsx_ns_type(ns, "ElementChildrenAttribute")?;
        match self.properties_of(eca)[..] {
            [(n, _)] => Some(n),
            _ => None,
        }
    }

    /// The type of a JSX element or fragment (module doc).
    fn jsx_element_type(&mut self, e: Loc) -> TypeId {
        let ns = self.jsx_ns(e);
        if let Some(t) = self.jsx_ns_type(ns, "Element") {
            return t;
        }
        let info = self.jsx_file_info(e.file);
        // A runtime that could not be found (TS2875) leaves the error type, as no namespace.
        let missing = ns.is_none() && info.missing_runtime.is_some();
        if (info.automatic.is_some() && !missing) || ns.is_some() {
            TypeId::UNRESOLVED
        } else {
            TypeId::ERROR
        }
    }

    // -----------------------------------------------------------------------------
    // Tags
    // -----------------------------------------------------------------------------

    /// The source text of a tag name (`a.b`, `a:b`).
    pub(crate) fn jsx_tag_text(&self, file: FileId, name: NodeId) -> String {
        let sf = self.c.file(file);
        let s = sf.ast.span(name);
        sf.text
            .get(s.start as usize..s.end as usize)
            .unwrap_or("")
            .to_owned()
    }

    /// The props of intrinsic tag `tag`: `Err(true)` without `JSX.IntrinsicElements`,
    /// `Err(false)` when the tag is not one of them.
    fn jsx_intrinsic_props(&mut self, ns: Option<SymId>, tag: &str) -> Result<TypeId, bool> {
        let Some(ie) = self.jsx_ns_type(ns, "IntrinsicElements") else {
            return Err(true);
        };
        if self.store().flags(ie).intersects(TypeFlags::ANY) {
            return Ok(TypeId::ANY);
        }
        if let Some(p) = self.property_of_type(ie, tag) {
            return Ok(self.type_of_symbol(p));
        }
        match self.index_info_of(ie, TypeId::STRING) {
            Some(i) => Ok(i.value),
            None => Err(false),
        }
    }

    /// Reports TS7026 / TS2339 for an intrinsic opening or closing tag at `at`.
    fn report_intrinsic_tag(&mut self, ns: Option<SymId>, tag: &str, at: Loc) {
        match self.jsx_intrinsic_props(ns, tag) {
            Ok(_) => {}
            Err(true) => {
                if self.c.opts.no_implicit_any {
                    self.error(
                        at,
                        dg::JSX_ELEMENT_IMPLICITLY_HAS_TYPE_ANY_BECAUSE_NO_INTERFACE_JSX_0_EXISTS_7026,
                        &["IntrinsicElements"],
                    );
                }
            }
            Err(false) => self.error(
                at,
                dg::PROPERTY_0_DOES_NOT_EXIST_ON_TYPE_1_2339,
                &[tag, "JSX.IntrinsicElements"],
            ),
        }
    }

    /// The type of a value tag expression.
    fn jsx_tag_type(&mut self, file: FileId, name: NodeId) -> TypeId {
        let ast = self.ast(file);
        if let Node::Ident { name: n } = *ast.node(name)
            && ast.atom(n) == "this"
        {
            return self.check_this(Loc::new(file, name));
        }
        self.check_expr(Loc::new(file, name))
    }

    /// Whether a type is unusable as a tag or props (not checked).
    fn jsx_unusable(&mut self, t: TypeId) -> bool {
        t == TypeId::UNRESOLVED
            || self.store().is_error(t)
            || self.store().flags(t).intersects(TypeFlags::ANY)
    }

    // -----------------------------------------------------------------------------
    // Resolution
    // -----------------------------------------------------------------------------

    /// The resolution of `opening` (computed quietly; cached outside speculation).
    pub(crate) fn jsx_resolved(&mut self, opening: Loc) -> JsxResolved {
        if let Some(r) = self.jsx.resolved.get(&opening) {
            return r.clone();
        }
        if !self.push_resolution(Resolving::Node(opening)) {
            return JsxResolved::default();
        }
        let r = self.quietly(|ck| ck.resolve_jsx(opening));
        let circular = !self.pop_resolution();
        if self.speculative == 0 && !circular {
            self.jsx.resolved.insert(opening, r.clone());
        }
        r
    }

    /// The target of `opening`, with the props assumed for it while candidates are tried.
    pub(crate) fn jsx_target(&mut self, opening: Loc) -> JsxTarget {
        if let Some(&(_, t)) = self.jsx.assumed.iter().rev().find(|(l, _)| *l == opening) {
            let ns = self.jsx_ns(opening);
            return JsxTarget {
                props: Some(t),
                children: self.jsx_children_prop(opening.file, ns),
            };
        }
        if self
            .resolving
            .iter()
            .any(|(r, _)| *r == Resolving::Node(opening))
        {
            return JsxTarget::default();
        }
        self.jsx_resolved(opening).target
    }

    fn resolve_jsx(&mut self, opening: Loc) -> JsxResolved {
        let file = opening.file;
        let ast = self.ast(file);
        let Node::JsxOpening {
            name, type_args, ..
        } = *ast.node(opening.node)
        else {
            return JsxResolved::default();
        };
        let ns = self.jsx_ns(opening);
        let mut r = JsxResolved {
            target: JsxTarget {
                props: None,
                children: self.jsx_children_prop(opening.file, ns),
            },
            ..JsxResolved::default()
        };
        if self.c.file(file).is_js() {
            return r;
        }
        if crate::check::is_intrinsic_tag(ast, name) {
            let tag = self.jsx_tag_text(file, name);
            r.target.props = self.jsx_intrinsic_props(ns, &tag).ok();
            return r;
        }
        let tt = self.jsx_tag_type(file, name);
        if self.jsx_unusable(tt) {
            return r;
        }
        if let Some(text) = self.literal_text(tt)
            && self.store().flags(tt).intersects(TypeFlags::STRING_LITERAL)
        {
            r.target.props = self.jsx_intrinsic_props(ns, &text).ok();
            return r;
        }
        let mut sigs = self.signatures_of(tt, SigKind::Call);
        let mut construct = false;
        if sigs.is_empty() {
            sigs = self.signatures_of(tt, SigKind::Construct);
            construct = true;
        }
        if sigs.is_empty() {
            return r;
        }
        r.tag_type = Some(tt);
        let explicit = match type_args {
            Some(ta) => {
                let Node::TypeArgs { args } = *ast.node(ta) else {
                    return r;
                };
                let list = ast.list(args).to_vec();
                Some(
                    list.into_iter()
                        .map(|a| self.type_from_node(Loc::new(file, a)))
                        .collect::<Vec<_>>(),
                )
            }
            None => None,
        };
        // Candidates: explicit type arguments must fit the type parameter count.
        let candidates: Vec<SigId> = sigs
            .iter()
            .copied()
            .filter(|&s| {
                explicit.as_ref().is_none_or(|ex| {
                    let tps = self
                        .store()
                        .list_items(self.store().sig(s).type_params)
                        .to_vec();
                    self.type_arg_count_fits(&tps, ex.len()) && !tps.is_empty()
                })
            })
            .collect();
        if candidates.is_empty() {
            return r;
        }
        if let [s] = candidates[..] {
            let Some(inst) = self.jsx_instantiate(opening, s, construct, explicit.as_deref())
            else {
                return r;
            };
            r.sig = Some((inst, construct));
            r.target.props = self.jsx_sig_props(ns, tt, inst, construct);
            return r;
        }
        let relations = [RelationKind::Subtype, RelationKind::Assignable];
        let mut last = None;
        for rel in relations {
            for &cand in &candidates {
                let Some(inst) =
                    self.jsx_instantiate(opening, cand, construct, explicit.as_deref())
                else {
                    return r;
                };
                let Some(props) = self.jsx_sig_props(ns, tt, inst, construct) else {
                    // A candidate we cannot model: give up on the element.
                    return r;
                };
                last = Some((inst, props));
                let attrs = self.jsx_attributes_under(opening, props, r.target.children);
                let Some(attrs) = attrs else {
                    return r;
                };
                if self.is_type_related(attrs, props, rel) {
                    r.sig = Some((inst, construct));
                    r.target.props = Some(props);
                    return r;
                }
            }
        }
        if let Some((inst, props)) = last {
            r.sig = Some((inst, construct));
            r.target.props = Some(props);
            r.no_overload = true;
        }
        r
    }

    /// The attributes type of `opening` with `props` assumed for it (speculatively).
    fn jsx_attributes_under(
        &mut self,
        opening: Loc,
        props: TypeId,
        children: Option<StrId>,
    ) -> Option<TypeId> {
        self.jsx.assumed.push((opening, props));
        self.enter_speculation();
        let t = self.jsx_attributes_type(opening, children, false);
        self.leave_speculation();
        self.jsx.assumed.pop();
        t
    }

    /// Instantiates a generic candidate with explicit type arguments or ones inferred from
    /// the attributes that are not context sensitive. `None` when the inference is not
    /// modelled: props that are not plain object types (indexed access, conditional types)
    /// or a type parameter only context-sensitive values could infer.
    fn jsx_instantiate(
        &mut self,
        opening: Loc,
        sig: SigId,
        construct: bool,
        explicit: Option<&[TypeId]>,
    ) -> Option<SigId> {
        let d = self.store().sig(sig);
        if d.type_params == ListId::EMPTY {
            return Some(sig);
        }
        let tps = self.store().list_items(d.type_params).to_vec();
        let args = match explicit {
            Some(ex) => self.fill_type_args(&tps, ex),
            None => {
                let ns = self.jsx_ns(opening);
                let pt = self.jsx_raw_props(ns, sig, construct)?;
                if !self.jsx_inferable(pt, 0) {
                    return None;
                }
                let mut ctx = self.inference_context_for(sig);
                let children = self.jsx_children_prop(opening.file, ns);
                self.enter_speculation();
                let attrs = self.jsx_attributes_type_inner(opening, children, false, true);
                self.leave_speculation();
                let attrs = attrs?;
                self.infer_types(&mut ctx, attrs, pt, 0);
                // A type parameter that only context-sensitive values infer, or one inferred
                // from the whole attributes object, needs tsc's second inference pass.
                if self.jsx_has_context_sensitive(opening, children)
                    && (self.jsx_naked_type_param(pt, 0)
                        || (0..tps.len())
                            .any(|i| ctx.candidates[i].is_empty() && ctx.contra[i].is_empty()))
                {
                    return None;
                }
                (0..tps.len())
                    .map(|i| self.inferred_type_at(&mut ctx, i))
                    .collect()
            }
        };
        let m = self.store().mapper(&tps, &args);
        Some(self.instantiate_signature_full(sig, m))
    }

    /// The props a component signature declares, before `LibraryManagedAttributes` and the
    /// intrinsic attributes (module doc); `None` when not modelled.
    fn jsx_raw_props(&mut self, ns: Option<SymId>, sig: SigId, construct: bool) -> Option<TypeId> {
        let first_param = |ck: &mut Self| ck.param_type_at(sig, 0).unwrap_or(TypeId::EMPTY_OBJECT);
        let p = if construct {
            let inst = self.return_type_of(sig);
            match self.jsx_ns_type(ns, "ElementAttributesProperty") {
                None => first_param(self),
                Some(eap) => match self.properties_of(eap)[..] {
                    [] => inst,
                    [(n, _)] => {
                        if self.jsx_unusable(inst) {
                            return None;
                        }
                        let p = self.property_of_type_id(inst, n)?;
                        self.type_of_symbol(p)
                    }
                    _ => return None,
                },
            }
        } else {
            first_param(self)
        };
        if self.jsx_unusable(p) {
            return None;
        }
        Some(p)
    }

    /// Whether props inference is modelled for `t`: type parameters, object types and
    /// unions or intersections of them.
    fn jsx_inferable(&self, t: TypeId, depth: u32) -> bool {
        use ts_r_types::TypeKind;
        if depth > 8 {
            return false;
        }
        match self.store().kind(t) {
            TypeKind::Union { .. } | TypeKind::Intersection { .. } => self
                .store()
                .members_of_union(t)
                .iter()
                .all(|&m| self.jsx_inferable(m, depth + 1)),
            TypeKind::IndexedAccess { .. }
            | TypeKind::Conditional(_)
            | TypeKind::Substitution { .. }
            | TypeKind::Index(_) => false,
            _ => true,
        }
    }

    /// Whether `t` is a type parameter or a union or intersection with one as a member.
    fn jsx_naked_type_param(&self, t: TypeId, depth: u32) -> bool {
        let flags = self.store().flags(t);
        if flags.contains(TypeFlags::TYPE_PARAMETER) {
            return true;
        }
        depth < 8
            && flags.intersects(TypeFlags::UNION | TypeFlags::INTERSECTION)
            && self
                .store()
                .members_of_union(t)
                .iter()
                .any(|&m| self.jsx_naked_type_param(m, depth + 1))
    }

    /// Whether some attribute value or child of `opening`'s element is context sensitive.
    fn jsx_has_context_sensitive(&self, opening: Loc, children: Option<StrId>) -> bool {
        let file = opening.file;
        let ast = self.ast(file);
        let Node::JsxOpening { attrs, .. } = *ast.node(opening.node) else {
            return false;
        };
        let cs = |x: NodeId| self.is_context_sensitive_expr(Loc::new(file, x));
        let in_attrs = ast.list(attrs).iter().any(|&a| match *ast.node(a) {
            Node::JsxAttr { value: Some(v), .. } => {
                matches!(*ast.node(v), Node::JsxExprContainer { expr: Some(x) } if cs(x))
            }
            _ => false,
        });
        let in_children = children.is_some()
            && self.c.parent(opening).is_some_and(|el| {
                matches!(*ast.node(el.node), Node::JsxElement { children: list, .. }
                if ast.list(list).iter().any(|&c| {
                    matches!(*ast.node(c), Node::JsxExprContainer { expr: Some(x) } if cs(x))
                }))
            });
        in_attrs || in_children
    }

    /// The props of a component signature (module doc); `None` when not modelled.
    fn jsx_sig_props(
        &mut self,
        ns: Option<SymId>,
        tag_type: TypeId,
        sig: SigId,
        construct: bool,
    ) -> Option<TypeId> {
        let p = self.jsx_raw_props(ns, sig, construct)?;
        let instance = construct.then(|| self.return_type_of(sig));
        let p = match self.jsx_ns_symbol(ns, "LibraryManagedAttributes") {
            Some(lma) => {
                let t = self.type_from_symbol_reference(lma, &[tag_type, p]);
                if t == TypeId::UNRESOLVED || self.store().is_error(t) {
                    return None;
                }
                t
            }
            None => p,
        };
        let mut parts = Vec::new();
        if let Some(ia) = self.jsx_ns_type(ns, "IntrinsicAttributes") {
            parts.push(ia);
        }
        if let Some(inst) = instance
            && let Some(ica) = self.jsx_ns_symbol(ns, "IntrinsicClassAttributes")
        {
            let t = self.type_from_symbol_reference(ica, &[inst]);
            if t != TypeId::UNRESOLVED && !self.store().is_error(t) {
                parts.push(t);
            }
        }
        if parts.is_empty() {
            return Some(p);
        }
        parts.push(p);
        Some(self.intersection(&parts))
    }

    // -----------------------------------------------------------------------------
    // Checking
    // -----------------------------------------------------------------------------

    /// Checks a JSX element or fragment; returns its type.
    pub(crate) fn check_jsx_expr(&mut self, e: Loc) -> TypeId {
        let ast = self.ast(e.file);
        let reporting = self.reporting(e.file);
        match *ast.node(e.node) {
            Node::JsxElement {
                opening,
                children,
                closing,
            } => {
                if reporting {
                    self.check_jsx_factory(e, Some(opening));
                }
                let ol = Loc::new(e.file, opening);
                let tag_diags = self.check_jsx_tag(ol);
                self.check_jsx_opening(ol, children);
                if reporting {
                    self.check_jsx_spread_children(e.file, children);
                }
                if let Some(c) = closing
                    && reporting
                {
                    self.check_jsx_closing(Loc::new(e.file, c), opening, tag_diags);
                }
            }
            Node::JsxFragment { children } => {
                if reporting {
                    self.check_jsx_factory(e, None);
                }
                self.check_jsx_child_exprs(e.file, children);
                if reporting {
                    self.check_jsx_spread_children(e.file, children);
                }
            }
            _ => {}
        }
        self.jsx_element_type(e)
    }

    /// Checks the tag of `opening` (TS7026, TS2339, TS2604 and the tag expression's own
    /// errors); returns the diagnostics reported in the tag name, which its closing tag
    /// repeats.
    fn check_jsx_tag(&mut self, opening: Loc) -> Vec<ts_r_diagnostics::Diagnostic> {
        let file = opening.file;
        let ast = self.ast(file);
        let Node::JsxOpening { name, .. } = *ast.node(opening.node) else {
            return Vec::new();
        };
        if crate::check::is_intrinsic_tag(ast, name) {
            if self.reporting(file) {
                let ns = self.jsx_ns(opening);
                let tag = self.jsx_tag_text(file, name);
                self.report_intrinsic_tag(ns, &tag, opening);
                self.check_jsx_intrinsic_element_type(ns, &tag, Loc::new(file, name));
                // `<A:foo>` under the React modes.
                if let Node::JsxNamespacedName { ns: n, .. } = *ast.node(name)
                    && let Node::Ident { name: a } = *ast.node(n)
                    && ast.atom(a).starts_with(|c: char| c.is_ascii_uppercase())
                    && matches!(
                        self.c.opts.jsx,
                        Some(JsxEmit::React | JsxEmit::ReactJsx | JsxEmit::ReactJsxDev)
                    )
                {
                    self.error(
                        Loc::new(file, name),
                        dg::REACT_COMPONENTS_CANNOT_INCLUDE_JSX_NAMESPACE_NAMES_2639,
                        &[],
                    );
                }
            }
            return Vec::new();
        }
        let mark = self.diags.len();
        let tt = self.jsx_tag_type(file, name);
        let span = ast.span(name);
        let own: Vec<_> = self.diags[mark..]
            .iter()
            .filter(|d| d.file == file && d.span.start >= span.start && d.span.end <= span.end)
            .cloned()
            .collect();
        if self.reporting(file) && !self.jsx_unusable(tt) && !self.c.file(file).is_js() {
            self.check_jsx_tag_callable(opening, name, tt);
        }
        own
    }

    /// TS2874 / TS2879 / TS17016 / TS2875 (module doc).
    fn check_jsx_factory(&mut self, e: Loc, opening: Option<NodeId>) {
        let ast = self.ast(e.file);
        let info = self.jsx_file_info(e.file);
        if info.automatic.is_some() {
            if let Some(spec) = info.missing_runtime
                && info.first == Some(e.node)
            {
                let at = opening.unwrap_or(e.node);
                let span = self.jsx_open_span(e.file, at);
                self.error_span(
                    e.file,
                    span,
                    dg::THIS_JSX_TAG_REQUIRES_THE_MODULE_PATH_0_TO_EXIST_BUT_NONE_COULD_BE_FOUND_MAKE_SU_2875,
                    &[&spec],
                );
            }
            // An untyped runtime: TS7016 at the first element, whatever `noImplicitAny`
            // says (observed: `tsc/incremental/react-jsx-emit-mode-with-no-backing-types-
            // found-doesnt-crash`).
            if let Some((spec, resolved, package)) = info.untyped_runtime
                && info.first == Some(e.node)
            {
                let mut chain = crate::diag::Chain::new();
                chain.push_back(
                    dg::COULD_NOT_FIND_A_DECLARATION_FILE_FOR_MODULE_0_1_IMPLICITLY_HAS_AN_ANY_TYPE_7016,
                    &[&spec, &resolved],
                );
                if package && !spec.starts_with('.') {
                    let first = spec.split('/').next().unwrap_or("");
                    let types_name = match first.strip_prefix('@') {
                        Some(scope) => {
                            format!("{scope}__{}", spec.split('/').nth(1).unwrap_or(""))
                        }
                        None => first.to_owned(),
                    };
                    chain.push_back(
                        dg::TRY_NPM_I_SAVE_DEV_TYPES_1_IF_IT_EXISTS_OR_ADD_A_NEW_DECLARATION_D_TS_FILE_CONTA_7035,
                        &[&spec, &types_name],
                    );
                }
                let span = ast.span(e.node);
                self.push_chain(e.file, span, &chain);
            }
            return;
        }
        if !matches!(self.c.opts.jsx, Some(ts_r_config::JsxEmit::React)) {
            return;
        }
        let sf = self.c.file(e.file);
        let pragma = crate::jsx_runtime::leading_pragma(ast, &sf.text, "@jsx").is_some();
        let custom = pragma || self.c.opts.jsx_factory_custom;
        let root = self.jsx_factory_root(e.file);
        match opening {
            Some(o) => {
                let Node::JsxOpening { name, .. } = *ast.node(o) else {
                    return;
                };
                let l = Loc::new(e.file, name);
                if self.jsx_lookup(l, &root).is_some() {
                    return;
                }
                if custom {
                    self.report_cannot_find_name(l, &root);
                } else {
                    self.error(
                        l,
                        dg::THIS_JSX_TAG_REQUIRES_0_TO_BE_IN_SCOPE_BUT_IT_COULD_NOT_BE_FOUND_2874,
                        &[&root],
                    );
                }
            }
            None => {
                let s = ast.span(e.node).start;
                let span = Span::new(s, s + 2);
                // An `@jsx` pragma: with `@jsxFrag` the factory must be in scope; without
                // it the fragment needs React's and TS17017.
                if pragma {
                    if let Some(frag) =
                        crate::jsx_runtime::leading_pragma(ast, &sf.text, "@jsxFrag")
                    {
                        let frag = frag.split('.').next().unwrap_or(frag).to_owned();
                        if self.jsx_lookup(e, &root).is_none() {
                            self.error_span(
                                e.file,
                                span,
                                dg::THIS_JSX_TAG_REQUIRES_0_TO_BE_IN_SCOPE_BUT_IT_COULD_NOT_BE_FOUND_2874,
                                &[&root],
                            );
                        }
                        // `@jsxFrag null` needs nothing in scope.
                        if !frag.is_empty() && frag != "null" && self.jsx_lookup(e, &frag).is_none()
                        {
                            self.error_span(
                                e.file,
                                span,
                                dg::THIS_JSX_TAG_REQUIRES_0_TO_BE_IN_SCOPE_BUT_IT_COULD_NOT_BE_FOUND_2874,
                                &[&frag],
                            );
                            self.error_span(
                                e.file,
                                span,
                                dg::USING_JSX_FRAGMENTS_REQUIRES_FRAGMENT_FACTORY_0_TO_BE_IN_SCOPE_BUT_IT_COULD_NOT_B_2879,
                                &[&frag],
                            );
                        }
                        return;
                    }
                    if self.jsx_lookup(e, "React").is_none() {
                        self.error_span(
                            e.file,
                            span,
                            dg::THIS_JSX_TAG_REQUIRES_0_TO_BE_IN_SCOPE_BUT_IT_COULD_NOT_BE_FOUND_2874,
                            &["React"],
                        );
                        self.error_span(
                            e.file,
                            span,
                            dg::USING_JSX_FRAGMENTS_REQUIRES_FRAGMENT_FACTORY_0_TO_BE_IN_SCOPE_BUT_IT_COULD_NOT_B_2879,
                            &["React"],
                        );
                    }
                    self.error_span(
                        e.file,
                        span,
                        dg::AN_JSXFRAG_PRAGMA_IS_REQUIRED_WHEN_USING_AN_JSX_PRAGMA_WITH_JSX_FRAGMENTS_17017,
                        &[],
                    );
                    return;
                }
                if custom {
                    if !self.c.opts.jsx_fragment_factory_set {
                        self.error_span(
                            e.file,
                            span,
                            dg::THE_JSXFRAGMENTFACTORY_COMPILER_OPTION_MUST_BE_PROVIDED_TO_USE_JSX_FRAGMENTS_WIT_17016,
                            &[],
                        );
                    }
                    return;
                }
                if self.c.opts.jsx_fragment_factory_set {
                    return;
                }
                if self.jsx_lookup(e, &root).is_none() {
                    self.error_span(
                        e.file,
                        span,
                        dg::THIS_JSX_TAG_REQUIRES_0_TO_BE_IN_SCOPE_BUT_IT_COULD_NOT_BE_FOUND_2874,
                        &[&root],
                    );
                    self.error_span(
                        e.file,
                        span,
                        dg::USING_JSX_FRAGMENTS_REQUIRES_FRAGMENT_FACTORY_0_TO_BE_IN_SCOPE_BUT_IT_COULD_NOT_B_2879,
                        &[&root],
                    );
                }
            }
        }
    }

    /// The span of an opening tag or fragment where TS2875 is reported.
    fn jsx_open_span(&self, file: FileId, n: NodeId) -> Span {
        let ast = self.ast(file);
        match *ast.node(n) {
            Node::JsxFragment { .. } => {
                let s = ast.span(n).start;
                Span::new(s, s + 2)
            }
            _ => ast.span(n),
        }
    }

    /// A closing tag: intrinsic lookups are reported again, and so are the errors of a
    /// value tag's name.
    fn check_jsx_closing(
        &mut self,
        closing: Loc,
        opening: NodeId,
        tag_diags: Vec<ts_r_diagnostics::Diagnostic>,
    ) {
        let ast = self.ast(closing.file);
        let Node::JsxClosing { name } = *ast.node(closing.node) else {
            return;
        };
        if crate::check::is_intrinsic_tag(ast, name) {
            let ns = self.jsx_ns(closing);
            let tag = self.jsx_tag_text(closing.file, name);
            self.report_intrinsic_tag(ns, &tag, closing);
            return;
        }
        let Node::JsxOpening { name: oname, .. } = *ast.node(opening) else {
            return;
        };
        if self.jsx_tag_text(closing.file, name) != self.jsx_tag_text(closing.file, oname) {
            return;
        }
        let (from, to) = (ast.span(oname).start, ast.span(name).start);
        // Use before initialization (TS2729) is reported at the opening tag only.
        for mut d in tag_diags.into_iter().filter(|d| d.code != 2729) {
            d.span = Span::new(d.span.start - from + to, d.span.end - from + to);
            self.diags.push(d);
        }
    }

    /// Checks an opening element's attributes and children against its resolved target.
    fn check_jsx_opening(&mut self, opening: Loc, children: NodeList) {
        let file = opening.file;
        let ast = self.ast(file);
        let Node::JsxOpening { name, attrs, .. } = *ast.node(opening.node) else {
            return;
        };
        let reporting = self.reporting(file);
        let ns = self.jsx_ns(opening);
        let r = self.jsx_resolved(opening);
        let Some(props) = r.target.props else {
            // Nothing to check against: the values are still checked.
            for &a in ast.list(attrs) {
                match *ast.node(a) {
                    Node::JsxAttr { value: Some(v), .. } => match *ast.node(v) {
                        Node::JsxExprContainer { expr: Some(x) } => {
                            self.check_expr(Loc::new(file, x));
                        }
                        Node::JsxElement { .. } | Node::JsxFragment { .. } => {
                            self.check_expr(Loc::new(file, v));
                        }
                        _ => {}
                    },
                    Node::JsxSpreadAttr { arg } => {
                        self.check_expr(Loc::new(file, arg));
                    }
                    _ => {}
                }
            }
            self.check_jsx_child_exprs(file, children);
            if reporting {
                // Spreads are still checked (TS2698, TS2783).
                self.jsx_attributes_type(opening, None, true);
                self.check_jsx_validity(opening, name, &r, ns);
            }
            return;
        };
        let attrs_t = self.jsx_attributes_type(opening, r.target.children, reporting);
        if r.target.children.is_none() {
            self.check_jsx_child_exprs(file, children);
        }
        if let Some(attrs_t) = attrs_t
            && reporting
        {
            // Children a component doesn't accept are only reported when the attributes
            // are otherwise assignable: a missing prop or a weak type is reported first.
            let mut attrs_t = attrs_t;
            if let Some(c) = r.target.children
                && let Some(cp) = self.property_of_type_id(attrs_t, c)
                && self
                    .c
                    .synth_sym(cp)
                    .is_some_and(|x| x.decl == Some(opening))
                && !self.is_known_property(props, c)
            {
                let regular = self.regular_of(attrs_t);
                if !self.is_assignable(regular, props) {
                    attrs_t = regular;
                }
            }
            let mark = self.diags.len();
            let (source, target) = self.jsx_report_target(ns, attrs_t, props);
            self.check_assignable_with_elaboration(
                source,
                target,
                Loc::new(file, name),
                Some(opening),
            );
            if r.no_overload {
                for d in &mut self.diags[mark..] {
                    d.message = crate::call::wrap_last_overload(&d.message);
                    d.code = 2769;
                    d.category = ts_r_diagnostics::Category::Error;
                }
                if let Some((sig, _)) = r.sig {
                    self.relate_last_overload(mark, sig);
                }
            }
        }
        if reporting {
            self.check_jsx_validity(opening, name, &r, ns);
        }
    }

    /// The source and target a failed attributes check is reported with. With
    /// `JSX.IntrinsicAttributes` and `JSX.IntrinsicClassAttributes` declared, attributes (an
    /// object, not a lone spread of a type parameter) that fail the props intersection for a
    /// reason other than an excess or weak-type failure are reported against the first
    /// member they fail, with no head for the intersection and no excess check against that
    /// member (`Property 'x' is missing in type '{}' but required in type 'P'.`; tsc:
    /// `reportErrorResults` in `tsc/internal/checker/relater.go`).
    fn jsx_report_target(
        &mut self,
        ns: Option<SymId>,
        attrs: TypeId,
        props: TypeId,
    ) -> (TypeId, TypeId) {
        if !self.store().flags(props).contains(TypeFlags::INTERSECTION)
            || !self.store().flags(attrs).contains(TypeFlags::OBJECT)
            || self.jsx_ns_type(ns, "IntrinsicAttributes").is_none()
            || self.jsx_ns_symbol(ns, "IntrinsicClassAttributes").is_none()
            || self.is_assignable(attrs, props)
        {
            return (attrs, props);
        }
        for (n, p) in self.properties_of(attrs) {
            if !self.is_known_property(props, n) && !self.is_hyphenated_jsx_attr(p) {
                return (attrs, props);
            }
        }
        // Members are related without excess property checks; weak type detection applies
        // to the whole intersection, not to its members.
        let regular = self.regular_of(attrs);
        let members = self.store().members_of_union(props).to_vec();
        match members.into_iter().find(|&m| {
            !self.is_assignable(regular, m)
                && !self.weak_type_violation(regular, m, RelationKind::Assignable)
        }) {
            Some(m) => (regular, m),
            None => (attrs, props),
        }
    }

    /// TS2604 for a value tag without call or construct signatures.
    fn check_jsx_tag_callable(&mut self, opening: Loc, name: NodeId, tt: TypeId) {
        // A type parameter (or `this`) is checked through its constraint.
        let tt = if self.store().flags(tt).intersects(TypeFlags::INSTANTIABLE) {
            let c = self.base_constraint(tt);
            if c == TypeId::UNKNOWN || self.jsx_unusable(c) {
                return;
            }
            c
        } else {
            tt
        };
        // A string literal tag is an intrinsic name: an unknown one is TS2339 at the element
        // and TS2604 at the tag.
        if self.store().flags(tt).intersects(TypeFlags::STRING_LITERAL)
            && let Some(text) = self.literal_text(tt)
        {
            let ns = self.jsx_ns(opening);
            if self.jsx_intrinsic_props(ns, &text) == Err(false) {
                self.error(
                    opening,
                    dg::PROPERTY_0_DOES_NOT_EXIST_ON_TYPE_1_2339,
                    &[&text, "JSX.IntrinsicElements"],
                );
                let tag = self.jsx_tag_text(opening.file, name);
                self.error(
                    Loc::new(opening.file, name),
                    dg::JSX_ELEMENT_TYPE_0_DOES_NOT_HAVE_ANY_CONSTRUCT_OR_CALL_SIGNATURES_2604,
                    &[&tag],
                );
            }
            return;
        }
        if self
            .store()
            .flags(tt)
            .intersects(TypeFlags::STRING_LIKE | TypeFlags::UNION | TypeFlags::INSTANTIABLE)
        {
            return;
        }
        if self.signatures_of(tt, SigKind::Call).is_empty()
            && self.signatures_of(tt, SigKind::Construct).is_empty()
        {
            let text = self.jsx_tag_text(opening.file, name);
            self.error(
                Loc::new(opening.file, name),
                dg::JSX_ELEMENT_TYPE_0_DOES_NOT_HAVE_ANY_CONSTRUCT_OR_CALL_SIGNATURES_2604,
                &[&text],
            );
        }
    }

    /// TS2786 and TS2607 (module doc).
    fn check_jsx_validity(
        &mut self,
        opening: Loc,
        name: NodeId,
        r: &JsxResolved,
        ns: Option<SymId>,
    ) {
        let file = opening.file;
        let (Some(tt), Some((sig, construct))) = (r.tag_type, r.sig) else {
            return;
        };
        let text = self.jsx_tag_text(file, name);
        let at = Loc::new(file, name);
        if construct
            && r.target.props.is_none()
            && let Some(eap) = self.jsx_ns_type(ns, "ElementAttributesProperty")
            && let [(n, _)] = self.properties_of(eap)[..]
        {
            let inst = self.return_type_of(sig);
            let ast = self.ast(file);
            let has_attrs = matches!(*ast.node(opening.node), Node::JsxOpening { attrs, .. } if !ast.list(attrs).is_empty());
            if has_attrs && !self.jsx_unusable(inst) && self.property_of_type_id(inst, n).is_none()
            {
                let pn = self.store().str(n).to_owned();
                self.error(
                    opening,
                    dg::JSX_ELEMENT_CLASS_DOES_NOT_SUPPORT_ATTRIBUTES_BECAUSE_IT_DOES_NOT_HAVE_A_0_PROPE_2607,
                    &[&pn],
                );
            }
        }
        // Overloads (none generic) are checked by their intersected return types.
        let kind = if construct {
            SigKind::Construct
        } else {
            SigKind::Call
        };
        let sigs = self.signatures_of(tt, kind);
        let returns = if sigs.len() > 1
            && sigs
                .iter()
                .all(|&s| self.store().sig(s).type_params == ListId::EMPTY)
        {
            let rs: Vec<TypeId> = sigs.iter().map(|&s| self.return_type_of(s)).collect();
            self.intersection(&rs)
        } else {
            self.return_type_of(sig)
        };
        let (source, target, info) = if let Some(et) = self.jsx_ns_type(ns, "ElementType") {
            (tt, et, dg::ITS_TYPE_0_IS_NOT_A_VALID_JSX_ELEMENT_TYPE_18053)
        } else if construct {
            let Some(ec) = self.jsx_ns_type(ns, "ElementClass") else {
                return;
            };
            (
                returns,
                ec,
                dg::ITS_INSTANCE_TYPE_0_IS_NOT_A_VALID_JSX_ELEMENT_2788,
            )
        } else {
            let Some(e) = self.jsx_ns_type(ns, "Element") else {
                return;
            };
            let t = self.union2(e, TypeId::NULL);
            (
                returns,
                t,
                dg::ITS_RETURN_TYPE_0_IS_NOT_A_VALID_JSX_ELEMENT_2787,
            )
        };
        if self.jsx_unusable(source) || self.is_assignable(source, target) {
            return;
        }
        let (inner, _) = self.relation_error_chain(source, target, RelationKind::Assignable, None);
        let st = self.type_to_string(source);
        let mut chain = Chain::new();
        chain.push_back(dg::_0_CANNOT_BE_USED_AS_A_JSX_COMPONENT_2786, &[&text]);
        chain.push_back(info, &[&st]);
        // The relation's own head (`Type 'X' is not assignable to type 'Element'.`) is not
        // repeated under "Its type ...".
        // Nor is anything for the intersected returns of overloads.
        let overloaded = self.store().flags(source).contains(TypeFlags::INTERSECTION);
        let skip = if overloaded || (inner.items.len() == 1 && inner.items[0].0 == 2322) {
            inner.items.len()
        } else {
            usize::from(inner.items.first().is_some_and(|i| i.0 == 2322))
        };
        for (code, msg) in inner.items.iter().skip(skip) {
            chain.items.push((*code, msg.clone()));
        }
        self.error_chain(at, &chain);
    }

    /// TS2786 for an intrinsic tag whose name `JSX.ElementType` doesn't accept.
    fn check_jsx_intrinsic_element_type(&mut self, ns: Option<SymId>, tag: &str, at: Loc) {
        let Some(et) = self.jsx_ns_type(ns, "ElementType") else {
            return;
        };
        let lit = self.store().string_literal(tag, false);
        if self.jsx_unusable(et) || self.is_assignable(lit, et) {
            return;
        }
        let st = self.type_to_string(lit);
        let mut chain = Chain::new();
        chain.push_back(dg::_0_CANNOT_BE_USED_AS_A_JSX_COMPONENT_2786, &[tag]);
        chain.push_back(dg::ITS_TYPE_0_IS_NOT_A_VALID_JSX_ELEMENT_TYPE_18053, &[&st]);
        self.error_chain(at, &chain);
    }

    /// Checks the children of an element whose children are not an attribute (or of a
    /// fragment).
    fn check_jsx_child_exprs(&mut self, file: FileId, children: NodeList) {
        let ast = self.ast(file);
        for &c in ast.list(children) {
            match *ast.node(c) {
                Node::JsxExprContainer { expr: Some(x) } | Node::JsxSpreadChild { expr: x } => {
                    self.check_expr(Loc::new(file, x));
                }
                Node::JsxElement { .. } | Node::JsxFragment { .. } => {
                    self.check_expr(Loc::new(file, c));
                }
                _ => {}
            }
        }
    }

    /// TS2609 for a spread child (`{...x}`) whose type is not an array.
    fn check_jsx_spread_children(&mut self, file: FileId, children: NodeList) {
        let ast = self.ast(file);
        for &c in ast.list(children) {
            if let Node::JsxSpreadChild { expr } = *ast.node(c) {
                let l = Loc::new(file, expr);
                let t = self.check_expr(l);
                if t == TypeId::ANY || t == TypeId::UNRESOLVED || self.c.is_array_like_type(t) {
                    continue;
                }
                self.error(
                    Loc::new(file, c),
                    dg::JSX_SPREAD_CHILD_MUST_BE_AN_ARRAY_TYPE_2609,
                    &[],
                );
            }
        }
    }

    /// The children that count (module doc), in order.
    pub(crate) fn jsx_real_children(&self, file: FileId, children: NodeList) -> Vec<NodeId> {
        let sf = self.c.file(file);
        let ast = &sf.ast;
        ast.list(children)
            .iter()
            .copied()
            .filter(|&c| match *ast.node(c) {
                Node::JsxText {} => {
                    let s = ast.span(c);
                    let t = sf.text.get(s.start as usize..s.end as usize).unwrap_or("");
                    !(t.trim().is_empty() && t.contains('\n'))
                }
                Node::JsxExprContainer { expr: None } => false,
                _ => true,
            })
            .collect()
    }
}
