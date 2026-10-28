//! JSX attributes: the attributes type of an opening element (a fresh object literal type:
//! `true` for a bare attribute, string literals and expressions widened unless the prop's
//! type keeps the literal, spreads merged, children as the children prop), attribute
//! contextual types, and elaboration of a failed attributes check. What the attributes are
//! checked against is resolved in `jsx_check.rs`.
//!
//! Observed from tsc 7:
//! - A wrong attribute is reported at its name (`Type 'string' is not assignable to type
//!   'number'.`), an excess one at its name with the whole attributes type, a missing
//!   prop at the tag name. Hyphenated names (`data-x`, `aria-x`) are never excess.
//! - Attribute values are contextually typed by the prop's type; child expressions by the
//!   children prop's type (its element type when there are several children).
//! - With a spread, the attributes type is fresh only when an attribute or child was also
//!   written: those are checked for excess, the spread's own properties are not
//!   (`<A {...props} zzz />` reports `zzz`; `<A {...props} />` reports nothing).
//! - Without a `JSX` namespace (React's types not installed) a component is checked like a
//!   call against its first parameter, and children are not attributes, so a required
//!   `children` prop is reported missing even when the element has children.

use ts_r_ast::{Node, NodeId};
use ts_r_binder::SymbolFlags;
use ts_r_types::{StrId, SymId, TypeFlags, TypeId};

use crate::checker::{Ck, Loc};
use crate::declared::TupleEl;
use crate::symbols::{SynthSym, check_flags};
use ts_r_span::Span;

impl<'c, 'a> Ck<'c, 'a> {
    /// The props type the attributes of a `JsxOpening` are checked against (see
    /// `jsx_check.rs`), or `None` when not modelled.
    pub fn jsx_props_type(&mut self, opening: Loc) -> Option<TypeId> {
        self.jsx_target(opening).props
    }

    /// The text of a JSX attribute name (`a`, `ns:a`).
    fn jsx_attr_name(&self, file: ts_r_diagnostics::FileId, name: NodeId) -> Option<String> {
        let ast = self.ast(file);
        match *ast.node(name) {
            Node::Ident { name } => Some(ast.atom(name).to_owned()),
            Node::JsxNamespacedName { ns, name } => {
                let a = self.jsx_attr_name(file, ns)?;
                let b = self.jsx_attr_name(file, name)?;
                Some(format!("{a}:{b}"))
            }
            _ => None,
        }
    }

    /// The contextual type of the value of the JSX attribute `attr` (its prop's type), or of
    /// a child expression of the element `attr` (the children prop's type; one element of
    /// it when there are several children).
    pub(crate) fn jsx_attr_contextual_type(&mut self, attr: Loc) -> Option<TypeId> {
        let ast = self.ast(attr.file);
        match *ast.node(attr.node) {
            Node::JsxAttr { name, .. } => {
                let opening = self.c.parent(attr)?;
                let props = self.jsx_contextual_props(opening)?;
                let n = self.jsx_attr_name(attr.file, name)?;
                if let Some(t) = self.contextual_property_type(props, &n) {
                    return Some(t);
                }
                self.index_info_of(props, TypeId::STRING).map(|i| i.value)
            }
            Node::JsxSpreadAttr { .. } => {
                let opening = self.c.parent(attr)?;
                self.jsx_props_type(opening)
            }
            Node::JsxElement {
                opening, children, ..
            } => {
                let ol = Loc::new(attr.file, opening);
                let name = self.jsx_target(ol).children?;
                let props = self.jsx_contextual_props(ol)?;
                let n = self.store().str(name).to_owned();
                let ct = self.contextual_property_type(props, &n)?;
                if self.jsx_real_children(attr.file, children).len() > 1 {
                    return Some(self.element_type_of_array_like(ct).unwrap_or(ct));
                }
                Some(ct)
            }
            _ => None,
        }
    }

