//! Implicit-any errors from widening `null` and `undefined` (TS7005, TS7006, TS7008,
//! TS7010, TS7011, TS7018, TS7025, TS7031, TS7055).
//!
//! Without `strictNullChecks`, `null`, `undefined` and `[]`'s element type are widening
//! types that become `any` when a declaration without an annotation takes them. Under
//! `noImplicitAny` tsc reports that (rule read from `reportErrorsFromWidening` and
//! `reportWideningErrorsInType` in `tsc/internal/checker/checker.go`, tsc 7.0.2):
//!
//! - The declarations: a variable with an initializer (not one that evolves, `let x =
//!   null`, nor `let xs = []`), a binding element of a variable's pattern, a parameter
//!   with an initializer and no contextual type, a class property with an initializer,
//!   and a function's inferred return type and generator yield type. For a function
//!   with a contextual signature, only when the contextual return (or yield) type is
//!   generic.
//! - First the widening is looked for inside the type: through union members (a union
//!   with an empty object type member counts as reported, silently), array and tuple
//!   elements, and object literal properties. An object literal property whose own type
//!   is a widening one and that is written directly in that literal is TS7018 at the
//!   property, with its widened type.
//! - When nothing inside was reported, the declaration is: TS7005 (variables), TS7031
//!   (binding elements), TS7006 (parameters), TS7008 (properties), TS7010 / TS7055 for a
//!   named function (return / yield type) and TS7011 / TS7025 for an anonymous one, each
//!   with the widened type printed.

use ts_r_ast::{FnFlags, MethodKind, Node, NodeId};
use ts_r_types::{ObjectFlags, TypeFlags, TypeId, TypeTraits};

use crate::checker::{Ck, Loc};
use crate::diag::dg;
use crate::nodes::FnParts;
use crate::unions::Reduction;

/// Nesting the search for a widening type inside a type follows.
const MAX_DEPTH: u32 = 32;

/// What kind of declaration took a widening type.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum WideningDecl {
    Variable,
    BindingElement,
    Parameter,
    Member,
}

impl<'c, 'a> Ck<'c, 'a> {
    /// Whether widening errors are reported in `file`.
    pub(crate) fn widening_errors_enabled(&self, file: ts_r_diagnostics::FileId) -> bool {
        self.c.opts.no_implicit_any
            && !self.c.opts.strict_null_checks
            && !self.c.file(file).is_js()
            && self.reporting(file)
    }

    /// Reports the widening of `t`, the unwidened type declaration `at` (named `name`)
    /// takes, when it has a widening type.
    pub(crate) fn report_widening_declaration(
        &mut self,
        at: Loc,
        kind: WideningDecl,
        name: &str,
        t: TypeId,
    ) {
        if !self
            .store()
            .traits(t)
            .contains(TypeTraits::CONTAINS_WIDENING)
            || self.report_widening_errors_in_type(t, 0)
        {
            return;
        }
        let w = self.widened_type(t);
        let ty = self.type_to_string(w);
        let info = match kind {
            WideningDecl::Variable => dg::VARIABLE_0_IMPLICITLY_HAS_AN_1_TYPE_7005,
            WideningDecl::BindingElement => dg::BINDING_ELEMENT_0_IMPLICITLY_HAS_AN_1_TYPE_7031,
            WideningDecl::Parameter => dg::PARAMETER_0_IMPLICITLY_HAS_AN_1_TYPE_7006,
            WideningDecl::Member => dg::MEMBER_0_IMPLICITLY_HAS_AN_1_TYPE_7008,
        };
        self.error(at, info, &[name, &ty]);
    }

    /// The widening errors of the names a variable's binding pattern `pattern` declares
    /// from the initializer's unwidened type `t`.
    pub(crate) fn report_binding_widening(&mut self, pattern: Loc, t: TypeId) {
        self.report_pattern_widening(pattern, t, 0);
    }

