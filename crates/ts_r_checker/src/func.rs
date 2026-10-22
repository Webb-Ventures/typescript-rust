//! Contextual typing, function expressions and arrows, and return type inference.

use ts_r_ast::{FnFlags, KeywordTypeKind, MethodKind, Modifiers, Node, NodeId};
use ts_r_binder::{SymbolFacts, SymbolFlags};
use ts_r_types::{
    AliasId, ObjectFlags, ResolvedMembers, SigFlags, SigId, StrId, SymId, TupleElement, TypeFlags,
    TypeId, TypeTraits,
};

use crate::checker::{Ck, Loc};
use crate::declared::TupleEl;
use crate::members::SigKind;
use crate::nodes::{fn_parts, is_this_param, key_text};
use crate::unions::Reduction;

/// Fixed parameters, and the rest parameter with its element type.
type CombinableParams = (Vec<ts_r_types::Param>, Option<(ts_r_types::Param, TypeId)>);

impl<'c, 'a> Ck<'c, 'a> {
    // ---------------------------------------------------------------------------------
    // Contextual types
    // ---------------------------------------------------------------------------------

    /// The contextual type of an expression, from its position.
    pub fn contextual_type(&mut self, e: Loc) -> Option<TypeId> {
        if !self.fixed_arg_contexts.is_empty()
            && let Some(&t) = self.fixed_arg_contexts.get(&e)
        {
            return t;
        }
        for &(l, t) in self.context_overrides.iter().rev() {
            if l == e {
                return Some(t);
            }
        }
        if self.depth > 300 {
            return None;
        }
        self.depth += 1;
        let r = self.contextual_type_inner(e);
        self.depth -= 1;
        r.filter(|&t| t != TypeId::UNRESOLVED && !self.store().is_error(t))
    }

    fn contextual_type_inner(&mut self, e: Loc) -> Option<TypeId> {
        let parent = self.c.parent(e)?;
        let ast = self.ast(e.file);
        let f = e.file;
        match *ast.node(parent.node) {
            Node::Paren { .. } | Node::NonNull { .. } => self.contextual_type(parent),
            Node::BindingProperty { init: Some(i), .. }
                if i == e.node && self.in_binding_pattern(parent) =>
            {
                self.binding_default_context(parent)
            }
            Node::AssignPattern { right, .. }
                if right == e.node && self.in_binding_pattern(parent) =>
            {
                self.binding_default_context(parent)
            }
            Node::JsxExprContainer { .. } => {
                let attr = self.c.parent(parent)?;
                self.jsx_attr_contextual_type(attr)
            }
            Node::JsxAttr { value: Some(v), .. } if v == e.node => {
                self.jsx_attr_contextual_type(parent)
            }
            Node::JsxSpreadAttr { arg } if arg == e.node => self.jsx_attr_contextual_type(parent),
            Node::ExportDefaultExpr { .. } => self.jsdoc_export_default_type(parent),
            Node::VarDeclarator {
                type_ann,
                init: Some(i),
                name,
                ..
            } if i == e.node => match type_ann {
                Some(t) => Some(self.type_from_node(Loc::new(f, t))),
                None => {
                    // An array binding pattern types an array literal initializer
                    // (`const [a, b] = [1, "x"]` destructures the tuple `[number, string]`).
                    // Also through conditionals and `||`/`??`/`&&` (`c ? [a, b] : [x, y]`).
                    // Not other initializers: a pattern is no inference source for calls.
                    if matches!(ast.node(name), Node::ArrayPattern { .. })
                        && array_literal_tree(ast, i, 0) == Some(true)
                    {
                        return Some(self.type_from_binding_pattern(Loc::new(f, name)));
                    }
                    None
                }
            },
            Node::Param {
                type_ann,
                init: Some(i),
                ..
            } if i == e.node => match type_ann {
                Some(t) => Some(self.type_from_node(Loc::new(f, t))),
                None => self.contextual_param_type(parent).or_else(|| {
                    // A binding pattern types its default (`([a, b] = [1, "x"])` is a tuple).
                    let Node::Param { pattern, .. } = *ast.node(parent.node) else {
                        return None;
                    };
                    if matches!(
                        ast.node(pattern),
                        Node::ObjectPattern { .. } | Node::ArrayPattern { .. }
                    ) {
                        Some(self.type_from_binding_pattern(Loc::new(f, pattern)))
                    } else {
                        None
                    }
                }),
            },
            Node::PropertyDef {
                type_ann: Some(t),
                value: Some(v),
                ..
            } if v == e.node => Some(self.type_from_node(Loc::new(f, t))),
            // A static field of a class expression: the class's contextual type's property.
            Node::PropertyDef {
                modifiers,
                key,
                type_ann: None,
                value: Some(v),
                ..
            } if v == e.node && modifiers.contains(Modifiers::STATIC) => {
                let class = self.c.parent(parent)?;
                if !matches!(ast.node(class.node), Node::ClassExpr { .. }) {
                    return None;
                }
                let ct = self.contextual_type(class)?;
                let name = self.member_key_name(Loc::new(f, key))?;
                self.contextual_property_type(ct, &name)
            }
            Node::Return { .. } => {
                let func = self.containing_function(parent)?;
                self.contextual_return_type(func)
            }
            Node::Yield {
                delegate: false,
                arg: Some(a),
            } if a == e.node => {
                let func = self.containing_function(parent)?;
                self.generator_context(func).map(|(y, _)| y)
            }
            // `yield* x` in a generator yielding `Y`: `x` is an iterable of `Y` (an async
            // generator also takes async iterables).
            Node::Yield {
                delegate: true,
                arg: Some(a),
            } if a == e.node => {
                let func = self.containing_function(parent)?;
                let is_async = fn_parts(ast, func.node)?.flags.contains(FnFlags::ASYNC);
                let (y, r) = self.generator_context(func)?;
                let r = if r == TypeId::UNRESOLVED {
                    TypeId::ANY
                } else {
                    r
                };
                use crate::checker::GlobalType as G;
                let mut out = Vec::new();
                let gs: &[G] = if is_async {
                    &[G::AsyncIterable, G::Iterable]
                } else {
                    &[G::Iterable]
                };
                for &g in gs {
                    let d = self.global_type(g)?;
                    let n = match self.store().object_of(d).map(|o| o.kind) {
                        Some(ts_r_types::ObjectKind::Declared { type_params, .. }) => {
                            self.store().list_items(type_params).len()
                        }
                        _ => 0,
                    };
                    let args: Vec<TypeId> = [y, r, TypeId::ANY].into_iter().take(n).collect();
                    if args.len() != n {
                        return None;
                    }
                    out.push(self.make_reference(d, &args));
                }
                Some(self.union_with(&out, Reduction::None, AliasId::NONE))
            }
            Node::Arrow { body, .. } if body == e.node => self.contextual_return_type(parent),
            // `await x` with context `T`: `x` may be a `T` or a `PromiseLike<T>`.
            Node::Await { .. } => {
                let t = self.contextual_type(parent)?;
                if self.store().flags(t).intersects(TypeFlags::ANY_OR_UNKNOWN) {
                    return Some(t);
                }
                let a = self.awaited_type(t);
                match self.global_reference(crate::checker::GlobalType::PromiseLike, &[a]) {
                    Some(p) => Some(self.union(&[a, p])),
                    None => Some(t),
                }
            }
            Node::Call { args, .. } | Node::New { args, .. } => {
                let idx = ast.list(args).iter().position(|&a| a == e.node)?;
                self.contextual_arg_type(parent, idx, e)
            }
            // A substitution of a tagged template: argument 1 + its index.
            Node::TemplateLit { parts } => {
                let tagged = self.c.parent(parent)?;
                if !matches!(*ast.node(tagged.node), Node::TaggedTemplate { quasi, .. } if quasi == parent.node)
                {
                    return None;
                }
                let pos = ast.list(parts).iter().position(|&a| a == e.node)?;
                if pos % 2 == 0 {
                    return None;
                }
                self.contextual_arg_type(tagged, 1 + pos / 2, e)
            }
            Node::Property { key, value, .. } if value == e.node => {
                if let Some(t) = self.jsdoc_property_type(parent) {
                    return Some(t);
                }
                let obj = self.c.parent(parent)?;
                let ct = self.contextual_type(obj)?;
                let ct = self.discriminate_object_context(obj, ct);
                self.contextual_member_type(ct, Loc::new(f, key))
            }
            // `{ type }`: the name is the value.
            Node::ShorthandProp {
                name, init: None, ..
            } if name == e.node => {
                if let Some(t) = self.jsdoc_property_type(parent) {
                    return Some(t);
                }
                let obj = self.c.parent(parent)?;
                if !matches!(ast.node(obj.node), Node::ObjectLit { .. }) {
                    return None;
                }
                let ct = self.contextual_type(obj)?;
                let ct = self.discriminate_object_context(obj, ct);
                let name = self.member_key_name(Loc::new(f, name))?;
                self.contextual_property_type(ct, &name)
            }
            Node::MethodDef { key, value, .. } if value == e.node => {
                let obj = self.c.parent(parent)?;
                if !matches!(ast.node(obj.node), Node::ObjectLit { .. }) {
                    return None;
                }
                let ct = self.contextual_type(obj)?;
                let ct = self.discriminate_object_context(obj, ct);
                self.contextual_member_type(ct, Loc::new(f, key))
            }
            Node::ArrayLit { elements } => {
                let ct = self.contextual_type(parent)?;
                let list = ast.list(elements);
                let idx = list.iter().position(|&a| a == e.node)?;
                let after_spread = list[..idx]
                    .iter()
                    .any(|&x| matches!(ast.node(x), Node::Spread { .. }));
                // An element after every spread is placed from the end of a tuple context
                // (`[...t3, 'a']` against `[string, string, string, 'a' | 'b']`).
                let from_end = (!list[idx + 1..]
                    .iter()
                    .any(|&x| matches!(ast.node(x), Node::Spread { .. })))
                .then(|| list.len() - idx);
                self.contextual_element_type(ct, idx, after_spread, from_end)
            }
            Node::Spread { .. } => {
                let gp = self.c.parent(parent)?;
                // An object spread is contextually typed by its object literal's type.
                if matches!(
                    ast.node(gp.node),
                    Node::ArrayLit { .. } | Node::ObjectLit { .. }
                ) {
                    return self.contextual_type(gp);
                }
                None
            }
            Node::Conditional { cons, alt, .. } if cons == e.node || alt == e.node => {
                self.contextual_type(parent)
            }
            Node::Binary { op, right, .. } => match op {
                ts_r_ast::BinaryOp::LogicalOr
                | ts_r_ast::BinaryOp::Coalesce
                | ts_r_ast::BinaryOp::LogicalAnd => {
                    // The left operand of `&&` has no contextual type (its value only
                    // reaches the result when falsy).
                    if op == ts_r_ast::BinaryOp::LogicalAnd && e.node != right {
                        return None;
                    }
                    let ct = self.contextual_type(parent);
                    if ct.is_none() && e.node == right && op != ts_r_ast::BinaryOp::LogicalAnd {
                        let ast = self.ast(f);
                        if let Node::Binary { left, .. } = *ast.node(parent.node) {
                            return Some(self.quietly(|ck| ck.check_expr(Loc::new(f, left))));
                        }
                    }
                    ct
                }
                _ => None,
            },
            Node::Assign {
                op:
                    ts_r_ast::AssignOp::Assign
                    | ts_r_ast::AssignOp::AndAssign
                    | ts_r_ast::AssignOp::OrAssign
                    | ts_r_ast::AssignOp::CoalesceAssign,
                left,
                right,
            } if right == e.node => {
                // The value of an expando property declaration (`f.x = v`) has no
                // contextual type: it declares the property's type.
                // (One the variable's annotation or a JSDoc `@type` declares is typed by it.)
                // `/** @type {T} */ x.y = v` (JavaScript): `T`, whatever `x.y` is.
                let tag = ast
                    .jsdoc_tags_of(left)
                    .iter()
                    .find(|t| t.kind == ts_r_ast::JsDocTagKind::Type)
                    .and_then(|t| t.ty);
                if let Some(ty) = tag {
                    return Some(self.type_from_node(Loc::new(f, ty)));
                }
                if self.c.sym_of_decl(Loc::new(f, left)).is_some_and(|s| {
                    self.c.sym_facts(s).intersects(SymbolFacts::EXPANDO)
                        && self.annotated_expando_type(s).is_none()
                        // `this.x = v` in a method: typed by a constructor's `@type`.
                        && !(is_this_member(ast, left) && self.jsdoc_expando_type(s).is_some())
                }) || self.is_late_expando(Loc::new(f, left))
                    || self.assigns_inferred_this_property(Loc::new(f, left))
                {
                    return None;
                }
                // An array pattern types an array literal value as a tuple
                // (`[a, b] = [1, "x"]`), and gives an object literal value its element
                // types (`[a, b] = { 0: "", 1: true }` keeps `true` for a `boolean` b).
                if let Node::ArrayLit { elements } = *ast.node(crate::nodes::skip_parens(ast, left))
                    && (array_literal_tree(ast, right, 0) == Some(true)
                        || matches!(
                            ast.node(crate::nodes::skip_parens(ast, right)),
                            Node::ObjectLit { .. }
                        ))
                {
                    let els: Vec<NodeId> = ast.list(elements).to_vec();
                    let mut tuple: Vec<TupleEl> = Vec::with_capacity(els.len());
                    for el in els {
                        let (target, kind) = match *ast.node(el) {
                            // `[a, ...rest] = [1, 2, 3]`: still a tuple, with a rest element.
                            Node::Spread { .. } => {
                                tuple.push(TupleEl {
                                    ty: TypeId::ANY,
                                    kind: ts_r_types::TupleElement::REST,
                                    label: None,
                                });
                                continue;
                            }
                            Node::Hole {} => {
                                tuple.push(TupleEl {
                                    ty: TypeId::ANY,
                                    kind: ts_r_types::TupleElement::OPTIONAL,
                                    label: None,
                                });
                                continue;
                            }
                            Node::Assign { left: l, .. } => (l, ts_r_types::TupleElement::OPTIONAL),
                            _ => (el, ts_r_types::TupleElement::REQUIRED),
                        };
                        let ty = match *ast.node(crate::nodes::skip_parens(ast, target)) {
                            Node::Ident { .. } | Node::Member { .. } | Node::Index { .. } => {
                                self.quietly(|ck| ck.assignment_target_type(Loc::new(f, target)))
                            }
                            _ => TypeId::ANY,
                        };
                        tuple.push(TupleEl {
                            ty,
                            kind,
                            label: None,
                        });
                    }
                    return Some(self.create_tuple(&tuple, false));
                }
                let t = self.quietly(|ck| ck.assignment_target_type(Loc::new(f, left)));
                Some(t)
            }
            Node::As { ty, .. } | Node::TypeAssertion { ty, .. } | Node::Satisfies { ty, .. } => {
                if crate::nodes::is_keyword(ast, ty, KeywordTypeKind::Const) {
                    return self.contextual_type(parent);
                }
                Some(self.type_from_node(Loc::new(f, ty)))
            }
            Node::Seq { exprs } if ast.list(exprs).last() == Some(&e.node) => {
                self.contextual_type(parent)
            }
            Node::ExportAssignment { .. } => None,
            _ => None,
        }
    }