    /// The props of `opening` as a contextual type: a union narrowed by the discriminant
    /// attributes given (`<Foo renderNumber>{(v) => {}}</Foo>` types `v` from the member
    /// with `renderNumber: true`; observed tsc 7).
    fn jsx_contextual_props(&mut self, opening: Loc) -> Option<TypeId> {
        let props = self.jsx_props_type(opening)?;
        if !self.store().flags(props).contains(TypeFlags::UNION) {
            return Some(props);
        }
        let file = opening.file;
        let ast = self.ast(file);
        let Node::JsxOpening { attrs, .. } = *ast.node(opening.node) else {
            return Some(props);
        };
        let mut items = Vec::new();
        let mut present = Vec::new();
        let mut open_ended = false;
        for &a in ast.list(attrs) {
            let Node::JsxAttr { name, value } = *ast.node(a) else {
                open_ended = true;
                continue;
            };
            let Some(n) = self.jsx_attr_name(file, name) else {
                continue;
            };
            let vt = match value.map(|v| *ast.node(v)) {
                None => Some(TypeId::TRUE),
                Some(Node::StringLit { value }) => {
                    Some(self.store().string_literal(ast.atom(value), false))
                }
                Some(Node::JsxExprContainer { expr: Some(x) }) => {
                    self.discriminant_value_type(Loc::new(file, x))
                }
                _ => None,
            };
            if let Some(vt) = vt {
                items.push((n.clone(), Some(vt)));
            }
            present.push(n);
        }
        if let Some(el) = self.c.parent(opening)
            && let Node::JsxElement { children, .. } = *ast.node(el.node)
            && let Some(name) = self.jsx_target(opening).children
            && !self.jsx_real_children(file, children).is_empty()
        {
            present.push(self.store().str(name).to_owned());
        }
        Some(self.discriminate_union_by(props, items, &present, open_ended))
    }

    /// Whether `t` can be spread (observed tsc 7): `any`, object types, unconstrained type
    /// parameters and unions of these with `null`/`undefined`; not primitives, `null`,
    /// `undefined`, `never`, `unknown` or a type parameter constrained to `any`.
    pub(crate) fn is_valid_spread_type(&mut self, t: TypeId, depth: u32) -> bool {
        let f = self.store().flags(t);
        if depth > 8 || f.intersects(TypeFlags::ANY) {
            return true;
        }
        if f.contains(TypeFlags::UNION) {
            let ms = self.store().members_of_union(t).to_vec();
            let mut some = false;
            for m in ms {
                if m == TypeId::NULL || m == TypeId::UNDEFINED {
                    continue;
                }
                if !self.is_valid_spread_type(m, depth + 1) {
                    return false;
                }
                some = true;
            }
            return some;
        }
        if f.contains(TypeFlags::TYPE_PARAMETER) {
            return match self.type_param_constraint(t) {
                None => true,
                Some(c) => {
                    !self
                        .store()
                        .flags(c)
                        .intersects(TypeFlags::ANY | TypeFlags::UNKNOWN)
                        && self.is_valid_spread_type(c, depth + 1)
                }
            };
        }
        if f.intersects(TypeFlags::PRIMITIVE | TypeFlags::NEVER | TypeFlags::UNKNOWN) {
            return false;
        }
        true
    }

    /// TS2783 for each earlier explicit attribute the spread of `t` always overwrites.
    fn report_jsx_overwritten(
        &mut self,
        file: ts_r_diagnostics::FileId,
        t: TypeId,
        overwritable: &mut Vec<(StrId, NodeId)>,
    ) {
        if self.store().flags(t).intersects(TypeFlags::ANY) {
            return;
        }
        let mut i = 0;
        while i < overwritable.len() {
            let (nid, a) = overwritable[i];
            let required = self
                .property_of_type_id(t, nid)
                .is_some_and(|p| !self.c.sym_flags(p).intersects(SymbolFlags::OPTIONAL));
            if required {
                let n = self.store().str(nid).to_owned();
                self.error(
                    Loc::new(file, a),
                    crate::diag::dg::_0_IS_SPECIFIED_MORE_THAN_ONCE_SO_THIS_USAGE_WILL_BE_OVERWRITTEN_2783,
                    &[&n],
                );
                overwritable.remove(i);
            } else {
                i += 1;
            }
        }
    }

