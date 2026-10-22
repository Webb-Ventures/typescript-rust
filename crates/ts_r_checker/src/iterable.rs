//! Values that must be iterable: array destructuring and spreads (TS2488, or TS2495 when
//! the lib has no `Symbol.iterator`).
//!
//! Rules observed from tsc 7.0.2:
//! - An array binding or assignment pattern (nested ones too) whose source is not
//!   iterable: at the pattern, naming the source type. `unknown` and `never` sources are
//!   errors; `any` is not. A declaration's top-level pattern with an initializer and no
//!   annotation names the initializer's own type (`([...r] = null)` says `'null'` even
//!   without `strictNullChecks`).
//! - A spread in an array literal or a call, or a `yield*` operand, that is not iterable:
//!   at the operand. `never` is fine there, `unknown` is not.
//! - `for...of` over a non-iterable is checked with the statement (check.rs).

use ts_r_ast::{Node, NodeId};
use ts_r_binder::meaning;
use ts_r_types::{TypeFlags, TypeId};

use crate::checker::{Ck, Loc};
use crate::diag::dg;

impl<'c, 'a> Ck<'c, 'a> {
    /// Reports `t` at `at` as not iterable unless it is (or cannot be judged).
    pub(crate) fn check_iterable_value(&mut self, at: Loc, t: TypeId) {
        if t == TypeId::UNRESOLVED
            || self.store().is_error(t)
            || self.store().flags(t).intersects(TypeFlags::ANY)
            || self.iterated_or_constraint(t).is_some()
            // Without a lib (`noLib`) the global types are missing and tsc is silent.
            || self.c.global_symbol("Array", meaning::TYPE).is_none()
        {
            return;
        }
        // Empty intersections print as the `never` tsc reduces them to.
        let members = if self.store().flags(t).contains(TypeFlags::UNION) {
            self.store().members_of_union(t).to_vec()
        } else {
            vec![t]
        };
        let all_empty = members
            .iter()
            .all(|&m| self.has_conflicting_unit_properties(m));
        let ts = self.type_to_string(if all_empty { TypeId::NEVER } else { t });
        let msg = if self.well_known_symbol_name("iterator").is_none() {
            dg::TYPE_0_IS_NOT_AN_ARRAY_TYPE_OR_A_STRING_TYPE_2495
        } else {
            dg::TYPE_0_MUST_HAVE_A_SYMBOL_ITERATOR_METHOD_THAT_RETURNS_AN_ITERATOR_2488
        };
        self.error(at, msg, &[&ts]);
    }

    /// The element type of iterating `t`, or of its constraint when `t` is generic. The
    /// constraint's `null` and `undefined` are left out: a reference narrowed by
    /// `typeof x !== "undefined"` is not narrowed here (no error rather than a wrong one).
    pub(crate) fn iterated_or_constraint(&mut self, t: TypeId) -> Option<TypeId> {
        if let Some(e) = self.iterated_type(t, false) {
            return Some(e);
        }
        // Union members that are empty intersections (`{ t: "a" } & { t: "b" }`) are
        // `never` and iterate nothing.
        if self.store().flags(t).contains(TypeFlags::UNION) {
            let members = self.store().members_of_union(t).to_vec();
            let kept: Vec<TypeId> = members
                .iter()
                .copied()
                .filter(|&m| !self.has_conflicting_unit_properties(m))
                .collect();
            if kept.len() < members.len() && !kept.is_empty() {
                let rest = self.union(&kept);
                return self.iterated_type(rest, false);
            }
        }
        if !self.is_generic_type(t) {
            return None;
        }
        let c = self.base_constraint(t);
        let c = self.non_nullable(c);
        if c == t || c == TypeId::UNRESOLVED || self.store().is_error(c) {
            return None;
        }
        if self.store().flags(c).intersects(TypeFlags::ANY) {
            return Some(c);
        }
        self.iterated_type(c, false)
    }

