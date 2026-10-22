//! Names initializers must not read: later parameters (TS2372, TS2373) and constructor
//! locals from instance property initializers (TS2301).
//!
//! Rules observed from tsc 7.0.2:
//! - Each parameter, and each binding element of a parameter's pattern, with a default
//!   value is a unit. A name read in a unit's default value that resolves to the unit itself
//!   is TS2372 (`x = x`, `[e = e]`); one that resolves to a parameter or binding element
//!   of the same function written after the unit is TS2373, naming the unit (`y = {z}, z`;
//!   `[h = i, i]`). Earlier ones are fine.
//! - Only reads evaluated with the default value count: functions, arrows, methods,
//!   accessors and instance property initializers run later, except functions and arrows
//!   called where they are written (not async or generator ones). Static property
//!   initializers and static blocks of class expressions and computed names of
//!   object-literal and class members run immediately. `typeof x` in a type does not read
//!   `x`. Assignments count (`a = (b = 2), b`).
//! - A name read in a parameter's pattern (a computed key) belongs to the whole parameter,
//!   printed as written.
//! - When the parameter list holds syntax the target lowers (`??`/`?.` before ES2020, an
//!   object rest pattern before ES2018, a class static field unless class fields are
//!   native), a name declared by the function body (`var x`) shadows an outer one for every
//!   parameter: TS2373 (`(b = a() ?? "", d = x) => { var a, x; }`). Otherwise the outer
//!   name is read.
//!
//! - A read in a parameter list (functions and classes nested in it included) of a name
//!   that only the function's body declares is TS2304 (TS2552 with a suggestion); a
//!   function declaration read immediately is TS2373. With an outer declaration of the
//!   name, the outer one is read.
//!
//! TS2301: an instance property initializer (functions and arrows in it included, nested
//! classes not) that reads a name declared outside the class (or nowhere, instead of
//! TS2304), when the class's constructor implementation declares the same name (a
//! parameter or a local of its body): the initializer would run inside the constructor and
//! see the local. Not reported when class fields are native (`useDefineForClassFields` with
//! target ES2022 or later). The same read in a `typeof` query of the property's type is TS2844
//! (tsc: binder/nameresolver.go, the property-declaration case of name resolution).

use ts_r_ast::{MethodKind, Modifiers, Node, NodeId, Span};
use ts_r_config::ScriptTarget;
use ts_r_types::SymId;

use crate::checker::{Ck, Loc};
use crate::diag::dg;

/// Maximum number of ancestors walked from a use.
const MAX_WALK: usize = 4096;

impl<'c, 'a> Ck<'c, 'a> {
    /// TS2372/TS2373 for the read `e` of symbol `s` named `name`.
    pub(crate) fn check_parameter_reference(&mut self, e: Loc, s: SymId, name: &str) {
        if !self.reporting(e.file) {
            return;
        }
        let Some((unit, unit_name)) = self.initializer_unit_of(e) else {
            return;
        };
        let Some((_, u_fn)) = self.param_binding_of(unit) else {
            return;
        };
        let ast = self.ast(e.file);
        let unit_span = ast.span(unit_name);
        let unit_text = self.c.text(Loc::new(e.file, unit_name)).to_owned();
        let param = self
            .c
            .value_decl(s)
            .filter(|d| d.file == e.file)
            .and_then(|d| self.param_binding_of(d))
            .filter(|&(_, f)| f == u_fn);
        match param {
            Some((d_name, _)) => {
                let d_span: Span = ast.span(d_name);
                if d_span == unit_span {
                    self.self_referencing_default_annotation(e, unit);
                    self.error(e, dg::PARAMETER_0_CANNOT_REFERENCE_ITSELF_2372, &[name]);
                } else if d_span.start > unit_span.start {
                    self.error(
                        e,
                        dg::PARAMETER_0_CANNOT_REFERENCE_IDENTIFIER_1_DECLARED_AFTER_IT_2373,
                        &[&unit_text, name],
                    );
                }
            }
            // A local of the function's body shadows the outer name for its parameters when
            // they are lowered into the body.
            None => {
                let Some(parts) = crate::nodes::fn_parts(ast, u_fn) else {
                    return;
                };
                let Some(body) = parts.body else {
                    return;
                };
                let b = self.c.bound(e.file);
                let declared = [u_fn, body].into_iter().any(|n| {
                    b.scope_of(n).is_some_and(|sc| {
                        let locals = b.scopes[sc.0 as usize].locals;
                        b.lookup_str(ast, locals, name).is_some_and(|l| {
                            b.decls(l)
                                .first()
                                .is_some_and(|&dn| ast.span(dn).start >= ast.span(body).start)
                        })
                    })
                });
                if declared && self.params_lowered(Loc::new(e.file, u_fn), parts.params) {
                    self.error(
                        e,
                        dg::PARAMETER_0_CANNOT_REFERENCE_IDENTIFIER_1_DECLARED_AFTER_IT_2373,
                        &[&unit_text, name],
                    );
                }
            }
        }
    }