    /// Whether `p` is a hyphenated JSX attribute (`data-x`), which is never excess.
    /// A hyphenated property of an object literal spread straight into the attributes
    /// (`{...{ "data-x": 1 }}`) counts too.
    pub(crate) fn is_hyphenated_jsx_attr(&self, p: SymId) -> bool {
        let Some(d) = self.c.first_decl(p) else {
            return false;
        };
        let ast = self.ast(d.file);
        if let Node::JsxAttr { name, .. } = *ast.node(d.node) {
            return matches!(*ast.node(name), Node::Ident { name: a } if ast.atom(a).contains('-'));
        }
        if !self.c.sym_name(p).contains('-') || !matches!(*ast.node(d.node), Node::Property { .. })
        {
            return false;
        }
        // The literal's own spread: property -> object literal (-> parentheses) -> spread.
        let mut cur = d;
        for _ in 0..16 {
            let Some(up) = self.c.parent(cur) else {
                return false;
            };
            match *ast.node(up.node) {
                Node::ObjectLit { .. } | Node::Paren { .. } => cur = up,
                Node::JsxSpreadAttr { .. } => return true,
                _ => return false,
            }
        }
        false
    }

    /// Whether the contextual type of the value of attribute `attr` (or of a child of the
    /// element `attr`) is known, present or absent: the props are resolved and are not
    /// `any` or generic, so a missing prop means no context (implicit-any parameters are
    /// reported there).
    pub(crate) fn jsx_context_modelled(&mut self, attr: Loc) -> bool {
        let ast = self.ast(attr.file);
        let opening = match *ast.node(attr.node) {
            Node::JsxAttr { .. } => match self.c.parent(attr) {
                Some(o) => o,
                None => return false,
            },
            Node::JsxElement { opening, .. } => Loc::new(attr.file, opening),
            _ => return false,
        };
        let target = self.jsx_target(opening);
        let Some(props) = target.props else {
            return false;
        };
        let unknown = |ck: &mut Self, t: TypeId| {
            t == TypeId::UNRESOLVED
                || ck.store().is_error(t)
                || ck.some_member(t, |m| {
                    m == TypeId::UNRESOLVED || ck.store().flags(m).intersects(TypeFlags::ANY)
                })
                || ck
                    .store()
                    .traits(t)
                    .contains(ts_r_types::TypeTraits::CONTAINS_TYPE_PARAMETER)
        };
        if unknown(self, props) {
            return false;
        }
        if matches!(ast.node(attr.node), Node::JsxElement { .. }) && target.children.is_none() {
            return true;
        }
        match self.jsx_attr_contextual_type(attr) {
            Some(t) => !unknown(self, t),
            None => true,
        }
    }

    /// The type of a JSX attribute's value (`None` for an empty `{}`).
    fn jsx_attr_value_type(
        &mut self,
        file: ts_r_diagnostics::FileId,
        value: Option<NodeId>,
    ) -> Option<TypeId> {
        let Some(v) = value else {
            return Some(TypeId::TRUE);
        };
        let ast = self.ast(file);
        match *ast.node(v) {
            Node::JsxExprContainer { expr: Some(x) } => {
                let t = self.check_expr_mutable(Loc::new(file, x));
                // A JSX element is `any` without a `JSX` namespace.
                if t == TypeId::UNRESOLVED
                    && matches!(
                        ast.node(crate::nodes::skip_parens(ast, x)),
                        Node::JsxElement { .. } | Node::JsxFragment { .. }
                    )
                {
                    return Some(TypeId::ANY);
                }
                Some(t)
            }
            Node::JsxExprContainer { expr: None } => None,
            Node::StringLit { .. } | Node::JsxElement { .. } | Node::JsxFragment { .. } => {
                Some(self.check_expr_mutable(Loc::new(file, v)))
            }
            _ => Some(TypeId::UNRESOLVED),
        }
    }