    /// An intersection of object types giving one property two different unit types (a
    /// type tsc reduces to `never`).
    fn has_conflicting_unit_properties(&mut self, t: TypeId) -> bool {
        if !self.store().flags(t).contains(TypeFlags::INTERSECTION) {
            return false;
        }
        let parts = self.store().members_of_union(t).to_vec();
        let mut seen: Vec<(ts_r_types::StrId, TypeId)> = Vec::new();
        for p in parts {
            if !self.store().flags(p).contains(TypeFlags::OBJECT) {
                continue;
            }
            for (name, sym) in self.properties_of(p) {
                let pt = self.type_of_symbol(sym);
                if !self.store().flags(pt).intersects(TypeFlags::UNIT) {
                    continue;
                }
                if seen.iter().any(|&(n, other)| n == name && other != pt) {
                    return true;
                }
                seen.push((name, pt));
            }
        }
        false
    }

    /// The element type of spreading `t` (a spread element or argument, or `yield*`),
    /// reporting `t` at `at` when it is not iterable.
    pub(crate) fn spread_element_type(&mut self, at: Loc, t: TypeId) -> TypeId {
        if self.store().flags(t).intersects(TypeFlags::ANY) {
            return t;
        }
        if let Some(e) = self.iterated_or_constraint(t) {
            return e;
        }
        if t != TypeId::NEVER {
            self.check_iterable_value(at, t);
        }
        TypeId::UNRESOLVED
    }

    /// TS2493 at `at` when `t` is a tuple without a rest element and `index` is past its
    /// end.
    pub(crate) fn check_tuple_element_in_range(&mut self, at: Loc, t: TypeId, index: f64) {
        if !self.reporting(at.file) {
            return;
        }
        let Some(shape) = self.store().tuple_shape_of(t) else {
            // A union of tuples none of which has the element: TS2339 naming the union.
            if self.union_tuples_out_of_range(t, index) {
                let ts = self.type_to_string(t);
                let is = ts_r_binder::js_number_to_string(index);
                self.error(
                    at,
                    dg::PROPERTY_0_DOES_NOT_EXIST_ON_TYPE_1_2339,
                    &[&is, &ts],
                );
            }
            return;
        };
        if shape.has_rest() {
            return;
        }
        let len = shape.elements.len();
        if index < len as f64 || index.fract() != 0.0 {
            return;
        }
        let ts = self.type_to_string(t);
        let is = ts_r_binder::js_number_to_string(index);
        self.error(
            at,
            dg::TUPLE_TYPE_0_OF_LENGTH_1_HAS_NO_ELEMENT_AT_INDEX_2_2493,
            &[&ts, &len.to_string(), &is],
        );
    }

    /// Whether `t` is a union of tuples without rest elements none of which has an element
    /// at `index`.
    pub(crate) fn union_tuples_out_of_range(&self, t: TypeId, index: f64) -> bool {
        let s = self.store();
        if !s.flags(t).contains(TypeFlags::UNION) || index < 0.0 || index.fract() != 0.0 {
            return false;
        }
        s.members_of_union(t).iter().all(|&m| {
            s.tuple_shape_of(m)
                .is_some_and(|sh| !sh.has_rest() && index >= sh.elements.len() as f64)
        })
    }