    /// Whether `left` (`f[k]`) is a late-bound expando assignment target.
    fn is_late_expando(&self, left: Loc) -> bool {
        let b = self.c.bound(left.file);
        let Node::Index { obj, .. } = *self.ast(left.file).node(left.node) else {
            return false;
        };
        b.resolved(obj)
            .is_some_and(|t| b.late_expandos.binary_search(&(t, left.node)).is_ok())
    }

    /// Narrows a union contextual type of an object literal by the literal's discriminant
    /// properties (`{ d: "s", cb: (x) => {} }` against `S | N` types `cb` from `S`). Members
    /// without the property or whose property type doesn't accept the value drop out; when
    /// none is left the union stays.
    fn discriminate_object_context(&mut self, obj: Loc, ct: TypeId) -> TypeId {
        if !self.store().flags(ct).contains(TypeFlags::UNION) {
            return ct;
        }
        let ast = self.ast(obj.file);
        let Node::ObjectLit { props } = *ast.node(obj.node) else {
            return ct;
        };
        let mut items: Vec<(String, Option<TypeId>)> = Vec::new();
        let mut present: Vec<String> = Vec::new();
        let mut open_ended = false;
        for &p in ast.list(props) {
            let key = match *ast.node(p) {
                Node::Property { key, .. } | Node::MethodDef { key, .. } => Some(key),
                Node::ShorthandProp { name, .. } => Some(name),
                _ => None,
            };
            let Some(name) = key.and_then(|k| key_text(ast, k)) else {
                open_ended = true;
                continue;
            };
            // A shorthand `{ kind }` discriminates by the named value's type.
            let value = match *ast.node(p) {
                Node::Property { value, .. } => Some(value),
                Node::ShorthandProp {
                    name: n,
                    init: None,
                    ..
                } => Some(n),
                _ => None,
            };
            if let Some(value) = value
                && let Some(vt) = self.discriminant_value_type(Loc::new(obj.file, value))
            {
                items.push((name.clone(), Some(vt)));
            }
            present.push(name);
        }
        self.discriminate_union_by(ct, items, &present, open_ended)
    }

    /// The core of [`Ck::discriminate_object_context`], shared with JSX attributes: narrows
    /// the union `ct` by the discriminant values in `items` (`None` for `undefined`), with
    /// `present` naming every property given and `open_ended` set when some are unknown.
    pub(crate) fn discriminate_union_by(
        &mut self,
        ct: TypeId,
        mut items: Vec<(String, Option<TypeId>)>,
        present: &[String],
        open_ended: bool,
    ) -> TypeId {
        if !self.store().flags(ct).contains(TypeFlags::UNION) {
            return ct;
        }
        let members = self.store().members_of_union(ct).to_vec();
        // Optional properties the literal leaves out discriminate as `undefined`
        // (`{ f }` against `{ t?: "a"; f } | { t: "b"; f }` picks the first).
        if !open_ended {
            let optional = self.optional_member_names(ct, &members);
            for &n in optional.iter() {
                let name = self.store().str(n);
                if !present.iter().any(|p| p == name) {
                    items.push((name.to_owned(), None));
                }
            }
        }
        if items.is_empty() {
            return ct;
        }
        let mut keep = vec![true; members.len()];
        for (name, vt) in items {
            let nid = self.store().intern_str(&name);
            let Some(types) = self.discriminant_column(ct, nid, &members) else {
                continue;
            };
            for (i, &t) in types.iter().enumerate() {
                if !keep[i] {
                    continue;
                }
                keep[i] = match (vt, t) {
                    (Some(vt), Some(t)) => self.is_assignable(vt, t),
                    (Some(_), None) => false,
                    (None, Some(t)) => {
                        let optional = self
                            .property_of_type_id(members[i], nid)
                            .is_some_and(|p| self.c.sym_flags(p).intersects(SymbolFlags::OPTIONAL));
                        optional || self.is_assignable(TypeId::UNDEFINED, t)
                    }
                    (None, None) => true,
                };
            }
        }
        let picked: Vec<TypeId> = members
            .iter()
            .zip(keep.iter())
            .filter(|(_, k)| **k)
            .map(|(m, _)| *m)
            .collect();
        if picked.is_empty() || picked.len() == members.len() {
            return ct;
        }
        self.union_with(&picked, Reduction::None, AliasId::NONE)
    }

    /// The optional property names of the members of the union `ct`, without duplicates,
    /// sorted by their text. Cached per union: object literals against one big union
    /// contextual type ask for every property they set.
    fn optional_member_names(&mut self, ct: TypeId, members: &[TypeId]) -> std::rc::Rc<[StrId]> {
        if let Some(r) = self.discriminants.optional.get(&ct) {
            return r.clone();
        }
        let mut names: Vec<StrId> = Vec::new();
        let mut seen: rustc_hash::FxHashSet<StrId> = Default::default();
        for &m in members {
            for (n, sym) in self.properties_of(m) {
                if self.c.sym_flags(sym).intersects(SymbolFlags::OPTIONAL) && seen.insert(n) {
                    names.push(n);
                }
            }
        }
        let s = self.store();
        names.sort_by(|a, b| s.str(*a).cmp(s.str(*b)));
        let r: std::rc::Rc<[StrId]> = names.into();
        self.discriminants.optional.insert(ct, r.clone());
        r
    }

    /// The type of property `name` in each member of the union `ct` (`None` where a member
    /// lacks it), or `None` when the property cannot discriminate: the same type in every
    /// member that has it, or no unit type among them. Cached per union and name.
    fn discriminant_column(
        &mut self,
        ct: TypeId,
        name: StrId,
        members: &[TypeId],
    ) -> Option<crate::checker::DiscriminantColumn> {
        if let Some(r) = self.discriminants.columns.get(&(ct, name)) {
            return r.clone();
        }
        let mut types: Vec<Option<TypeId>> = Vec::with_capacity(members.len());
        for &m in members {
            let t = self
                .property_of_type_id(m, name)
                .map(|p| self.type_of_symbol(p));
            types.push(t);
        }
        let first = types.iter().flatten().next().copied();
        let uniform = types.iter().all(|t| *t == first);
        let has_unit = types
            .iter()
            .flatten()
            .any(|&t| self.some_member(t, |m| self.store().flags(m).intersects(TypeFlags::UNIT)));
        let r = (!uniform && has_unit).then(|| std::rc::Rc::from(types));
        self.discriminants.columns.insert((ct, name), r.clone());
        r
    }

    /// The type of a property value that can discriminate a union without checking it:
    /// literals, `undefined`, and template expressions (a string, or its text when every
    /// part is a literal).
    pub(crate) fn discriminant_value_type(&mut self, v: Loc) -> Option<TypeId> {
        let ast = self.ast(v.file);
        let s = self.store();
        match *ast.node(v.node) {
            Node::Paren { expr } => self.discriminant_value_type(Loc::new(v.file, expr)),
            Node::StringLit { value } => Some(s.string_literal(ast.atom(value), false)),
            Node::NumberLit { value } => Some(s.number_literal(value.get(), false)),
            Node::BoolLit { value } => Some(if value { TypeId::TRUE } else { TypeId::FALSE }),
            Node::NullLit {} => Some(TypeId::NULL),
            Node::Ident { name } if ast.atom(name) == "undefined" => Some(TypeId::UNDEFINED),
            // Names and property accesses (`Kind.A`) don't depend on context.
            Node::Ident { .. } | Node::Member { .. } => {
                let t = self.quietly(|ck| ck.check_expr(v));
                (t != TypeId::UNRESOLVED && !self.store().is_error(t)).then_some(t)
            }
            Node::TemplateLit { parts } => {
                let mut text = String::new();
                for &p in ast.list(parts) {
                    match *ast.node(p) {
                        Node::TemplateElement { cooked, .. } => text.push_str(ast.atom(cooked)),
                        Node::StringLit { value } => text.push_str(ast.atom(value)),
                        _ => return Some(TypeId::STRING),
                    }
                }
                Some(s.string_literal(&text, false))
            }
            _ => None,
        }
    }