    /// The attributes type of `opening`, with the element's children as the `children`
    /// prop when given (`None` when unknown: an unresolved spread). `reporting` reports
    /// TS2710.
    pub(crate) fn jsx_attributes_type(
        &mut self,
        opening: Loc,
        children: Option<StrId>,
        reporting: bool,
    ) -> Option<TypeId> {
        self.jsx_attributes_type_inner(opening, children, reporting, false)
    }

    /// [`Ck::jsx_attributes_type`]; `context_free` leaves out context-sensitive values.
    pub(crate) fn jsx_attributes_type_inner(
        &mut self,
        opening: Loc,
        children: Option<StrId>,
        reporting: bool,
        context_free: bool,
    ) -> Option<TypeId> {
        let ol = opening;
        let file = opening.file;
        let ast = self.ast(file);
        let Node::JsxOpening { attrs, .. } = *ast.node(opening.node) else {
            return None;
        };
        let mut members: Vec<(StrId, SymId)> = Vec::new();
        let mut index_types = Vec::new();
        let mut spread: Option<TypeId> = None;
        let mut explicit_children: Option<NodeId> = None;
        // Explicit attributes a later spread may overwrite (TS2783).
        let mut overwritable: Vec<(StrId, NodeId)> = Vec::new();
        let mut lone_spread: Option<TypeId> = None;
        // An explicit attribute was written.
        let mut written = false;
        for &a in ast.list(attrs) {
            match *ast.node(a) {
                Node::JsxAttr { name: an, value } => {
                    let n = self.jsx_attr_name(file, an)?;
                    if context_free
                        && let Some(v) = value
                        && let Node::JsxExprContainer { expr: Some(x) } = *ast.node(v)
                        && self.is_context_sensitive_expr(Loc::new(file, x))
                    {
                        continue;
                    }
                    let Some(ty) = self.jsx_attr_value_type(file, value) else {
                        continue;
                    };
                    let nid = self.store().intern_str(&n);
                    if Some(nid) == children {
                        explicit_children = Some(an);
                    }
                    members.retain(|(m, _)| *m != nid);
                    if reporting {
                        overwritable.retain(|(m, _)| *m != nid);
                        overwritable.push((nid, a));
                    }
                    let s = self.c.new_synth(
                        SynthSym::new(nid, SymbolFlags::PROPERTY, check_flags::OBJECT_LITERAL)
                            .with_type(ty)
                            .with_decl(Some(Loc::new(file, a))),
                    );
                    members.push((nid, s));
                    written = true;
                }
                Node::JsxSpreadAttr { arg } => {
                    let t = self.check_expr(Loc::new(file, arg));
                    if t == TypeId::UNRESOLVED {
                        return None;
                    }
                    // A lone spread of a type parameter or of something that can't be spread
                    // is the attributes type itself.
                    if ast.list(attrs).len() == 1
                        && (!self.is_valid_spread_type(t, 0)
                            || self.store().flags(t).intersects(TypeFlags::INSTANTIABLE))
                    {
                        lone_spread = Some(t);
                    }
                    if !self.is_valid_spread_type(t, 0) {
                        if reporting {
                            self.error(
                                Loc::new(file, arg),
                                crate::diag::dg::SPREAD_TYPES_MAY_ONLY_BE_CREATED_FROM_OBJECT_TYPES_2698,
                                &[],
                            );
                        }
                        continue;
                    }
                    if reporting {
                        self.report_jsx_overwritten(file, t, &mut overwritable);
                    }
                    let so_far = self.make_object_literal(
                        ol,
                        std::mem::take(&mut members),
                        &mut index_types,
                        false,
                        false,
                    );
                    let sp = match spread {
                        Some(sp) => self.spread_types(sp, so_far),
                        None => so_far,
                    };
                    spread = Some(self.spread_types(sp, t));
                }
                _ => return None,
            }
        }
        // Children (module doc of `jsx_check.rs`).
        if let Some(cname) = children
            && let Some(el) = self.c.parent(opening)
            && let Node::JsxElement { children: list, .. } = *ast.node(el.node)
        {
            let real = self.jsx_real_children(file, list);
            if !real.is_empty() {
                // At the attributes (from the first to the last attribute).
                let list = ast.list(attrs);
                if reporting
                    && explicit_children.is_some()
                    && let (Some(&first), Some(&last)) = (list.first(), list.last())
                {
                    let n = self.store().str(cname).to_owned();
                    let span = Span::new(ast.span(first).start, ast.span(last).end);
                    self.error_span(
                        file,
                        span,
                        crate::diag::dg::_0_ARE_SPECIFIED_TWICE_THE_ATTRIBUTE_NAMED_0_WILL_BE_OVERWRITTEN_2710,
                        &[&n],
                    );
                }
                let mut types = Vec::new();
                let mut skipped = false;
                let mut spread_child = false;
                for &c in &real {
                    match *ast.node(c) {
                        Node::JsxText {} => types.push(TypeId::STRING),
                        Node::JsxExprContainer { expr: Some(x) } => {
                            let xl = Loc::new(file, x);
                            if context_free && self.is_context_sensitive_expr(xl) {
                                skipped = true;
                                continue;
                            }
                            types.push(self.check_expr_mutable(xl));
                        }
                        Node::JsxSpreadChild { expr: x } => {
                            spread_child = true;
                            let t = self.check_expr(Loc::new(file, x));
                            types.push(self.element_type_of_array_like(t).unwrap_or(t));
                        }
                        _ => types.push(self.check_expr(Loc::new(file, c))),
                    }
                }
                if types.contains(&TypeId::UNRESOLVED) {
                    return None;
                }
                if !skipped && !types.is_empty() {
                    let ty = if types.len() == 1 && !spread_child {
                        types[0]
                    } else if !spread_child
                        && self.jsx_contextual_props(opening).is_some_and(|props| {
                            let n = self.store().str(cname).to_owned();
                            self.contextual_property_type(props, &n)
                                .is_some_and(|ct| self.is_tuple_like_context(ct))
                        })
                    {
                        let elems: Vec<TupleEl> = types
                            .iter()
                            .map(|&ty| TupleEl {
                                ty,
                                kind: ts_r_types::TupleElement::REQUIRED,
                                label: None,
                            })
                            .collect();
                        self.create_tuple(&elems, false)
                    } else {
                        let u = self.union(&types);
                        self.create_array(u, false)
                    };
                    members.retain(|(m, _)| *m != cname);
                    let s = self.c.new_synth(
                        SynthSym::new(cname, SymbolFlags::PROPERTY, check_flags::OBJECT_LITERAL)
                            .with_type(ty)
                            .with_decl(Some(opening)),
                    );
                    members.push((cname, s));
                }
            }
        }
        if let Some(t) = lone_spread
            && members.is_empty()
        {
            return Some(t);
        }
        let lit = self.make_object_literal(ol, members, &mut index_types, true, false);
        let attrs_t = match spread {
            Some(sp) => {
                if self.store().flags(sp).intersects(TypeFlags::ANY) {
                    return None;
                }
                let r = self.spread_types(sp, lit);
                // With an explicit attribute the result is fresh: the explicit attributes
                // and children are checked for excess, the spread properties not.
                if written {
                    self.fresh_spread_literal(r, ol)
                } else {
                    r
                }
            }
            None => lit,
        };
        (attrs_t != TypeId::UNRESOLVED).then_some(attrs_t)
    }