    fn report_pattern_widening(&mut self, pattern: Loc, t: TypeId, depth: u32) {
        if depth > MAX_DEPTH {
            return;
        }
        let file = pattern.file;
        let ast = self.ast(file);
        match *ast.node(pattern.node) {
            Node::ArrayPattern { elements } => {
                let tuple = self.store().tuple_shape_of(t).is_some();
                let args = self.c.type_args_of(t).to_vec();
                let is_array = self.c.is_array_type(t);
                for (i, &e) in ast.list(elements).to_vec().iter().enumerate() {
                    let et = match args.get(if tuple { i } else { 0 }) {
                        Some(&a) if tuple || is_array => a,
                        _ => continue,
                    };
                    self.report_element_widening(Loc::new(file, e), et, depth);
                }
            }
            Node::ObjectPattern { props } => {
                for &p in ast.list(props).to_vec().iter() {
                    let Node::BindingProperty { key, value, init } = *ast.node(p) else {
                        continue;
                    };
                    let Some(name) = crate::nodes::key_text(ast, key) else {
                        continue;
                    };
                    let Some(sym) = self.property_of_type(t, &name) else {
                        continue;
                    };
                    let mut pt = self.type_of_symbol(sym);
                    if let Some(i) = init {
                        let d = self.check_expr(Loc::new(file, i));
                        pt = self.union(&[pt, d]);
                    }
                    let target = value.unwrap_or(key);
                    if matches!(ast.node(target), Node::Ident { .. }) {
                        let at = if value.is_some() {
                            Loc::new(file, p)
                        } else {
                            Loc::new(file, key)
                        };
                        let n = self.c.text(Loc::new(file, target)).to_owned();
                        self.report_widening_declaration(at, WideningDecl::BindingElement, &n, pt);
                    } else {
                        self.report_pattern_widening(Loc::new(file, target), pt, depth + 1);
                    }
                }
            }
            _ => {}
        }
    }

    /// An array pattern element `e` of type `t`.
    fn report_element_widening(&mut self, e: Loc, t: TypeId, depth: u32) {
        let ast = self.ast(e.file);
        match *ast.node(e.node) {
            Node::Ident { .. } => {
                let n = self.c.text(e).to_owned();
                self.report_widening_declaration(e, WideningDecl::BindingElement, &n, t);
            }
            Node::AssignPattern { left, right } => {
                let d = self.check_expr(Loc::new(e.file, right));
                let u = self.union(&[t, d]);
                if matches!(ast.node(left), Node::Ident { .. }) {
                    let n = self.c.text(Loc::new(e.file, left)).to_owned();
                    self.report_widening_declaration(e, WideningDecl::BindingElement, &n, u);
                } else {
                    self.report_pattern_widening(Loc::new(e.file, left), u, depth + 1);
                }
            }
            Node::ObjectPattern { .. } | Node::ArrayPattern { .. } => {
                self.report_pattern_widening(e, t, depth + 1);
            }
            _ => {}
        }
    }

    /// TS7018 inside `t` where possible; whether something counts as reported.
    fn report_widening_errors_in_type(&mut self, t: TypeId, depth: u32) -> bool {
        if depth > MAX_DEPTH
            || !self
                .store()
                .traits(t)
                .contains(TypeTraits::CONTAINS_WIDENING)
        {
            return false;
        }
        let s = self.store();
        if s.flags(t).contains(TypeFlags::UNION) {
            let members = s.members_of_union(t).to_vec();
            if members.iter().any(|&m| self.is_empty_object_type(m)) {
                return true;
            }
            let mut reported = false;
            for m in members {
                reported = reported || self.report_widening_errors_in_type(m, depth + 1);
            }
            return reported;
        }
        if self.c.is_array_type(t) || s.tuple_shape_of(t).is_some() {
            let args = self.c.type_args_of(t).to_vec();
            let mut reported = false;
            for a in args {
                reported = reported || self.report_widening_errors_in_type(a, depth + 1);
            }
            return reported;
        }
        let Some(o) = s.object_of(t) else {
            return false;
        };
        if !o.flags.contains(ObjectFlags::OBJECT_LITERAL) {
            return false;
        }
        let literal = match o.kind {
            ts_r_types::ObjectKind::Literal { sym } => self.c.value_decl(sym),
            _ => None,
        };
        let Some(m) = o.members() else {
            return false;
        };
        let props: Vec<_> = m
            .names
            .iter()
            .copied()
            .zip(m.props.iter().copied())
            .collect();
        let mut reported = false;
        for (n, p) in props {
            let pt = self.type_of_symbol(p);
            if !self
                .store()
                .traits(pt)
                .contains(TypeTraits::CONTAINS_WIDENING)
            {
                continue;
            }
            reported = self.report_widening_errors_in_type(pt, depth + 1);
            if reported {
                continue;
            }
            let decl = self.c.synth_sym(p).and_then(|x| x.decl);
            if let Some(d) = decl
                && literal.is_some()
                && self.c.parent(d) == literal
            {
                let name = self.store().str(n).to_owned();
                let w = self.widened_type(pt);
                let ty = self.type_to_string(w);
                self.error(
                    d,
                    dg::OBJECT_LITERAL_S_PROPERTY_0_IMPLICITLY_HAS_AN_1_TYPE_7018,
                    &[&name, &ty],
                );
                reported = true;
            }
        }
        reported
    }