    pub(crate) fn member_key_name(&mut self, key: Loc) -> Option<String> {
        let ast = self.ast(key.file);
        if let Some(n) = key_text(ast, key.node) {
            return Some(n);
        }
        if let Node::ComputedKey { expr } = *ast.node(key.node) {
            let t = self.quietly(|ck| ck.check_expr(Loc::new(key.file, expr)));
            return self.property_name_of_type(self.store().regular(t));
        }
        None
    }

    /// The contextual type of an object literal member keyed by `key`: the property of
    /// that name, or for a computed key that is not a literal (`["" + 0]`, `[+"x"]`) the
    /// index signatures applying to its type (number keys prefer a number index
    /// signature, as in tsc 7).
    pub(crate) fn contextual_member_type(&mut self, ct: TypeId, key: Loc) -> Option<TypeId> {
        if let Some(name) = self.member_key_name(key) {
            return self.contextual_property_type(ct, &name);
        }
        let Node::ComputedKey { expr } = *self.ast(key.file).node(key.node) else {
            return None;
        };
        let t = self.quietly(|ck| ck.check_expr(Loc::new(key.file, expr)));
        let tf = self.store().flags(t);
        let index_key = if tf.intersects(TypeFlags::NUMBER_LIKE) {
            TypeId::NUMBER
        } else if tf.intersects(TypeFlags::STRING_LIKE) {
            TypeId::STRING
        } else if tf.intersects(TypeFlags::ES_SYMBOL_LIKE) {
            TypeId::SYMBOL
        } else {
            return None;
        };
        let mut out = Vec::new();
        for m in self.union_members(ct) {
            let mf = self.store().flags(m);
            if mf.intersects(TypeFlags::ANY) {
                return None;
            }
            if mf.intersects(TypeFlags::PRIMITIVE | TypeFlags::INSTANTIABLE) {
                continue;
            }
            if let Some(i) = self.index_info_of(m, index_key) {
                out.push(i.value);
            }
        }
        if out.is_empty() {
            return None;
        }
        Some(self.contextual_union(out))
    }

    /// The type of property `name` in a contextual type (mapped over unions).
    pub fn contextual_property_type(&mut self, ct: TypeId, name: &str) -> Option<TypeId> {
        let members = self.union_members(ct);
        let nid = self.store().intern_str(name);
        // A unique symbol's property name (`__@A@12`) keys by the symbol itself.
        let key = self.property_key_type(name);
        let is_symbol = self
            .store()
            .flags(key)
            .contains(TypeFlags::UNIQUE_ES_SYMBOL);
        let mut out = Vec::new();
        for m in members {
            let mf = self.store().flags(m);
            if mf.intersects(TypeFlags::PRIMITIVE) && !mf.intersects(TypeFlags::INSTANTIABLE) {
                // A primitive contributes the members of its apparent type (`normalize` of
                // `string | { normalize?: (m: M) => void }` is two signatures, no context).
                if mf.intersects(
                    TypeFlags::STRING_LIKE
                        | TypeFlags::NUMBER_LIKE
                        | TypeFlags::BIGINT_LIKE
                        | TypeFlags::BOOLEAN_LIKE
                        | TypeFlags::ES_SYMBOL_LIKE,
                ) {
                    let a = self.apparent_type(m);
                    if a != m
                        && a != TypeId::UNRESOLVED
                        && let Some(p) = self.property_of_type_id(a, nid)
                    {
                        out.push(self.type_of_symbol(p));
                    }
                }
                continue;
            }
            if mf.intersects(TypeFlags::ANY) {
                return None;
            }
            let am = if mf.intersects(TypeFlags::INSTANTIABLE) {
                let c = self.base_constraint(m);
                if c == TypeId::UNKNOWN || c == TypeId::UNRESOLVED {
                    continue;
                }
                // A union constraint (`T["states"]` constrained by `Record<string, S> |
                // undefined`) contributes by its members.
                if self.store().flags(c).contains(TypeFlags::UNION) {
                    if let Some(t) = self.contextual_property_type(c, name) {
                        out.push(t);
                    }
                    continue;
                }
                c
            } else {
                m
            };
            if let Some(p) = self.property_of_type_id(am, nid) {
                // A generic mapped type's property is its template for that key (`T["x"]`
                // keeps `1` a literal under `T extends { x: number }`), not its constraint's.
                if self.is_generic_mapped_type(m)
                    && let Some(t) = self.generic_mapped_template_for(m, key)
                {
                    out.push(t);
                } else {
                    out.push(self.type_of_symbol(p));
                }
                continue;
            }
            if let Some(t) = self.generic_mapped_template_for(am, key) {
                out.push(t);
                continue;
            }
            if let Some(shape) = self.store().tuple_shape_of(am)
                && shape.has_rest()
                && crate::expr::is_numeric_name(name)
                && let Some(e) = self.element_type_of_array_like(am)
            {
                out.push(e);
                continue;
            }
            let numeric = crate::expr::is_numeric_name(name);
            let info = if numeric {
                self.index_info_of(am, TypeId::NUMBER)
            } else if is_symbol {
                self.index_info_of(am, TypeId::SYMBOL)
            } else {
                self.index_info_of(am, TypeId::STRING)
            };
            if let Some(i) = info {
                out.push(i.value);
            }
        }
        if out.is_empty() {
            return None;
        }
        Some(self.contextual_union(out))
    }

    /// The union of per-member contextual types. An `any` member does not absorb the others
    /// (tsc keeps such a union unreduced): `"a"` stays a literal against `["a"] | { [i:
    /// number]: any }`. Nor does an `unknown` member, which adds nothing to the context: `K
    /// | unknown` from `[K, number] | [unknown, string]` stays generic.
    fn contextual_union(&mut self, mut out: Vec<TypeId>) -> TypeId {
        if out.contains(&TypeId::ANY) && out.iter().any(|&t| t != TypeId::ANY) {
            out.retain(|&t| t != TypeId::ANY);
        }
        if out.contains(&TypeId::UNKNOWN) && out.iter().any(|&t| t != TypeId::UNKNOWN) {
            out.retain(|&t| t != TypeId::UNKNOWN);
        }
        self.union_with(&out, Reduction::None, AliasId::NONE)
    }

    pub(crate) fn contextual_element_type(
        &mut self,
        ct: TypeId,
        idx: usize,
        after_spread: bool,
        from_end: Option<usize>,
    ) -> Option<TypeId> {
        let members = self.union_members(ct);
        let mut out = Vec::new();
        for m in members {
            let mf = self.store().flags(m);
            if mf.intersects(TypeFlags::ANY) {
                return None;
            }
            let am = if mf.intersects(TypeFlags::INSTANTIABLE) {
                let c = self.base_constraint(m);
                if c == TypeId::UNKNOWN || c == TypeId::UNRESOLVED {
                    continue;
                }
                c
            } else {
                m
            };
            if let Some(shape) = self.store().tuple_shape_of(am) {
                let shape = shape.clone();
                let args = self.c.type_args_of_tuple(am).to_vec();
                // Leading fixed elements by index, trailing ones (after a rest, or after
                // the literal's spreads) from the end.
                let n = shape.elements.len();
                let first_var = shape
                    .elements
                    .iter()
                    .position(|e| e.kind == TupleElement::REST || e.kind == TupleElement::VARIADIC);
                if !after_spread && idx < first_var.unwrap_or(n) {
                    out.push(args.get(idx).copied().unwrap_or(TypeId::UNRESOLVED));
                    continue;
                }
                if let Some(k) = from_end
                    && (after_spread || first_var.is_some())
                {
                    let var = shape.elements.iter().rposition(|e| {
                        e.kind == TupleElement::REST || e.kind == TupleElement::VARIADIC
                    });
                    let trailing = var.map_or(n, |v| n - v - 1);
                    if k <= trailing && shape.elements[n - k].kind == TupleElement::REQUIRED {
                        out.push(args.get(n - k).copied().unwrap_or(TypeId::UNRESOLVED));
                        continue;
                    }
                }
                if let Some(ri) = shape
                    .elements
                    .iter()
                    .position(|e| e.kind == TupleElement::REST)
                {
                    out.push(args.get(ri).copied().unwrap_or(TypeId::UNRESOLVED));
                }
                continue;
            }
            if let Some(i) = self.index_info_of(am, TypeId::NUMBER) {
                out.push(i.value);
                continue;
            }
            if let Some(it) = self.quietly(|ck| ck.iterated_type(am, false))
                && it != TypeId::UNRESOLVED
            {
                out.push(it);
            }
        }
        if out.is_empty() {
            return None;
        }
        Some(self.contextual_union(out))
    }

    /// The contextual type of argument `idx` of a call: the parameter type of the resolved
    /// signature.
    fn contextual_arg_type(&mut self, call: Loc, idx: usize, arg: Loc) -> Option<TypeId> {
        if self.circular_iife_args.contains(&arg) {
            return None;
        }
        // An immediately invoked function's parameter typed by this argument is being
        // resolved: the argument has no context, then or later.
        if self.iife_args.contains(&arg) {
            self.circular_iife_args.insert(arg);
            return None;
        }
        let sig = match self.resolved_calls.get(&call) {
            Some(s) => *s,
            None => {
                if self.is_resolving(crate::checker::Resolving::Node(call)) {
                    return None;
                }
                self.quietly(|ck| ck.check_expr(call));
                self.resolved_calls.get(&call).copied().flatten()
            }
        }?;
        if let Some(t) = self.first_context_of(arg, sig) {
            return Some(t);
        }
        self.param_type_at(sig, idx)
    }

    /// The function-like node containing `l` (not crossing class bodies).
    pub fn containing_function(&self, l: Loc) -> Option<Loc> {
        let ast = self.ast(l.file);
        let parents = self.c.file(l.file).parents();
        let mut cur = parents.parent(l.node);
        while let Some(n) = cur {
            match ast.node(n) {
                Node::FunctionDecl { .. } | Node::FunctionExpr { .. } | Node::Arrow { .. } => {
                    return Some(Loc::new(l.file, n));
                }
                Node::MethodDef { .. }
                | Node::ClassDecl { .. }
                | Node::ClassExpr { .. }
                | Node::StaticBlock { .. } => return None,
                Node::PropertyDef { .. } => return None,
                _ => {}
            }
            cur = parents.parent(n);
        }
        None
    }