    /// Elaborates a failed attributes check to the attributes whose values are wrong.
    pub(crate) fn elaborate_jsx_attributes(
        &mut self,
        opening: Loc,
        source: TypeId,
        target: TypeId,
    ) -> bool {
        let ast = self.ast(opening.file);
        let Node::JsxOpening { attrs, .. } = *ast.node(opening.node) else {
            return false;
        };
        let file = opening.file;
        let mut reported = false;
        let list = ast.list(attrs);
        for (i, &a) in list.iter().enumerate() {
            let Node::JsxAttr { name, value } = *ast.node(a) else {
                continue;
            };
            let Some(n) = self.jsx_attr_name(file, name) else {
                continue;
            };
            // Hyphenated names are reported on the whole attributes type.
            if n.contains('-') {
                continue;
            }
            // A repeated name is reported at its last attribute.
            let repeated = list[i + 1..].iter().any(|&b| {
                matches!(*ast.node(b), Node::JsxAttr { name: bn, .. }
                    if self.jsx_attr_name(file, bn).as_deref() == Some(n.as_str()))
            });
            if repeated {
                continue;
            }
            let pt = match self.property_of_type(target, &n) {
                Some(p) => self.type_of_symbol(p),
                None => match self.index_info_of(target, TypeId::STRING) {
                    Some(info) => info.value,
                    None => continue,
                },
            };
            // The attribute's value, or the value of a later spread that overwrites it.
            let sp = self.property_of_type(source, &n);
            let from_spread = sp.is_some_and(|p| {
                self.c
                    .synth_sym(p)
                    .is_none_or(|x| x.decl != Some(Loc::new(file, a)))
            });
            let vt = if let Some(p) = sp
                && from_spread
            {
                self.type_of_symbol(p)
            } else {
                let Some(vt) = self.quietly(|ck| ck.jsx_attr_value_type(file, value)) else {
                    continue;
                };
                self.regular_of(vt)
            };
            if self.is_assignable(vt, pt) {
                continue;
            }
            if !from_spread
                && let Some(v) = value
                && let Node::JsxExprContainer { expr: Some(x) } = *ast.node(v)
                && self.elaborate_error(Loc::new(file, x), vt, pt)
            {
                reported = true;
                continue;
            }
            let (chain, at) =
                self.relation_error_chain(vt, pt, ts_r_types::RelationKind::Assignable, None);
            let at = if from_spread { None } else { at };
            self.error_chain(at.unwrap_or(Loc::new(file, name)), &chain);
            reported = true;
        }
        reported | self.elaborate_jsx_children(opening, target)
    }