    /// The widening errors of the inferred return and yield types of function `l`
    /// without a return type annotation.
    pub(crate) fn check_return_widening(&mut self, l: Loc, parts: FnParts) {
        if parts.return_type.is_some() || !self.widening_errors_enabled(l.file) {
            return;
        }
        let Some(body) = parts.body else { return };
        let file = l.file;
        let ast = self.ast(file);
        let parent = self.c.parent(l);
        // The name tsc prints: the function's own, or its method's.
        let name: Option<NodeId> = match *ast.node(l.node) {
            Node::FunctionDecl { name, .. } | Node::FunctionExpr { name, .. } => match name {
                Some(n) => Some(n),
                None => match parent.map(|p| *ast.node(p.node)) {
                    Some(Node::MethodDef { kind, key, .. }) => match kind {
                        MethodKind::Method | MethodKind::Get => Some(key),
                        _ => return,
                    },
                    _ => None,
                },
            },
            Node::Arrow { .. } => None,
            _ => return,
        };
        let is_async = parts.flags.contains(FnFlags::ASYNC);
        let is_generator = parts.flags.contains(FnFlags::GENERATOR);
        let is_expr = matches!(
            ast.node(l.node),
            Node::FunctionExpr { .. } | Node::Arrow { .. }
        );
        // With a contextual signature, only a generic contextual type reports.
        let ctx = if is_expr {
            self.contextual_signature(l).map(|s| self.return_type_of(s))
        } else {
            None
        };
        if is_generator {
            let mut yields = Vec::new();
            crate::func::collect_yields(ast, body, &mut yields, 0);
            let mut ys = Vec::with_capacity(yields.len());
            for y in yields {
                let Node::Yield { arg, delegate } = *ast.node(y) else {
                    continue;
                };
                let t = match arg {
                    None => TypeId::UNDEFINED_WIDENING,
                    Some(a) => {
                        let t = self.check_expr(Loc::new(file, a));
                        if delegate {
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
            if !ys.is_empty() {
                let y = self.union_with(&ys, Reduction::Subtype, ts_r_types::AliasId::NONE);
                let report = match ctx {
                    None => true,
                    Some(_) => match self.generator_context(l) {
                        Some((cy, _)) => cy != TypeId::UNRESOLVED && self.is_generic_type(cy),
                        None => false,
                    },
                };
                if report {
                    self.report_function_widening(l, name, y, true);
                }
            }
        }
        let mut types = Vec::new();
        if parts.is_arrow && parts.flags.contains(FnFlags::EXPRESSION_BODY) {
            types.push(self.check_expr(Loc::new(file, body)));
        } else {
            let mut returns = Vec::new();
            crate::func::collect_returns(ast, body, &mut returns, 0);
            for r in returns {
                if let Node::Return { arg: Some(a) } = *ast.node(r) {
                    types.push(self.check_expr(Loc::new(file, a)));
                }
            }
        }
        if types.is_empty() {
            return;
        }
        if is_async {
            for t in &mut types {
                *t = self.awaited_type(*t);
            }
        }
        let r = self.union_with(&types, Reduction::Subtype, ts_r_types::AliasId::NONE);
        let report = match ctx {
            None => true,
            Some(_) if is_generator => match self.generator_context(l) {
                Some((_, cr)) => cr != TypeId::UNRESOLVED && self.is_generic_type(cr),
                None => false,
            },
            Some(c) => {
                let c = if is_async { self.awaited_type(c) } else { c };
                self.is_generic_type(c)
            }
        };
        if report {
            self.report_function_widening(l, name, r, false);
        }
    }

    /// The widening error of function `l`'s inferred return (or yield) type `t`.
    fn report_function_widening(&mut self, l: Loc, name: Option<NodeId>, t: TypeId, yields: bool) {
        if !self
            .store()
            .traits(t)
            .contains(TypeTraits::CONTAINS_WIDENING)
            || self.report_widening_errors_in_type(t, 0)
        {
            return;
        }
        let w = self.widened_type(t);
        let ty = self.type_to_string(w);
        match name {
            Some(n) => {
                let nl = Loc::new(l.file, n);
                let text = self.c.text(nl).to_owned();
                let info = if yields {
                    dg::_0_WHICH_LACKS_RETURN_TYPE_ANNOTATION_IMPLICITLY_HAS_AN_1_YIELD_TYPE_7055
                } else {
                    dg::_0_WHICH_LACKS_RETURN_TYPE_ANNOTATION_IMPLICITLY_HAS_AN_1_RETURN_TYPE_7010
                };
                self.error(nl, info, &[&text, &ty]);
            }
            None => {
                let info = if yields {
                    dg::GENERATOR_IMPLICITLY_HAS_YIELD_TYPE_0_CONSIDER_SUPPLYING_A_RETURN_TYPE_ANNOTATIO_7025
                } else {
                    dg::FUNCTION_EXPRESSION_WHICH_LACKS_RETURN_TYPE_ANNOTATION_IMPLICITLY_HAS_AN_0_RETUR_7011
                };
                self.error(l, info, &[&ty]);
            }
        }
    }
}