    /// The contextual return type for `return` expressions of a function: its annotation,
    /// else its contextual signature's return type.
    pub fn contextual_return_type(&mut self, func: Loc) -> Option<TypeId> {
        let ast = self.ast(func.file);
        let parts = fn_parts(ast, func.node)?;
        let is_async = parts.flags.contains(FnFlags::ASYNC);
        if parts.flags.contains(FnFlags::GENERATOR) {
            // An async generator's `return` is awaited like an async function's.
            let r = self.generator_context(func).map(|(_, r)| r)?;
            return Some(if is_async && r != TypeId::UNRESOLVED {
                self.async_return_context(r)
            } else {
                r
            });
        }
        if let Some(rt) = parts.return_type {
            let t = self.type_from_node(Loc::new(func.file, rt));
            if let Node::TypePredicate { .. } = ast.node(rt) {
                return Some(TypeId::BOOLEAN);
            }
            return Some(if is_async {
                self.async_return_context(t)
            } else {
                t
            });
        }
        // A getter returns its setter's parameter type; constructors and other accessors
        // have no contextual return type. An immediately invoked function returns into its
        // call's context (`const x: T = (() => v)()`).
        if let Some(t) = self.unannotated_getter_setter_type(func) {
            return Some(t);
        }
        let r = match self.iife_call(func) {
            Some(call) => self.contextual_type(call)?,
            None => {
                let sig = self.contextual_signature(func)?;
                let r = self.return_type_of(sig);
                self.instantiate_with_return_context(r)
            }
        };
        Some(if is_async {
            // An async function always returns a promise, so contextual members that can't
            // be awaited into a value (`string`, `{ a: string }`) give no context:
            // `() => Promise<X> | SM<X>` contextually types `return mk()` by `X` only.
            let r = self.map_type(r, |ck, m| {
                let f = ck.store().flags(m);
                if f.intersects(
                    TypeFlags::ANY_OR_UNKNOWN | TypeFlags::VOID | TypeFlags::INSTANTIABLE,
                ) || m == TypeId::UNRESOLVED
                    || ck.store().is_error(m)
                {
                    return m;
                }
                if f.intersects(TypeFlags::PRIMITIVE) {
                    return TypeId::NEVER;
                }
                if ck.awaited_type(m) == m {
                    return TypeId::NEVER;
                }
                m
            });
            self.async_return_context(r)
        } else {
            r
        })
    }

    /// The call that immediately invokes the function expression or arrow `f` (through
    /// parentheses), if any.
    pub(crate) fn iife_call(&self, f: Loc) -> Option<Loc> {
        let ast = self.ast(f.file);
        if !matches!(
            ast.node(f.node),
            Node::FunctionExpr { .. } | Node::Arrow { .. }
        ) {
            return None;
        }
        let mut cur = f;
        for _ in 0..64 {
            let p = self.c.parent(cur)?;
            match *ast.node(p.node) {
                Node::Paren { .. } => cur = p,
                Node::Call { callee, .. } if callee == cur.node => return Some(p),
                _ => return None,
            }
        }
        None
    }

    /// The contextual yield and return types of a generator, from its annotated or
    /// contextual return type taken member by member: `number | Generator<Y, R>` gives `Y`
    /// and `R`; members that aren't iterators give nothing.
    pub(crate) fn generator_context(&mut self, func: Loc) -> Option<(TypeId, TypeId)> {
        self.generator_context_full(func).map(|(y, r, _)| (y, r))
    }

    /// [`Self::generator_context`] with the next type (`UNRESOLVED` when no member has
    /// one).
    pub(crate) fn generator_context_full(&mut self, func: Loc) -> Option<(TypeId, TypeId, TypeId)> {
        let ast = self.ast(func.file);
        let parts = fn_parts(ast, func.node)?;
        let rt = match parts.return_type {
            Some(rt) => self.type_from_node(Loc::new(func.file, rt)),
            // An immediately invoked generator yields into its call's context
            // (`yield* function* () { ... }()`).
            None => match self.iife_call(func) {
                Some(call) => self.contextual_type(call)?,
                None => {
                    let sig = self.contextual_signature(func)?;
                    self.return_type_of(sig)
                }
            },
        };
        use crate::checker::GlobalType as G;
        let mut iters = Vec::new();
        for g in [
            G::Generator,
            G::AsyncGenerator,
            G::Iterator,
            G::AsyncIterator,
            G::IterableIterator,
            G::AsyncIterableIterator,
            G::Iterable,
            G::AsyncIterable,
            G::IteratorObject,
        ] {
            if let Some(t) = self.global_type(g) {
                iters.push(t);
            }
        }
        let (mut ys, mut rs, mut ns) = (Vec::new(), Vec::new(), Vec::new());
        for m in self.union_members(rt) {
            let f = self.store().flags(m);
            if f.intersects(TypeFlags::ANY) {
                return None;
            }
            let Some((target, args)) = self.generic_args(m) else {
                continue;
            };
            if !iters.contains(&target) {
                continue;
            }
            if let Some(&y) = args.first() {
                ys.push(y);
            }
            if let Some(&r) = args.get(1) {
                rs.push(r);
            }
            if let Some(&n) = args.get(2) {
                ns.push(n);
            }
        }
        if ys.is_empty() {
            return None;
        }
        let y = self.union_with(&ys, Reduction::None, AliasId::NONE);
        let r = if rs.is_empty() {
            TypeId::UNRESOLVED
        } else {
            let r = self.union_with(&rs, Reduction::None, AliasId::NONE);
            // A return type that is a type parameter of a call being inferred reads
            // through that call's contextual type, as for other functions.
            if parts.return_type.is_none() {
                self.instantiate_with_return_context(r)
            } else {
                r
            }
        };
        let n = if ns.is_empty() {
            TypeId::UNRESOLVED
        } else {
            self.intersection(&ns)
        };
        Some((y, r, n))
    }

    /// The contextual type of an async function's return expression: the awaited return
    /// type or a promise-like of it (`return Promise.resolve({ m: 'PUT' })` keeps `'PUT'`).
    fn async_return_context(&mut self, r: TypeId) -> TypeId {
        let a = self.awaited_type(r);
        let s = self.store().flags(a);
        // (`void` too: `return await race(new Promise(r => r()))` in a `Promise<void>`
        // function infers `void` through `PromiseLike<void>`.)
        if s.intersects(TypeFlags::ANY_OR_UNKNOWN | TypeFlags::NEVER)
            || a == TypeId::UNRESOLVED
            || self.store().is_error(a)
        {
            return a;
        }
        match self.global_reference(crate::checker::GlobalType::PromiseLike, &[a]) {
            Some(p) => self.union2(a, p),
            None => a,
        }
    }

    /// Whether a function expression has parameters whose types come from context.
    pub fn is_context_sensitive_function(&self, f: Loc) -> bool {
        let ast = self.ast(f.file);
        let Some(parts) = fn_parts(ast, f.node) else {
            return false;
        };
        if matches!(ast.node(f.node), Node::FunctionDecl { .. }) {
            return false;
        }
        if parts.type_params.is_some() {
            return false;
        }
        ast.list(parts.params).iter().any(|&p| {
            matches!(*ast.node(p), Node::Param { type_ann: None, .. }) && !is_this_param(ast, p)
        })
    }

    /// The contextual signature of a function expression, arrow or object literal method.
    pub fn contextual_signature(&mut self, f: Loc) -> Option<SigId> {
        let ast = self.ast(f.file);
        // JavaScript: `/** @type {F} */ function f() {}` is typed by `F`'s signature.
        if let Some(ty) = self.jsdoc_function_type_tag(f) {
            let ct = self.type_from_node(Loc::new(f.file, ty));
            return self.signature_of_contextual_type(ct, f);
        }
        if !matches!(
            ast.node(f.node),
            Node::FunctionExpr { .. } | Node::Arrow { .. }
        ) {
            return None;
        }
        // Methods of classes are not contextually typed.
        if let Some(p) = self.c.parent(f)
            && let Node::MethodDef { .. } = ast.node(p.node)
        {
            let gp = self.c.parent(p)?;
            if !matches!(ast.node(gp.node), Node::ObjectLit { .. }) {
                return None;
            }
        }
        let ct = self.contextual_type(f)?;
        self.signature_of_contextual_type(ct, f)
    }

    fn signature_of_contextual_type(&mut self, ct: TypeId, f: Loc) -> Option<SigId> {
        let members = self.union_members(ct);
        let mut found: Vec<SigId> = Vec::new();
        for m in members {
            let mf = self.store().flags(m);
            if mf.intersects(TypeFlags::NULLABLE | TypeFlags::VOID) {
                continue;
            }
            if mf.intersects(TypeFlags::ANY) {
                return None;
            }
            let sigs = self.signatures_of(m, SigKind::Call);
            if sigs.is_empty() {
                continue;
            }
            // A member whose overloads give no one signature gives no context; the other
            // members still do (`{ apply: (c) => {} }` against `I | ((c: C) => void)`:
            // the function's `apply` overloads are generic).
            if sigs.len() > 1 {
                if let Some(s) = self.intersected_contextual_signature(&sigs, f) {
                    found.push(s);
                }
                continue;
            }
            // A signature with fewer parameters than the function requires gives no
            // context (observed: `const f: (a: string) => void = (x, y) => {}` leaves both
            // implicitly `any`).
            let d = self.store().sig(sigs[0]);
            if !d.has_rest() && d.params.len() < self.required_param_count(f) {
                continue;
            }
            found.push(sigs[0]);
        }
        match found.len() {
            0 => None,
            1 => Some(found[0]),
            _ => {
                // Signatures from a union give context only when their parameters agree:
                // identical types, optionality and rest-ness where both have a parameter
                // (one may have fewer), and no type parameters. The longest one is used.
                let mut best = found[0];
                for &s in &found[1..] {
                    if s == best {
                        continue;
                    }
                    if !self.contextual_params_agree(best, s) {
                        return None;
                    }
                    if self.store().sig(s).params.len() > self.store().sig(best).params.len() {
                        best = s;
                    }
                }
                Some(best)
            }
        }
    }

    /// The contextual signature of a type with several call signatures (overloads, or an
    /// intersection of function types such as the callback of a method of `A[] | B[]`):
    /// those taking at least the function's required parameters; when several remain,
    /// under `noImplicitAny`, one signature whose parameters are the unions of theirs
    /// (`((v: string) => void) & ((v: number) => void)` gives `v: string | number`).
    /// Generic or rest signatures give no context.
    /// The number of leading parameters of function `f` without `?`, a default or `...`
    /// (not counting `this`).
    fn required_param_count(&self, f: Loc) -> usize {
        let ast = self.ast(f.file);
        let Some(parts) = fn_parts(ast, f.node) else {
            return 0;
        };
        let mut required = 0usize;
        for (i, &p) in ast.list(parts.params).iter().enumerate() {
            if i == 0 && is_this_param(ast, p) {
                continue;
            }
            match *ast.node(p) {
                Node::Param {
                    optional: false,
                    rest: false,
                    init: None,
                    ..
                } => required += 1,
                _ => break,
            }
        }
        required
    }

    fn intersected_contextual_signature(&mut self, sigs: &[SigId], f: Loc) -> Option<SigId> {
        fn_parts(self.ast(f.file), f.node)?;
        let required = self.required_param_count(f);
        let applicable: Vec<SigId> = sigs
            .iter()
            .copied()
            .filter(|&s| {
                let d = self.store().sig(s);
                d.has_rest() || d.params.len() >= required
            })
            .collect();
        match applicable.len() {
            0 => return None,
            1 => return Some(applicable[0]),
            _ => {}
        }
        if !self.c.opts.no_implicit_any {
            return None;
        }
        let mut acc = applicable[0];
        for &s in &applicable[1..] {
            if s == acc {
                continue;
            }
            acc = self.combine_intersection_signatures(acc, s)?;
        }
        Some(acc)
    }