    /// `true` when the type name `l` in a function's signature (type parameters,
    /// parameters, return type) resolved to `s`, declared only in that function's body:
    /// the body's types are not in scope there (the binder resolves outward first, so no
    /// outer declaration exists). The caller reports TS2304.
    pub(crate) fn type_from_signature_to_body(&self, l: Loc, s: SymId) -> bool {
        let Some(d) = self
            .c
            .decls(s)
            .first()
            .copied()
            .filter(|d| d.file == l.file)
        else {
            return false;
        };
        let ds = self.c.span(d);
        let ast = self.ast(l.file);
        let parents = self.c.file(l.file).parents();
        let mut child = l.node;
        let mut cur = parents.parent(l.node);
        for _ in 0..MAX_WALK {
            let Some(n) = cur else {
                return false;
            };
            if let Some(parts) = crate::nodes::fn_parts(ast, n)
                && let Some(b) = parts.body
                && b != child
            {
                let bs = ast.span(b);
                if bs.start <= ds.start && ds.end <= bs.end {
                    return true;
                }
            }
            let ns = ast.span(n);
            if ns.start <= ds.start && ds.end <= ns.end {
                return false;
            }
            child = n;
            cur = parents.parent(n);
        }
        false
    }

    /// A read `e` in the parameter list of a function (nested functions and classes in it
    /// included) that resolved to `s`, a declaration of that function's body (only when no
    /// outer declaration exists; the binder resolves outward first): TS2304/TS2552, or
    /// TS2373 for a function declaration read immediately. Returns whether it was reported
    /// (the read is then unresolved). Namespaces are left to TS2708.
    pub(crate) fn check_body_local_from_parameter(&mut self, e: Loc, s: SymId, name: &str) -> bool {
        let Some(d) = self.c.value_decl(s).filter(|d| d.file == e.file) else {
            return false;
        };
        let ast = self.ast(e.file);
        if matches!(ast.node(d.node), Node::Module { .. }) {
            return false;
        }
        let parents = self.c.file(e.file).parents();
        let mut cur = parents.parent(e.node);
        let mut body = None;
        for _ in 0..MAX_WALK {
            let Some(n) = cur else {
                break;
            };
            if let Node::Param { .. } = *ast.node(n)
                && let Some(f) = parents.parent(n)
                && let Some(parts) = crate::nodes::fn_parts(ast, f)
                && ast.list(parts.params).contains(&n)
                && let Some(b) = parts.body
            {
                body = Some(b);
                break;
            }
            if matches!(ast.node(n), Node::Program { .. }) {
                break;
            }
            cur = parents.parent(n);
        }
        let Some(body) = body else {
            return false;
        };
        let bs = ast.span(body);
        let ds = self.c.span(d);
        if ds.start < bs.start || ds.end > bs.end {
            return false;
        }
        if !self.reporting(e.file) {
            return true;
        }
        if matches!(ast.node(d.node), Node::FunctionDecl { .. }) {
            if let Some((_, unit_name)) = self.initializer_unit_of(e) {
                let unit_text = self.c.text(Loc::new(e.file, unit_name)).to_owned();
                self.error(
                    e,
                    dg::PARAMETER_0_CANNOT_REFERENCE_IDENTIFIER_1_DECLARED_AFTER_IT_2373,
                    &[&unit_text, name],
                );
                return true;
            }
            return false;
        }
        self.report_cannot_find_name(e, name);
        true
    }

    /// Whether a parameter list holds syntax the target lowers, which moves the parameter
    /// initializers into the function body: `??` and `?.` before ES2020, an object rest
    /// pattern before ES2018, a class expression with a static field unless class fields
    /// are native.
    fn params_lowered(&mut self, function: Loc, params: ts_r_ast::NodeList) -> bool {
        if let Some(&lowered) = self.lowered_params.get(&function) {
            return lowered;
        }
        let lowered = self.compute_params_lowered(function.file, params);
        self.lowered_params.insert(function, lowered);
        lowered
    }