    /// Elaborates a failed children prop (observed tsc 7): one child against a children
    /// type whose members are all array-like (strings count) is TS2745 at the tag, several
    /// children against one without array-like members TS2746; otherwise each child is
    /// checked against the children type (one child) or its element type (several), text
    /// with TS2747 at the text.
    fn elaborate_jsx_children(&mut self, opening: Loc, target: TypeId) -> bool {
        use crate::diag::dg;
        let file = opening.file;
        let ast = self.ast(file);
        let Node::JsxOpening { name: tag, .. } = *ast.node(opening.node) else {
            return false;
        };
        let Some(cname) = self.jsx_target(opening).children else {
            return false;
        };
        let Some(el) = self.c.parent(opening) else {
            return false;
        };
        let Node::JsxElement { children, .. } = *ast.node(el.node) else {
            return false;
        };
        let real = self.jsx_real_children(file, children);
        if real.is_empty()
            || real
                .iter()
                .any(|&c| matches!(ast.node(c), Node::JsxSpreadChild { .. }))
        {
            return false;
        }
        let Some(p) = self.property_of_type_id(target, cname) else {
            return false;
        };
        let ct = self.type_of_symbol(p);
        let members = self.union_members(ct);
        let array_like = |ck: &Self, m: TypeId| {
            ck.store().flags(m).intersects(TypeFlags::STRING_LIKE) || ck.c.is_array_like_type(m)
        };
        let n = self.store().str(cname).to_owned();
        let tag_at = Loc::new(file, tag);
        if real.len() == 1 {
            let c = real[0];
            let ct_t = self.jsx_child_type(file, c);
            if ct_t.is_none_or(|t| self.is_assignable(t, ct)) {
                return false;
            }
            if members.iter().all(|&m| array_like(self, m)) {
                let ts = self.type_to_string(ct);
                self.error(
                    tag_at,
                    dg::THIS_JSX_TAG_S_0_PROP_EXPECTS_TYPE_1_WHICH_REQUIRES_MULTIPLE_CHILDREN_BUT_ONLY_A_2745,
                    &[&n, &ts],
                );
                return true;
            }
            return self.elaborate_jsx_child(file, tag, c, &n, ct, ct);
        }
        // Several children are checked one by one against the element types of the array
        // members, or against `string` when every member is a string; otherwise TS2746.
        // Other iterable object members (`Iterable<ReactNode>`) take their iterated type.
        let mut elems = Vec::new();
        for &m in &members {
            if self.c.is_array_like_type(m) {
                match self.element_type_of_array_like(m) {
                    Some(e) => elems.push(e),
                    None => return false,
                }
            } else if self.store().flags(m).intersects(TypeFlags::OBJECT)
                && self
                    .global_type(crate::checker::GlobalType::Iterable)
                    .is_some()
                && let Some(e) = self.iterated_type(m, false)
            {
                elems.push(e);
            }
        }
        if elems.is_empty() {
            let strings = members
                .iter()
                .filter(|&&m| m != TypeId::NULL && m != TypeId::UNDEFINED)
                .all(|&m| self.store().flags(m).intersects(TypeFlags::STRING_LIKE));
            if !strings {
                let ts = self.type_to_string(ct);
                self.error(
                    tag_at,
                    dg::THIS_JSX_TAG_S_0_PROP_EXPECTS_A_SINGLE_CHILD_OF_TYPE_1_BUT_MULTIPLE_CHILDREN_WER_2746,
                    &[&n, &ts],
                );
                return true;
            }
            elems.push(TypeId::STRING);
        }
        let elem = self.union(&elems);
        let mut reported = false;
        for c in real {
            reported |= self.elaborate_jsx_child(file, tag, c, &n, elem, ct);
        }
        reported
    }