    /// One contextual signature for two overloads (`(a: number) => void & (b: string) =>
    /// void` types `(arg)` as `string | number`): per position the union of the
    /// parameter types of the signatures that have it, rest elements included; a rest
    /// parameter when either has one (`log(...data: any[])` with `log(message?: any,
    /// ...rest: any[])` is `(message?: any, ...rest: any[])`). An empty tuple rest
    /// (`...args: []`) is no parameter. Generic signatures and tuple rests do not combine.
    fn combine_intersection_signatures(&mut self, a: SigId, b: SigId) -> Option<SigId> {
        let (da, db) = (self.store().sig(a), self.store().sig(b));
        if da.type_params != ts_r_types::ListId::EMPTY
            || db.type_params != ts_r_types::ListId::EMPTY
        {
            return None;
        }
        let ma = da.min_args;
        let mb = db.min_args;
        let (ta, tb) = (da.this_type, db.this_type);
        let (fa, ra) = self.combinable_params(a)?;
        let (fb, rb) = self.combinable_params(b)?;
        let (long, short) = if fa.len() >= fb.len() {
            ((&fa, ra), (&fb, rb))
        } else {
            ((&fb, rb), (&fa, ra))
        };
        let min_args = ma.min(mb);
        let this_type = match (ta, tb) {
            (Some(x), Some(y)) => Some(self.union(&[x, y])),
            (x, y) => x.or(y),
        };
        let mut params = Vec::with_capacity(long.0.len() + 1);
        for (i, p) in long.0.iter().enumerate() {
            let other = match short.0.get(i) {
                Some(q) => Some(q.ty),
                None => short.1.map(|r| r.1),
            };
            let ty = match other {
                Some(o) => self.union(&[p.ty, o]),
                None => p.ty,
            };
            params.push(ts_r_types::Param {
                name: p.name,
                ty,
                optional: (i as u32) >= min_args,
                sym: p.sym,
            });
        }
        let mut flags = SigFlags::NONE;
        let rest = match (long.1, short.1) {
            (Some(x), Some(y)) => Some((x.0, self.union(&[x.1, y.1]))),
            (x, y) => x.or(y),
        };
        if let Some((p, el)) = rest {
            let ty = self.create_array(el, false);
            params.push(ts_r_types::Param {
                name: p.name,
                ty,
                optional: false,
                sym: p.sym,
            });
            flags = SigFlags::HAS_REST;
        }
        let ra = self.return_type_of(a);
        let rb = self.return_type_of(b);
        let ret = self.intersection(&[ra, rb]);
        let decl = if fa.len() >= fb.len() {
            self.store().sig(a).decl
        } else {
            self.store().sig(b).decl
        };
        let sig = ts_r_types::Signature::new(
            flags,
            decl,
            ts_r_types::ListId::EMPTY,
            params,
            this_type,
            min_args,
            Some(ret),
        );
        Some(self.store().add_signature(sig))
    }

    /// A signature's fixed parameters and its rest parameter with the rest's element
    /// type, for [`Self::combine_intersection_signatures`]; `None` for a tuple rest other
    /// than `[]`.
    fn combinable_params(&mut self, s: SigId) -> Option<CombinableParams> {
        let d = self.store().sig(s);
        let mut params = d.params.to_vec();
        if !d.has_rest() {
            return Some((params, None));
        }
        let rest = params.pop()?;
        if let Some(shape) = self.store().tuple_shape_of(rest.ty) {
            return shape.elements.is_empty().then_some((params, None));
        }
        let el = self.element_type_of_array_like(rest.ty)?;
        Some((params, Some((rest, el))))
    }

    fn contextual_params_agree(&mut self, a: SigId, b: SigId) -> bool {
        let (da, db) = (self.store().sig(a), self.store().sig(b));
        if da.type_params != ts_r_types::ListId::EMPTY
            || db.type_params != ts_r_types::ListId::EMPTY
        {
            return false;
        }
        let n = da.params.len().min(db.params.len());
        let (ra, rb) = (da.has_rest(), db.has_rest());
        let (la, lb) = (da.params.len(), db.params.len());
        let pairs: Vec<(TypeId, TypeId, bool, bool, bool, bool)> = (0..n)
            .map(|i| {
                (
                    da.params[i].ty,
                    db.params[i].ty,
                    (i as u32) >= da.min_args,
                    (i as u32) >= db.min_args,
                    ra && i + 1 == la,
                    rb && i + 1 == lb,
                )
            })
            .collect();
        pairs
            .into_iter()
            .all(|(ta, tb, oa, ob, xa, xb)| oa == ob && xa == xb && self.is_identical(ta, tb))
    }

    /// The contextual type of an unannotated parameter (assigned when its function
    /// expression is checked, else computed from the contextual signature).
    pub fn contextual_param_type(&mut self, p: Loc) -> Option<TypeId> {
        if let Some(&t) = self.param_types.get(&p) {
            return Some(t);
        }
        let ast = self.ast(p.file);
        let Node::Param { type_ann: None, .. } = *ast.node(p.node) else {
            return None;
        };
        let func = self.c.parent(p)?;
        // (A JavaScript function declaration is typed by its JSDoc `@type`.)
        let typed = match ast.node(func.node) {
            Node::FunctionExpr { .. } | Node::Arrow { .. } => true,
            Node::FunctionDecl { .. } => self.jsdoc_declared_signature(func).is_some(),
            _ => false,
        };
        if !typed {
            return None;
        }
        let parts = fn_parts(ast, func.node)?;
        let params: Vec<NodeId> = ast
            .list(parts.params)
            .iter()
            .copied()
            .filter(|&x| !is_this_param(ast, x))
            .collect();
        let idx = params.iter().position(|&x| x == p.node)?;
        let t = match self.contextual_signature(func) {
            Some(sig) => self.param_type_from_contextual(sig, idx, p)?,
            None => self.iife_arg_type(func, idx, p)?,
        };
        if self.speculative == 0 && self.context_overrides.is_empty() && self.flow_loop == 0 {
            self.param_types.insert(p, t);
        }
        Some(t)
    }

    /// A parameter of an immediately invoked function expression is typed by its argument
    /// (`(({ kind, ...rest }) => rest)(t)`).
    /// A rest parameter takes the remaining arguments as an array; a parameter past the
    /// arguments is `undefined`.
    fn iife_arg_type(&mut self, func: Loc, idx: usize, p: Loc) -> Option<TypeId> {
        let ast = self.ast(func.file);
        let is_rest = matches!(*ast.node(p.node), Node::Param { rest: true, .. });
        let call = self.iife_call(func)?;
        let Node::Call { args, .. } = *ast.node(call.node) else {
            return None;
        };
        let list = ast.list(args).to_vec();
        let upto = if is_rest {
            list.len()
        } else {
            list.len().min(idx + 1)
        };
        let has_spread = list[..upto]
            .iter()
            .any(|&a| matches!(ast.node(a), Node::Spread { .. }));
        let arg_type = |ck: &mut Self, a: NodeId| {
            let a = Loc::new(func.file, a);
            ck.iife_args.push(a);
            let t = ck.quietly(|ck| ck.check_expr(a));
            ck.iife_args.pop();
            let t = ck.widen_literal(t);
            ck.widened_type(t)
        };
        if has_spread {
            return self.iife_spread_arg_type(func, &list, idx, p, is_rest);
        }
        if is_rest {
            let rest: Vec<NodeId> = list.get(idx..).map(|r| r.to_vec()).unwrap_or_default();
            let types: Vec<TypeId> = rest.into_iter().map(|a| arg_type(self, a)).collect();
            let elem = self.union(&types);
            return Some(self.create_array(elem, false));
        }
        let Node::Param {
            optional,
            init,
            pattern,
            ..
        } = *ast.node(p.node)
        else {
            return None;
        };
        match list.get(idx) {
            Some(&a) => {
                let t = arg_type(self, a);
                Some(if optional { self.add_optionality(t) } else { t })
            }
            // A missing argument: `undefined` (`any` without strictNullChecks), unless an
            // initializer or a binding pattern types the parameter.
            None if init.is_none() && matches!(ast.node(pattern), Node::Ident { .. }) => {
                Some(if self.c.opts.strict_null_checks {
                    TypeId::UNDEFINED
                } else {
                    TypeId::ANY
                })
            }
            None => None,
        }
    }

    /// [`Self::iife_arg_type`] with spread arguments. Each argument fills one parameter: a
    /// spread tuple its elements, any other spread (an array, `any`) one parameter of its
    /// element type. A rest parameter takes the remaining arguments as a tuple (array
    /// spreads as `...T[]`), or the last argument's array when it is a spread before it
    /// (observed: `((a, b) => {})(...arr)` gives `number` and `undefined`,
    /// `((a, ...r) => {})(true, "s", ...arr)` gives `r: [string, ...number[]]`).
    fn iife_spread_arg_type(
        &mut self,
        func: Loc,
        list: &[NodeId],
        idx: usize,
        p: Loc,
        is_rest: bool,
    ) -> Option<TypeId> {
        let ast = self.ast(func.file);
        let widen = |ck: &mut Self, t: TypeId| {
            let t = ck.widen_literal(t);
            ck.widened_type(t)
        };
        // (type, is an array spread: `ty` is the array)
        let mut slots: Vec<(TypeId, bool)> = Vec::new();
        let mut last_array: Option<TypeId> = None;
        for &a in list {
            if !is_rest && slots.len() > idx {
                break;
            }
            last_array = None;
            let Node::Spread { arg } = *ast.node(a) else {
                let t = self.quietly(|ck| ck.check_expr(Loc::new(func.file, a)));
                slots.push((widen(self, t), false));
                continue;
            };
            let t = self.quietly(|ck| ck.check_expr(Loc::new(func.file, arg)));
            if self.store().flags(t).intersects(TypeFlags::ANY) {
                slots.push((t, false));
            } else if let Some(shape) = self.store().tuple_shape_of(t).cloned() {
                // Required elements fill one parameter each; a trailing rest element is an
                // array spread (`[number, ...string[]]`).
                let n = shape.elements.len();
                for (i, el) in shape.elements.iter().enumerate() {
                    let ok = el.kind == ts_r_types::TupleElement::REQUIRED
                        || (el.kind == ts_r_types::TupleElement::REST && i + 1 == n);
                    if !ok {
                        return None;
                    }
                }
                let args = self.c.type_args_of_tuple(t).to_vec();
                for (i, el) in shape.elements.iter().enumerate() {
                    let e = args.get(i).copied().unwrap_or(TypeId::UNRESOLVED);
                    if el.kind == ts_r_types::TupleElement::REST {
                        let arr = self.create_array(e, false);
                        let arr = widen(self, arr);
                        slots.push((arr, true));
                        last_array = Some(arr);
                    } else {
                        slots.push((widen(self, e), false));
                    }
                }
            } else if self.element_type_of_array_like(t).is_some() {
                let t = widen(self, t);
                slots.push((t, true));
                last_array = Some(t);
            } else if let Some(e) = self.iterated_type(t, false) {
                // Another iterable spreads as an array of its elements (`...Iterable<number>`
                // gives `number[]`).
                let arr = self.create_array(e, false);
                let arr = widen(self, arr);
                slots.push((arr, true));
                last_array = Some(arr);
            } else {
                return None;
            }
        }
        if is_rest {
            let rest = slots.get(idx..).unwrap_or_default().to_vec();
            if rest.is_empty()
                && let Some(arr) = last_array
            {
                return Some(arr);
            }
            // `[...T[]]` is `T[]`.
            if let [(arr, true)] = rest[..] {
                return Some(arr);
            }
            let elems: Vec<TupleEl> = rest
                .into_iter()
                .map(|(ty, spread)| TupleEl {
                    ty,
                    kind: if spread {
                        ts_r_types::TupleElement::VARIADIC
                    } else {
                        ts_r_types::TupleElement::REQUIRED
                    },
                    label: None,
                })
                .collect();
            return Some(self.create_tuple(&elems, false));
        }
        let Node::Param {
            optional,
            init,
            pattern,
            ..
        } = *ast.node(p.node)
        else {
            return None;
        };
        match slots.get(idx).copied() {
            Some((t, spread)) => {
                let t = if spread {
                    self.element_type_of_array_like(t)?
                } else {
                    t
                };
                Some(if optional { self.add_optionality(t) } else { t })
            }
            None if init.is_none() && matches!(ast.node(pattern), Node::Ident { .. }) => {
                Some(if self.c.opts.strict_null_checks {
                    TypeId::UNDEFINED
                } else {
                    TypeId::ANY
                })
            }
            None => None,
        }
    }