    fn compute_params_lowered(
        &self,
        file: ts_r_diagnostics::FileId,
        params: ts_r_ast::NodeList,
    ) -> bool {
        let ast = self.ast(file);
        let target = self.c.opts.target;
        let native_fields =
            self.c.opts.use_define_for_class_fields && target >= ScriptTarget::ES2022;
        let mut stack: Vec<NodeId> = ast.list(params).to_vec();
        let mut budget = 100_000u32;
        while let Some(n) = stack.pop() {
            budget = budget.saturating_sub(1);
            if budget == 0 {
                return false;
            }
            let lowered = match *ast.node(n) {
                Node::Binary {
                    op: ts_r_ast::BinaryOp::Coalesce,
                    ..
                }
                | Node::Call { optional: true, .. }
                | Node::Member { optional: true, .. }
                | Node::Index { optional: true, .. } => target < ScriptTarget::ES2020,
                Node::ObjectPattern { props } => {
                    target < ScriptTarget::ES2018
                        && ast
                            .list(props)
                            .iter()
                            .any(|&p| matches!(ast.node(p), Node::Rest { .. }))
                }
                Node::PropertyDef {
                    modifiers,
                    value: Some(_),
                    ..
                } => !native_fields && modifiers.contains(Modifiers::STATIC),
                _ => false,
            };
            if lowered {
                return true;
            }
            ast.for_each_child(n, |c| stack.push(c));
        }
        false
    }