    /// The array patterns in the binding pattern of variable or parameter `decl` must
    /// destructure iterables.
    pub(crate) fn check_binding_pattern_iterables(&mut self, decl: Loc) {
        let ast = self.ast(decl.file);
        let (pattern, type_ann, init) = match *ast.node(decl.node) {
            Node::VarDeclarator {
                name,
                type_ann,
                init,
                ..
            } => (name, type_ann, init),
            Node::Param {
                pattern,
                type_ann,
                init,
                ..
            } => (pattern, type_ann, init),
            _ => return,
        };
        if !matches!(
            ast.node(pattern),
            Node::ArrayPattern { .. } | Node::ObjectPattern { .. }
        ) {
            return;
        }
        let is_param = matches!(ast.node(decl.node), Node::Param { .. });
        let in_for = self
            .c
            .parent(decl)
            .and_then(|vd| self.c.parent(vd))
            .is_some_and(|f| {
                matches!(
                    *self.ast(f.file).node(f.node),
                    Node::ForOf { .. } | Node::ForIn { .. }
                )
            });
        let mut stack = vec![pattern];
        let mut guard = 0;
        while let Some(p) = stack.pop() {
            guard += 1;
            if guard > 10_000 {
                break;
            }
            let pl = Loc::new(decl.file, p);
            match *ast.node(p) {
                Node::ArrayPattern { elements } => {
                    let t = match (type_ann, init) {
                        // The annotation itself (an optional parameter's adds no
                        // `undefined`); a parameter default replaces a written
                        // `undefined` (`([x]: [number] | undefined = [1])`).
                        (Some(ta), i) if p == pattern => {
                            let t = self.type_from_node(Loc::new(decl.file, ta));
                            if is_param && i.is_some() {
                                self.without_undefined_strict(t)
                            } else {
                                t
                            }
                        }
                        (None, Some(i)) if p == pattern && !in_for => {
                            self.quietly(|ck| ck.check_expr(Loc::new(decl.file, i)))
                        }
                        _ => self.pattern_parent_type(pl),
                    };
                    // A contextual `unknown` is a failed inference; tsc types such a
                    // parameter from its pattern instead.
                    let contextual_unknown = is_param
                        && type_ann.is_none()
                        && init.is_none()
                        && self.store().flags(t).contains(TypeFlags::UNKNOWN);
                    if !contextual_unknown {
                        self.check_iterable_value(pl, t);
                    }
                    // Elements past the end of a tuple without a rest (TS2493); one with a
                    // default is fine, and so is any of a parameter's own default (`([x,
                    // y] = [])`: the default is typed by the pattern).
                    let param_default = is_param && p == pattern && init.is_some();
                    for (i, &el) in ast.list(elements).iter().enumerate() {
                        if param_default {
                            break;
                        }
                        if !matches!(
                            ast.node(el),
                            Node::Hole {} | Node::Rest { .. } | Node::AssignPattern { .. }
                        ) {
                            self.check_tuple_element_in_range(Loc::new(decl.file, el), t, i as f64);
                        }
                    }
                    stack.extend(ast.list(elements).iter().rev());
                }
                Node::ObjectPattern { props } => {
                    // A numeric property past the end of a tuple (`{ 3: d }` of a pair).
                    let numeric: Vec<(NodeId, f64)> = ast
                        .list(props)
                        .iter()
                        .filter_map(|&pr| match *ast.node(pr) {
                            Node::BindingProperty { key, value, init } => {
                                let defaulted = init.is_some()
                                    || value.is_some_and(|v| {
                                        matches!(ast.node(v), Node::AssignPattern { .. })
                                    });
                                match *ast.node(key) {
                                    Node::NumberLit { value } if !defaulted => {
                                        Some((key, value.get()))
                                    }
                                    _ => None,
                                }
                            }
                            _ => None,
                        })
                        .collect();
                    let param_default = is_param && p == pattern && init.is_some();
                    if !numeric.is_empty() && !param_default {
                        let t = match (type_ann, init) {
                            (Some(ta), _) if p == pattern => {
                                self.type_from_node(Loc::new(decl.file, ta))
                            }
                            (None, Some(i)) if p == pattern && !in_for => {
                                self.quietly(|ck| ck.check_expr(Loc::new(decl.file, i)))
                            }
                            _ => self.pattern_parent_type(pl),
                        };
                        for (key, v) in numeric {
                            self.check_tuple_element_in_range(Loc::new(decl.file, key), t, v);
                        }
                    }
                    stack.extend(ast.list(props).iter().rev())
                }
                Node::BindingProperty { value: Some(v), .. } => stack.push(v),
                Node::AssignPattern { left, .. } => stack.push(left),
                Node::Rest { arg } => stack.push(arg),
                _ => {}
            }
        }
    }
}