    /// Whether the reference `e` (`x.a.b`, `x.f()`) starts at an unannotated parameter of a
    /// function expression in an unresolved context (whose `any` tsc might have typed).
    fn rooted_in_unresolved_param(&mut self, e: Loc) -> bool {
        let ast = self.ast(e.file);
        let mut n = e.node;
        for _ in 0..64 {
            match *ast.node(n) {
                Node::Member { obj, .. } | Node::Index { obj, .. } => n = obj,
                Node::Call { callee, .. } => n = callee,
                Node::Paren { expr } | Node::NonNull { expr } => n = expr,
                Node::Ident { .. } => break,
                _ => return false,
            }
        }
        let Some(s) = self
            .c
            .resolve_ident(Loc::new(e.file, n), ts_r_binder::meaning::VALUE)
        else {
            return false;
        };
        let Some(d) = self.c.first_decl(s) else {
            return false;
        };
        let ast = self.ast(d.file);
        let param = if matches!(ast.node(d.node), Node::Param { .. }) {
            d
        } else {
            match self.c.parent(d) {
                Some(p) if matches!(ast.node(p.node), Node::Param { .. }) => p,
                _ => return false,
            }
        };
        let Node::Param { type_ann: None, .. } = *ast.node(param.node) else {
            return false;
        };
        let Some(func) = self.c.parent(param) else {
            return false;
        };
        if !matches!(
            ast.node(func.node),
            Node::FunctionExpr { .. } | Node::Arrow { .. }
        ) || self.contextual_param_type(param).is_some()
        {
            return false;
        }
        self.depth += 1;
        let r = self.depth < 300 && self.in_unresolved_context(func);
        self.depth -= 1;
        r
    }

    /// Whether the function expression `f` sits where our checker could not compute a
    /// contextual type (an argument of a call whose callee is unresolved, or a raw
    /// contextual type that is unresolved). Implicit-any errors are not reported there:
    /// tsc might well have contextually typed the parameters.
    pub fn in_unresolved_context(&mut self, f: Loc) -> bool {
        if self.depth > 300 {
            return true;
        }
        self.depth += 1;
        let raw = self.contextual_type_inner(f);
        self.depth -= 1;
        if matches!(raw, Some(t) if t == TypeId::UNRESOLVED) {
            return true;
        }
        if raw.is_some() {
            return false;
        }
        // Walk up through parentheses to a call argument position.
        let mut cur = f;
        for _ in 0..64 {
            let Some(p) = self.c.parent(cur) else {
                return false;
            };
            let ast = self.ast(p.file);
            match *ast.node(p.node) {
                Node::Paren { .. } | Node::ArrayLit { .. } => cur = p,
                // Contextual types flow through conditional branches and logical operands.
                Node::Conditional { cons, alt, .. } if cons == cur.node || alt == cur.node => {
                    cur = p
                }
                Node::Binary { op, right, .. }
                    if matches!(
                        op,
                        ts_r_ast::BinaryOp::LogicalOr | ts_r_ast::BinaryOp::Coalesce
                    ) || (op == ts_r_ast::BinaryOp::LogicalAnd && right == cur.node) =>
                {
                    cur = p
                }
                // A property of an object literal whose context is a mapped type we could not
                // instantiate (reverse mapped inference is not done yet).
                Node::Property { value, .. } if value == cur.node => {
                    let Some(obj) = self.c.parent(p) else {
                        return false;
                    };
                    // No context for the object: whatever its own position says.
                    let Some(ct) = self.contextual_type_inner(obj) else {
                        cur = obj;
                        continue;
                    };
                    if ct == TypeId::UNRESOLVED {
                        return true;
                    }
                    if let Some(info) = self.mapped_info(ct) {
                        let c = self.mapped_constraint(&info);
                        return c == TypeId::UNRESOLVED
                            || self.some_member(c, |m| m == TypeId::UNRESOLVED)
                            || self.type_mentions(c, TypeId::UNRESOLVED);
                    }
                    return false;
                }
                // JSX attribute types are not computed yet, except where they are `any`.
                Node::JsxExprContainer { .. } => {
                    if let Some(attr) = self.c.parent(p)
                        && self.jsx_context_modelled(attr)
                    {
                        return false;
                    }
                    return !self.jsx_attrs_are_any(p);
                }
                Node::JsxAttr { .. } | Node::JsxSpreadAttr { .. } => {
                    return true;
                }
                Node::Call { callee, args, .. } | Node::New { callee, args, .. }
                    if ast.list(args).contains(&cur.node) =>
                {
                    let ct = self.quietly(|ck| ck.check_expr(Loc::new(p.file, callee)));
                    return ct == TypeId::UNRESOLVED
                        || (self.store().flags(ct).intersects(TypeFlags::ANY)
                            && self.rooted_in_unresolved_param(Loc::new(p.file, callee)));
                }
                _ => return false,
            }
        }
        false
    }

    /// Whether the attributes of the element around the JSX attribute value `container` are
    /// `any`: an intrinsic tag without a global `JSX` namespace, or a component tag whose
    /// type is `any` (an unresolved import). tsc reports implicit-any parameters there.
    fn jsx_attrs_are_any(&mut self, container: Loc) -> bool {
        let Some(attr) = self.c.parent(container) else {
            return false;
        };
        let Some(open) = self.c.parent(attr) else {
            return false;
        };
        // Only modelled for programs without `jsx` (where React's types are usually absent
        // too); with `jsx` set the JSX namespace may come from the factory's namespace.
        if self.c.opts.jsx_set {
            return false;
        }
        let ast = self.ast(open.file);
        // A child expression: without a global `JSX` namespace there is no children
        // attribute, so nothing types it.
        if matches!(ast.node(attr.node), Node::JsxElement { .. }) {
            return !self.has_global_jsx_intrinsics();
        }
        if !matches!(ast.node(attr.node), Node::JsxAttr { .. }) {
            return false;
        }
        let Node::JsxOpening { name, .. } = *ast.node(open.node) else {
            return false;
        };
        if crate::check::is_intrinsic_tag(ast, name) {
            return !self.has_global_jsx_intrinsics();
        }
        if !matches!(ast.node(name), Node::Ident { .. }) {
            return false;
        }
        let is_any = |ck: &mut Self, t: TypeId| {
            t != TypeId::UNRESOLVED && ck.store().flags(t).intersects(TypeFlags::ANY)
        };
        let t = self.quietly(|ck| ck.check_expr(Loc::new(open.file, name)));
        if is_any(self, t) {
            return true;
        }
        // A function component whose props parameter is `any` (an unresolved props type).
        let sigs = self.signatures_of(t, crate::members::SigKind::Call);
        if let [s] = sigs[..]
            && self.store().sig(s).type_params == ts_r_types::ListId::EMPTY
            && let Some(p) = self.store().sig(s).params.first().map(|p| p.ty)
        {
            return is_any(self, p);
        }
        false
    }

    fn param_type_from_contextual(&mut self, sig: SigId, idx: usize, p: Loc) -> Option<TypeId> {
        let ast = self.ast(p.file);
        let Node::Param { rest, optional, .. } = *ast.node(p.node) else {
            return None;
        };
        if rest {
            let (count, has_rest) = self.param_count(sig);
            if has_rest && idx >= count {
                let d = self.store().sig(sig);
                return d.params.last().map(|x| x.ty);
            }
            let mut els = Vec::new();
            // Labelled with the contextual parameters' names (`[value: X, index: number]`).
            for i in idx..count {
                let t = self.param_type_at(sig, i).unwrap_or(TypeId::ANY);
                let (opt, label) = self
                    .store()
                    .sig(sig)
                    .params
                    .get(i)
                    .map_or((false, None), |x| (x.optional, Some(x.name)));
                els.push(TupleEl {
                    ty: t,
                    kind: if opt {
                        TupleElement::OPTIONAL
                    } else {
                        TupleElement::REQUIRED
                    },
                    label,
                });
            }
            if has_rest && let Some(last) = self.store().sig(sig).params.last() {
                els.push(TupleEl {
                    ty: last.ty,
                    kind: TupleElement::REST,
                    label: Some(last.name),
                });
            }
            return Some(self.create_tuple(&els, false));
        }
        let t = self.param_type_at(sig, idx)?;
        // A binding pattern contextually typed by `unknown` takes the pattern's own type
        // (`p.then(({ data }) => ...)` on a `Promise<unknown>` then fails the argument
        // check, as in tsc).
        if t == TypeId::UNKNOWN
            && let Node::Param { pattern, .. } = *ast.node(p.node)
            && matches!(
                ast.node(pattern),
                Node::ObjectPattern { .. } | Node::ArrayPattern { .. }
            )
            && let Some(func) = self.c.parent(p)
            && self.unknown_param_is_declared(func, idx)
        {
            return Some(self.type_from_binding_pattern(Loc::new(p.file, pattern)));
        }
        if optional {
            return Some(self.add_optionality(t));
        }
        Some(t)
    }

    /// Whether the `unknown` contextual type of parameter `idx` of the function expression
    /// `func` comes from a declaration (`Promise<unknown>`'s `then`), not from an inference
    /// that found no candidates (which tsc may well resolve where we don't): every signature
    /// of the call's callee types that callback parameter without its own type parameters.
    fn unknown_param_is_declared(&mut self, func: Loc, idx: usize) -> bool {
        let ast = self.ast(func.file);
        let mut cur = func;
        let mut call = None;
        for _ in 0..32 {
            let Some(parent) = self.c.parent(cur) else {
                return false;
            };
            match *ast.node(parent.node) {
                Node::Paren { .. } => cur = parent,
                Node::Call { callee, args, .. } | Node::New { callee, args, .. } => {
                    let list = ast.list(args);
                    let Some(ai) = list.iter().position(|&a| a == cur.node) else {
                        return false;
                    };
                    if list[..ai]
                        .iter()
                        .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
                    {
                        return false;
                    }
                    let kind = if matches!(ast.node(parent.node), Node::New { .. }) {
                        SigKind::Construct
                    } else {
                        SigKind::Call
                    };
                    call = Some((callee, ai, kind));
                    break;
                }
                // Annotated declarations and properties.
                Node::VarDeclarator { .. } | Node::PropertyDef { .. } => return true,
                _ => return false,
            }
        }
        let Some((callee, ai, kind)) = call else {
            return false;
        };
        let ct = self.quietly(|ck| ck.check_expr(Loc::new(func.file, callee)));
        let sigs = self.signatures_of(ct, kind);
        if sigs.is_empty() {
            return false;
        }
        for sig in sigs {
            let tps = self
                .store()
                .list_items(self.store().sig(sig).type_params)
                .to_vec();
            if tps.is_empty() {
                continue;
            }
            let Some(pt) = self.param_type_at(sig, ai) else {
                return false;
            };
            let pt = self.non_nullable_strict(pt);
            let cbs = self.signatures_of(pt, SigKind::Call);
            if cbs.len() != 1 {
                return false;
            }
            let Some(cpt) = self.param_type_at(cbs[0], idx) else {
                return false;
            };
            if tps.iter().any(|&tp| self.type_mentions(cpt, tp)) {
                return false;
            }
        }
        true
    }