    /// The type of a JSX child (`string` for text); `None` when unknown.
    fn jsx_child_type(&mut self, file: ts_r_diagnostics::FileId, c: NodeId) -> Option<TypeId> {
        let ast = self.ast(file);
        let t = match *ast.node(c) {
            Node::JsxText {} => TypeId::STRING,
            Node::JsxExprContainer { expr: Some(x) } => {
                self.quietly(|ck| ck.check_expr_mutable(Loc::new(file, x)))
            }
            Node::JsxExprContainer { expr: None } | Node::JsxSpreadChild { .. } => return None,
            _ => self.quietly(|ck| ck.check_expr(Loc::new(file, c))),
        };
        (t != TypeId::UNRESOLVED && !self.store().is_error(t)).then(|| self.regular_of(t))
    }

    /// Reports child `c` when its type is not assignable to `target`; `full` is the
    /// children prop's type (TS2747 names it).
    fn elaborate_jsx_child(
        &mut self,
        file: ts_r_diagnostics::FileId,
        tag: NodeId,
        c: NodeId,
        children: &str,
        target: TypeId,
        full: TypeId,
    ) -> bool {
        let Some(t) = self.jsx_child_type(file, c) else {
            return false;
        };
        if self.is_assignable(t, target) {
            return false;
        }
        let ast = self.ast(file);
        let at = Loc::new(file, c);
        match *ast.node(c) {
            Node::JsxText {} => {
                let tag_text = self.jsx_tag_text(file, tag);
                let ts = self.type_to_string(full);
                self.error(
                    at,
                    crate::diag::dg::_0_COMPONENTS_DON_T_ACCEPT_TEXT_AS_CHILD_ELEMENTS_TEXT_IN_JSX_HAS_THE_TYPE_STRING_2747,
                    &[&tag_text, children, &ts],
                );
                return true;
            }
            Node::JsxExprContainer { expr: Some(x) } => {
                if self.elaborate_error(Loc::new(file, x), t, target) {
                    return true;
                }
            }
            _ => {}
        }
        let (chain, eat) =
            self.relation_error_chain(t, target, ts_r_types::RelationKind::Assignable, None);
        self.error_chain(eat.unwrap_or(at), &chain);
        true
    }
}