    /// TS2301 for the read `e` named `name`, resolved to `s` (`None`: not resolved).
    /// Returns whether it was reported.
    pub(crate) fn check_constructor_shadowed_reference(
        &mut self,
        e: Loc,
        s: Option<SymId>,
        name: &str,
    ) -> bool {
        if !self.reporting(e.file)
            || (self.c.opts.use_define_for_class_fields
                && self.c.opts.target >= ScriptTarget::ES2022)
        {
            return false;
        }
        let ast = self.ast(e.file);
        // The instance property whose initializer holds `e`.
        let mut prev = e;
        let mut cur = self.c.parent(e);
        let mut prop = None;
        let mut in_query = false;
        for _ in 0..MAX_WALK {
            let Some(c) = cur else {
                return false;
            };
            match *ast.node(c.node) {
                Node::PropertyDef {
                    value,
                    type_ann,
                    modifiers,
                    key,
                    ..
                } if value == Some(prev.node) || (in_query && type_ann == Some(prev.node)) => {
                    if modifiers.contains(Modifiers::STATIC) {
                        return false;
                    }
                    prop = Some((c, key, type_ann == Some(prev.node)));
                    break;
                }
                Node::TypeQuery { .. } => in_query = true,
                Node::ClassDecl { .. } | Node::ClassExpr { .. } | Node::Program { .. } => {
                    return false;
                }
                _ => {}
            }
            prev = c;
            cur = self.c.parent(c);
        }
        let Some((p, key, in_type)) = prop else {
            return false;
        };
        let Some(class) = self.c.parent(p) else {
            return false;
        };
        let members = match *ast.node(class.node) {
            Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. } => members,
            _ => return false,
        };
        // Only names declared outside the class.
        if let Some(s) = s
            && let Some(d) = self.c.value_decl(s).or_else(|| self.c.first_decl(s))
            && d.file == e.file
        {
            let cs = ast.span(class.node);
            let ds = self.c.span(d);
            if cs.start <= ds.start && ds.end <= cs.end {
                return false;
            }
        }
        let ctor = ast.list(members).iter().find_map(|&m| match *ast.node(m) {
            Node::MethodDef {
                kind: MethodKind::Constructor,
                value,
                ..
            } => {
                crate::nodes::fn_parts(ast, value).and_then(|parts| parts.body.map(|b| (value, b)))
            }
            _ => None,
        });
        let Some((f, body)) = ctor else {
            return false;
        };
        // An unresolved name that is also an instance member is TS2663 instead (not in a
        // `typeof` query, where no `this.` prefix is suggested).
        if s.is_none() && !in_query {
            let member_named = ast.list(members).iter().any(|&m| match *ast.node(m) {
                Node::PropertyDef { key, modifiers, .. }
                | Node::MethodDef { key, modifiers, .. } => {
                    !modifiers.contains(Modifiers::STATIC)
                        && crate::nodes::key_text(ast, key).as_deref() == Some(name)
                }
                _ => false,
            });
            let param_property = crate::nodes::fn_parts(ast, f).is_some_and(|parts| {
                ast.list(parts.params).iter().any(|&p| {
                    matches!(*ast.node(p), Node::Param { modifiers, pattern, .. }
                        if !modifiers.is_empty()
                            && crate::nodes::ident_text(ast, pattern) == Some(name))
                })
            });
            if member_named || param_property {
                return false;
            }
        }
        let b = self.c.bound(e.file);
        let declared = [f, body].into_iter().any(|n| {
            b.scope_of(n).is_some_and(|sc| {
                let locals = b.scopes[sc.0 as usize].locals;
                b.lookup_str(ast, locals, name).is_some()
            })
        });
        if !declared {
            return false;
        }
        let prop_name = self.c.text(Loc::new(e.file, key)).to_owned();
        let info = if in_type {
            dg::TYPE_OF_INSTANCE_MEMBER_VARIABLE_0_CANNOT_REFERENCE_IDENTIFIER_1_DECLARED_IN_THE_2844
        } else {
            dg::INITIALIZER_OF_INSTANCE_MEMBER_VARIABLE_0_CANNOT_REFERENCE_IDENTIFIER_1_DECLARED_2301
        };
        self.error(e, info, &[&prop_name, name]);
        true
    }

    /// For a parameter, or a binding element (or its name) inside a parameter's pattern:
    /// the name node, and the function whose parameter list holds it.
    fn param_binding_of(&self, d: Loc) -> Option<(NodeId, NodeId)> {
        let ast = self.ast(d.file);
        let name = match *ast.node(d.node) {
            Node::Param { pattern, .. } => pattern,
            Node::AssignPattern { left, .. } => left,
            Node::BindingProperty { key, value, .. } => value.unwrap_or(key),
            Node::Ident { .. } => d.node,
            _ => return None,
        };
        let mut cur = d;
        for _ in 0..MAX_WALK {
            match *ast.node(cur.node) {
                Node::Param { .. } => {
                    let f = self.c.parent(cur)?;
                    return match *ast.node(f.node) {
                        Node::FunctionDecl { .. }
                        | Node::FunctionExpr { .. }
                        | Node::Arrow { .. } => Some((name, f.node)),
                        _ => None,
                    };
                }
                Node::Ident { .. }
                | Node::AssignPattern { .. }
                | Node::BindingProperty { .. }
                | Node::ObjectPattern { .. }
                | Node::ArrayPattern { .. }
                | Node::Rest { .. } => {}
                _ => return None,
            }
            cur = self.c.parent(cur)?;
        }
        None
    }

    /// The parameter or binding element whose default value evaluates `e` immediately, and
    /// that unit's name node.
    /// TS2502 at a parameter whose default reads the parameter itself, when its declared
    /// type (annotated or `?`) includes `undefined` under strict null checks: whether the
    /// default removes `undefined` from it depends on the default's own type.
    fn self_referencing_default_annotation(&mut self, e: Loc, unit: Loc) {
        if !self.c.opts.strict_null_checks || self.c.file(e.file).is_js() {
            return;
        }
        let ast = self.ast(unit.file);
        let Node::Param {
            pattern,
            init: Some(init),
            type_ann,
            optional,
            ..
        } = *ast.node(unit.node)
        else {
            return;
        };
        let inside = ast.span(init);
        let at = ast.span(e.node);
        if (type_ann.is_none() && !optional)
            || !matches!(ast.node(pattern), Node::Ident { .. })
            || at.start < inside.start
            || at.end > inside.end
        {
            return;
        }
        let t = self.param_type(unit);
        if !self.contains_undefined(t) {
            return;
        }
        let name = self.c.text(Loc::new(unit.file, pattern)).to_owned();
        self.error(
            Loc::new(unit.file, pattern),
            dg::_0_IS_REFERENCED_DIRECTLY_OR_INDIRECTLY_IN_ITS_OWN_TYPE_ANNOTATION_2502,
            &[&name],
        );
    }

    fn initializer_unit_of(&self, e: Loc) -> Option<(Loc, NodeId)> {
        let ast = self.ast(e.file);
        let mut prev = e;
        let mut cur = self.c.parent(e)?;
        for _ in 0..MAX_WALK {
            match *ast.node(cur.node) {
                // Computed keys in the pattern belong to the whole parameter.
                Node::Param { pattern, init, .. } => {
                    return (init == Some(prev.node) || pattern == prev.node)
                        .then_some((cur, pattern));
                }
                Node::AssignPattern { left, right } if right == prev.node => {
                    return Some((cur, left));
                }
                Node::BindingProperty {
                    key,
                    value,
                    init: Some(i),
                } if i == prev.node => {
                    return Some((cur, value.unwrap_or(key)));
                }
                Node::FunctionExpr { .. } | Node::Arrow { .. } => {
                    if !self.is_iife(cur) {
                        return None;
                    }
                }
                Node::PropertyDef {
                    value: Some(v),
                    modifiers,
                    ..
                } if v == prev.node && !modifiers.contains(ts_r_ast::Modifiers::STATIC) => {
                    return None;
                }
                Node::FunctionDecl { .. } | Node::TypeQuery { .. } | Node::Program { .. } => {
                    return None;
                }
                _ => {}
            }
            prev = cur;
            cur = self.c.parent(cur)?;
        }
        None
    }
}