    // ---------------------------------------------------------------------------------
    // Function expressions
    // ---------------------------------------------------------------------------------

    /// The type of a function expression or arrow (a fresh anonymous function type whose
    /// parameters may be contextually typed).
    pub fn check_function_expr(&mut self, e: Loc) -> TypeId {
        if !self.placeholder_fns.is_empty() && self.placeholder_fns.contains(&e) {
            return TypeId::ANY;
        }
        // Under loop assumptions the contextual parameter types may rest on pending flow
        // types (`Array.from(g).some(x => ...)` while `g`'s walk is open): nothing is kept.
        let spec = self.speculative > 0 || !self.context_overrides.is_empty() || self.flow_loop > 0;
        if !spec && let Some(&t) = self.fn_types.get(&e) {
            self.defer(e);
            return t;
        }
        let ast = self.ast(e.file);
        let Some(parts) = fn_parts(ast, e.node) else {
            return TypeId::ERROR;
        };
        let mut saved: Vec<(Loc, Option<TypeId>)> = Vec::new();
        if spec {
            self.push_assumptions();
        }
        if self.is_context_sensitive_function(e)
            && let Some(ctx) = self.contextual_signature(e)
        {
            if !spec && let Some(&t) = self.fn_types.get(&e) {
                self.defer(e);
                return t;
            }
            let params: Vec<NodeId> = ast
                .list(parts.params)
                .iter()
                .copied()
                .filter(|&x| !is_this_param(ast, x))
                .collect();
            for (i, &p) in params.iter().enumerate() {
                let pl = Loc::new(e.file, p);
                if !matches!(*ast.node(p), Node::Param { type_ann: None, .. }) {
                    continue;
                }
                if let Some(t) = self.param_type_from_contextual(ctx, i, pl) {
                    let old = self.param_types.insert(pl, t);
                    saved.push((pl, old));
                }
            }
        }
        if spec {
            self.spec_frames.push((e, Default::default()));
        }
        let sig = self.signature_from_decl(e);
        if spec {
            // Return types inferred under speculative parameter types must be computed now.
            if self.store().sig(sig).ret().is_none() {
                let _ = self.return_type_of(sig);
            }
            self.spec_frames.pop();
            for (p, old) in saved.into_iter().rev() {
                match old {
                    Some(t) => {
                        self.param_types.insert(p, t);
                    }
                    None => {
                        self.param_types.remove(&p);
                    }
                }
            }
            self.pop_assumptions();
        }
        let sym = self.c.sym_of_decl(e).unwrap_or(SymId::NONE);
        let mut traits = TypeTraits::NONE;
        if parts.type_params.is_some() || !self.outer_type_params(e).is_empty() {
            traits |= TypeTraits::CONTAINS_TYPE_PARAMETER;
        }
        let t = self.store().literal_object(
            sym,
            ObjectFlags::ANONYMOUS | ObjectFlags::FUNCTION_LIKE,
            ResolvedMembers::new(Vec::new(), vec![sig], Vec::new(), Vec::new()),
            traits,
        );
        if !spec {
            self.fn_types.insert(e, t);
            self.defer(e);
        }
        t
    }

    // ---------------------------------------------------------------------------------
    // Return type inference
    // ---------------------------------------------------------------------------------

    /// The return type of a function-like declaration without an annotation, from its body.
    pub fn infer_return_type(&mut self, decl: Loc, _sig: SigId) -> TypeId {
        let ast = self.ast(decl.file);
        let Some(parts) = fn_parts(ast, decl.node) else {
            return TypeId::ANY;
        };
        let Some(body) = parts.body else {
            return TypeId::ANY;
        };
        let is_async = parts.flags.contains(FnFlags::ASYNC);
        let fnode = match *ast.node(decl.node) {
            Node::MethodDef { value, .. } => value,
            _ => decl.node,
        };
        if matches!(
            ast.node(decl.node),
            Node::MethodDef {
                kind: MethodKind::Constructor,
                ..
            }
        ) {
            return TypeId::VOID;
        }
        // Function expressions, arrows and object literal methods (not class methods or
        // accessors): they infer `never` when they can't return, and take their context.
        let is_expr_fn = matches!(
            ast.node(fnode),
            Node::FunctionExpr { .. } | Node::Arrow { .. }
        ) && match self.c.parent(Loc::new(decl.file, fnode)) {
            Some(p) => match *self.node(p) {
                Node::MethodDef { kind, .. } => {
                    kind == MethodKind::Method
                        && self
                            .c
                            .parent(p)
                            .is_some_and(|gp| matches!(self.node(gp), Node::ObjectLit { .. }))
                }
                _ => true,
            },
            None => true,
        };
        if parts.flags.contains(FnFlags::GENERATOR) {
            return self.infer_generator_type(decl, fnode, body, is_async, is_expr_fn);
        }
        let mut types: Vec<TypeId> = Vec::new();
        let mut has_empty_return = false;
        // Returns of a bare call to the function itself (`return g()`), which add nothing.
        let mut has_self_return = false;
        let expression_body = parts.is_arrow && parts.flags.contains(FnFlags::EXPRESSION_BODY);
        let end_reachable;
        if expression_body {
            if self.is_bare_self_call(Loc::new(decl.file, fnode), body, is_async) {
                has_self_return = true;
            } else {
                let t = self.check_expr(Loc::new(decl.file, body));
                types.push(t);
            }
            end_reachable = false;
        } else {
            let mut returns = Vec::new();
            collect_returns(ast, body, &mut returns, 0);
            for r in returns {
                match *ast.node(r) {
                    Node::Return { arg: Some(a) }
                        if self.is_bare_self_call(Loc::new(decl.file, fnode), a, is_async) =>
                    {
                        has_self_return = true;
                    }
                    Node::Return { arg: Some(a) } => {
                        let t = self.check_expr(Loc::new(decl.file, a));
                        types.push(t);
                    }
                    _ => has_empty_return = true,
                }
            }
            end_reachable = match self.c.bound(decl.file).flow.function_flow(fnode) {
                None => true,
                Some(ff) => {
                    let end = ff.end;
                    !end.is_unreachable() && self.is_flow_reachable(decl.file, end)
                }
            };
        }
        if is_async {
            types = types.into_iter().map(|t| self.awaited_type(t)).collect();
        }
        let ret = if types.is_empty() {
            if (is_expr_fn || has_self_return) && !end_reachable && !has_empty_return {
                TypeId::NEVER
            } else if is_expr_fn && self.contextual_return_has_undefined(decl.file, fnode, is_async)
            {
                // Without return values, a function whose contextual return type takes
                // `undefined` returns `undefined`, not `void` (`const f: () => undefined =
                // () => {}`).
                TypeId::UNDEFINED
            } else {
                TypeId::VOID
            }
        } else {
            if (end_reachable || has_empty_return) && self.c.opts.strict_null_checks {
                types.push(TypeId::UNDEFINED);
            }
            let u = self.union_with(&types, Reduction::Subtype, AliasId::NONE);
            let ctx = if is_expr_fn {
                self.contextual_signature(Loc::new(decl.file, fnode))
                    .map(|s| self.return_type_of(s))
            } else {
                None
            };
            // Only a single unit type widens (`return 1` gives `number`); a union of
            // returns keeps its literals (`1 | "a"`, `1 | Promise<string>`).
            let unit = {
                let f = self.store().flags(u);
                f.intersects(TypeFlags::UNIT) && !f.contains(TypeFlags::UNION)
            };
            // A literal returned into a `const` type parameter's indexed access (a reverse
            // mapped `const T`'s `T[K]`) is kept.
            let u = match ctx {
                _ if !unit => u,
                Some(c) if self.is_literal_of_contextual_type(u, c) => u,
                Some(c)
                    if matches!(
                        self.store().kind(c),
                        ts_r_types::TypeKind::IndexedAccess { .. }
                    ) && self.is_const_type_variable(c, 0) =>
                {
                    u
                }
                _ => self.widen_literal(u),
            };
            self.widened_type(u)
        };
        if is_async {
            return self.promise_of(ret);
        }
        ret
    }

    /// Whether `e` (a returned expression of function `f`) is a bare call to `f` itself
    /// (`return await f()` in an async function): a function declaration by its name, a
    /// function expression by its own name or a `const` it initializes. Such returns
    /// don't contribute to the inferred return type (`function g() { return g(); }`
    /// returns `never`), as observed in tsc 7; a `var` may be reassigned, so its calls do.
    fn is_bare_self_call(&mut self, f: Loc, e: NodeId, is_async: bool) -> bool {
        let ast = self.ast(f.file);
        let mut e = crate::nodes::skip_parens(ast, e);
        if is_async && let Node::Await { arg } = *ast.node(e) {
            e = crate::nodes::skip_parens(ast, arg);
        }
        let Node::Call {
            callee,
            optional: false,
            ..
        } = *ast.node(e)
        else {
            return false;
        };
        if !matches!(ast.node(callee), Node::Ident { .. }) {
            return false;
        }
        let Some(target) = self
            .c
            .resolve_ident(Loc::new(f.file, callee), ts_r_binder::meaning::VALUE)
        else {
            return false;
        };
        match *ast.node(f.node) {
            Node::FunctionDecl { .. } => self.c.sym_of_decl(f) == Some(target),
            // The binder gives a named function expression's own symbol to the expression.
            Node::FunctionExpr { name: Some(n), .. }
                if self.c.sym_of_decl(Loc::new(f.file, n)) == Some(target)
                    || self.c.sym_of_decl(f) == Some(target) =>
            {
                true
            }
            Node::FunctionExpr { .. } | Node::Arrow { .. } => {
                let mut at = f;
                while let Some(p) = self.c.parent(at)
                    && matches!(ast.node(p.node), Node::Paren { .. })
                {
                    at = p;
                }
                let Some(p) = self.c.parent(at) else {
                    return false;
                };
                match *ast.node(p.node) {
                    Node::VarDeclarator {
                        name,
                        init: Some(i),
                        ..
                    } if i == at.node => {
                        self.c.sym_of_decl(Loc::new(f.file, name)) == Some(target)
                            && self.is_const_declaration(Loc::new(f.file, name))
                    }
                    _ => false,
                }
            }
            _ => false,
        }
    }

    /// Whether the contextual return type of the function expression `fnode` (awaited for
    /// an async one) has `undefined` among its members.
    fn contextual_return_has_undefined(
        &mut self,
        file: ts_r_diagnostics::FileId,
        fnode: NodeId,
        is_async: bool,
    ) -> bool {
        let Some(sig) = self.contextual_signature(Loc::new(file, fnode)) else {
            return false;
        };
        let r = self.return_type_of(sig);
        let r = if is_async { self.awaited_type(r) } else { r };
        self.some_member(r, |m| m == TypeId::UNDEFINED)
    }

    /// The inferred type of an unannotated generator: `Generator<Y, R, N>` (or
    /// `AsyncGenerator`), with `Y` the union of what it yields (`undefined` for a bare
    /// `yield`, the iterated type of a `yield*` operand), `R` from its returns as for other
    /// functions (`void` without any), and `N` the intersection of what it is resumed with
    /// (see `delegated_next_type`), else its context's, else `unknown`. Lone unit types
    /// widen unless the context expects literals.
    fn infer_generator_type(
        &mut self,
        decl: Loc,
        fnode: NodeId,
        body: NodeId,
        is_async: bool,
        is_expr_fn: bool,
    ) -> TypeId {
        use crate::checker::GlobalType as G;
        let ast = self.ast(decl.file);
        let floc = Loc::new(decl.file, fnode);
        let mut yields = Vec::new();
        collect_yields(ast, body, &mut yields, 0);
        // A contextually typed generator expression that yields nothing and has no
        // annotated parameter is left unresolved: its yield type's arguments would come
        // from return-type inferences (`effectFn(function* (a) {})` gives
        // `Effect<void, never, never>`), not modelled.
        if is_expr_fn
            && yields.is_empty()
            && fn_parts(ast, fnode).is_some_and(|p| {
                ast.list(p.params).iter().all(|&pn| {
                    !matches!(
                        *ast.node(pn),
                        Node::Param {
                            type_ann: Some(_),
                            ..
                        }
                    )
                })
            })
            && self.contextual_signature(floc).is_some()
        {
            return TypeId::UNRESOLVED;
        }
        let ctx = if is_expr_fn {
            self.generator_context_full(floc)
        } else {
            None
        };
        let mut ys: Vec<TypeId> = Vec::new();
        // What the generator is resumed with: each `yield*` operand's next type and each
        // contextually typed `yield` result (`const x: number = yield 1`).
        let mut nexts: Vec<TypeId> = Vec::new();
        for y in yields {
            let Node::Yield { arg, delegate } = *ast.node(y) else {
                continue;
            };
            if !delegate && let Some(c) = self.contextual_type(Loc::new(decl.file, y)) {
                nexts.push(c);
            }
            let t = match arg {
                None => TypeId::UNDEFINED,
                Some(a) => {
                    let t = self.check_expr(Loc::new(decl.file, a));
                    if delegate {
                        nexts.push(self.delegated_next_type(t, is_async));
                        if self.store().flags(t).intersects(TypeFlags::ANY) {
                            TypeId::ANY
                        } else {
                            self.iterated_type(t, is_async).unwrap_or(TypeId::ANY)
                        }
                    } else if is_async {
                        self.awaited_type(t)
                    } else {
                        t
                    }
                }
            };
            ys.push(t);
        }
        let y = if ys.is_empty() {
            TypeId::NEVER
        } else {
            let u = self.union_with(&ys, Reduction::Subtype, AliasId::NONE);
            let unit = {
                let f = self.store().flags(u);
                f.intersects(TypeFlags::UNIT) && !f.contains(TypeFlags::UNION)
            };
            let u = match ctx {
                _ if !unit => u,
                Some((c, _, _)) if self.is_literal_of_contextual_type(u, c) => u,
                _ => self.widen_literal(u),
            };
            self.widened_type(u)
        };
        let mut types: Vec<TypeId> = Vec::new();
        let mut has_empty_return = false;
        let mut returns = Vec::new();
        collect_returns(ast, body, &mut returns, 0);
        for r in returns {
            match *ast.node(r) {
                Node::Return { arg: Some(a) } => {
                    let t = self.check_expr(Loc::new(decl.file, a));
                    types.push(if is_async { self.awaited_type(t) } else { t });
                }
                _ => has_empty_return = true,
            }
        }
        let end_reachable = match self.c.bound(decl.file).flow.function_flow(fnode) {
            None => true,
            Some(ff) => {
                let end = ff.end;
                !end.is_unreachable() && self.is_flow_reachable(decl.file, end)
            }
        };
        let r = if types.is_empty() {
            TypeId::VOID
        } else {
            if (end_reachable || has_empty_return) && self.c.opts.strict_null_checks {
                types.push(TypeId::UNDEFINED);
            }
            let u = self.union_with(&types, Reduction::Subtype, AliasId::NONE);
            let unit = {
                let f = self.store().flags(u);
                f.intersects(TypeFlags::UNIT) && !f.contains(TypeFlags::UNION)
            };
            let u = match ctx {
                _ if !unit => u,
                Some((_, c, _))
                    if c != TypeId::UNRESOLVED && self.is_literal_of_contextual_type(u, c) =>
                {
                    u
                }
                _ => self.widen_literal(u),
            };
            self.widened_type(u)
        };
        let n = match ctx {
            _ if !nexts.is_empty() => self.intersection(&nexts),
            Some((_, _, n)) if n != TypeId::UNRESOLVED => n,
            _ => TypeId::UNKNOWN,
        };
        let g = if is_async {
            G::AsyncGenerator
        } else {
            G::Generator
        };
        self.global_reference(g, &[y, r, n])
            .unwrap_or(TypeId::UNRESOLVED)
    }
}

impl<'c, 'a> Ck<'c, 'a> {
    /// The next type of a `yield*` operand: the third argument of a generator or iterator
    /// type (`Generator<Y, R, N>`), else `unknown` (arrays and strings take `unknown`).
    fn delegated_next_type(&mut self, t: TypeId, is_async: bool) -> TypeId {
        use crate::checker::GlobalType as G;
        if self.store().flags(t).intersects(TypeFlags::ANY) {
            return TypeId::ANY;
        }
        let known = |ck: &mut Self, t: TypeId| -> Option<TypeId> {
            let (target, args) = ck.generic_args(t)?;
            for g in [
                G::Generator,
                G::AsyncGenerator,
                G::Iterator,
                G::AsyncIterator,
                G::IterableIterator,
                G::AsyncIterableIterator,
                G::IteratorObject,
            ] {
                if ck.global_type(g) == Some(target) {
                    return Some(args.get(2).copied().unwrap_or(TypeId::UNKNOWN));
                }
            }
            None
        };
        if let Some(n) = known(self, t) {
            return n;
        }
        // Otherwise the parameter of `t[Symbol.iterator]().next` (a rest parameter's
        // element: `next(...args: any[])` takes `any`).
        let next_param = |ck: &mut Self| -> Option<TypeId> {
            let name = ck.well_known_symbol_name(if is_async {
                "asyncIterator"
            } else {
                "iterator"
            })?;
            let p = ck.property_of_type(t, &name)?;
            let pt = ck.type_of_symbol(p);
            let sig = ck
                .signatures_of(pt, SigKind::Call)
                .into_iter()
                .find(|&s| ck.store().sig(s).min_args == 0)?;
            let iter = ck.return_type_of(sig);
            if let Some(n) = known(ck, iter) {
                return Some(n);
            }
            let next = ck.property_of_type(iter, "next")?;
            let nt = ck.type_of_symbol(next);
            let nsig = *ck.signatures_of(nt, SigKind::Call).first()?;
            let d = ck.store().sig(nsig);
            let first = d.params.first().map(|p| p.ty)?;
            if d.has_rest() && d.params.len() == 1 {
                if ck.store().tuple_shape_of(first).is_some() {
                    return None;
                }
                return ck.element_type_of_array_like(first);
            }
            Some(first)
        };
        self.quietly(next_param).unwrap_or(TypeId::UNKNOWN)
    }
}

/// `yield` expressions of a generator body (not of nested functions or classes).
fn collect_yields(ast: &ts_r_ast::Ast, n: NodeId, out: &mut Vec<NodeId>, depth: u32) {
    if depth > 500 {
        return;
    }
    match ast.node(n) {
        Node::Yield { .. } => {
            out.push(n);
            ast.for_each_child(n, |c| collect_yields(ast, c, out, depth + 1));
        }
        Node::FunctionDecl { .. }
        | Node::FunctionExpr { .. }
        | Node::Arrow { .. }
        | Node::ClassDecl { .. }
        | Node::ClassExpr { .. } => {}
        _ => ast.for_each_child(n, |c| collect_yields(ast, c, out, depth + 1)),
    }
}

/// `return` statements of a function body (not of nested functions or classes).
pub fn collect_returns(ast: &ts_r_ast::Ast, n: NodeId, out: &mut Vec<NodeId>, depth: u32) {
    if depth > 500 {
        return;
    }
    match ast.node(n) {
        Node::Return { .. } => out.push(n),
        Node::FunctionDecl { .. }
        | Node::FunctionExpr { .. }
        | Node::Arrow { .. }
        | Node::ClassDecl { .. }
        | Node::ClassExpr { .. }
        | Node::ObjectLit { .. }
        | Node::Call { .. }
        | Node::Binary { .. }
        | Node::Assign { .. } => {}
        Node::ExprStmt { .. } | Node::VarDecl { .. } | Node::Throw { .. } => {}
        _ => ast.for_each_child(n, |c| collect_returns(ast, c, out, depth + 1)),
    }
}

impl<'c, 'a> Ck<'c, 'a> {
    /// Whether `t` is a function type created for a function expression.
    pub fn is_function_expression_type(&self, t: TypeId) -> bool {
        self.store()
            .object_of(t)
            .is_some_and(|o| o.flags.contains(ObjectFlags::FUNCTION_LIKE))
    }

    /// `SigFlags` helper for callers.
    pub fn sig_is_method(&self, s: SigId) -> bool {
        self.store().sig(s).flags.contains(SigFlags::METHOD)
    }
}

/// Whether the operands an initializer's value comes from (through parentheses,
/// conditionals and logical operators) include an array literal and no call: `Some(true)`
/// with an array literal, `Some(false)` with neither, `None` with a call.
fn array_literal_tree(ast: &ts_r_ast::Ast, n: ts_r_ast::NodeId, depth: u32) -> Option<bool> {
    if depth > 32 {
        return None;
    }
    let n = crate::nodes::skip_parens(ast, n);
    match *ast.node(n) {
        Node::ArrayLit { .. } => Some(true),
        Node::Conditional { cons, alt, .. } => {
            let a = array_literal_tree(ast, cons, depth + 1)?;
            let b = array_literal_tree(ast, alt, depth + 1)?;
            Some(a || b)
        }
        Node::Binary { left, op, right } => match op {
            ts_r_ast::BinaryOp::LogicalAnd => array_literal_tree(ast, right, depth + 1),
            ts_r_ast::BinaryOp::LogicalOr | ts_r_ast::BinaryOp::Coalesce => {
                let a = array_literal_tree(ast, left, depth + 1)?;
                let b = array_literal_tree(ast, right, depth + 1)?;
                Some(a || b)
            }
            _ => Some(false),
        },
        Node::Call { .. } | Node::New { .. } | Node::TaggedTemplate { .. } => None,
        _ => Some(false),
    }
}

/// `this.x` or `this["x"]`.
fn is_this_member(ast: &ts_r_ast::Ast, n: NodeId) -> bool {
    match *ast.node(crate::nodes::skip_parens(ast, n)) {
        Node::Member { obj, .. } | Node::Index { obj, .. } => {
            matches!(ast.node(crate::nodes::skip_parens(ast, obj)), Node::This {})
        }
        _ => false,
    }
}
