//! `@typescript-eslint/no-unsafe-enum-assignment`: values written to enum-typed places
//! (variables, parameters, properties, arguments, returns, assertions, computed keys of
//! enum-keyed objects) must have that enum's type, not a plain number or string.
//!
//! A source "has the enum" when every union member is a member of one of the target's enums,
//! something a non-enum part of the target accepts (`Fruit | number`), `any`/`unknown` or
//! nullish. Bitwise combinations of the enum's members (`A | B`, `A & ~B`) count as the
//! enum. Object and array types are compared structurally (same generic: type arguments;
//! else properties, index signatures and elements), and object and array literals are
//! followed down so the report lands on the offending property or element.

use rustc_hash::{FxHashMap, FxHashSet};

use super::prelude::*;
use super::ts_no_misused_promises::class_bases;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-unsafe-enum-assignment",
        "Disallow assigning non-enum values to enum typed locations",
        TS_STRICT_TC,
        Category::Suspicious,
    )
    .typed(),
    create: create_default::<NoUnsafeEnumAssignment>,
};

#[derive(Debug, Default)]
struct NoUnsafeEnumAssignment;

/// How deep structural comparisons and enum searches go.
const MAX_DEPTH: u32 = 10;

#[derive(Clone, Copy)]
enum Msg {
    Access,
    Argument,
    Assertion,
    Assignment,
    Mutation,
    Return,
}

impl Msg {
    fn id(self) -> &'static str {
        match self {
            Msg::Access => "unsafeEnumAccess",
            Msg::Argument => "unsafeEnumArgument",
            Msg::Assertion => "unsafeEnumAssertion",
            Msg::Assignment => "unsafeEnumAssignment",
            Msg::Mutation => "unsafeEnumMutation",
            Msg::Return => "unsafeEnumReturn",
        }
    }

    fn text(self, names: &str) -> String {
        let what = match self {
            Msg::Access => "The computed key used here",
            Msg::Argument => "The argument passed here",
            Msg::Assertion => "The value asserted here",
            Msg::Assignment => "The value assigned here",
            Msg::Return => "The value returned here",
            Msg::Mutation => {
                return format!(
                    "This mutation can produce a value outside of the expected enum {names}."
                );
            }
        };
        format!("{what} does not have a shared enum type with the expected enum {names}.")
    }
}

struct Found {
    span: Span,
    msg: Msg,
    enums: Vec<Sym>,
}

impl Rule for NoUnsafeEnumAssignment {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::VarDeclarator,
            NodeKind::Param,
            NodeKind::PropertyDef,
            NodeKind::Assign,
            NodeKind::Update,
            NodeKind::Return,
            NodeKind::Arrow,
            NodeKind::Call,
            NodeKind::New,
            NodeKind::TaggedTemplate,
            NodeKind::As,
            NodeKind::TypeAssertion,
            NodeKind::Satisfies,
            NodeKind::JsxAttr,
            NodeKind::Index,
        ]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let found = {
            let cxr: &Ctx<'_, '_> = cx;
            let mut w = Walk {
                cx: cxr,
                ti: cxr.types(),
                out: Vec::new(),
                has_enum_memo: FxHashMap::default(),
            };
            w.visit(node);
            w.out
        };
        for f in found {
            let names = enum_names(cx.types(), &f.enums);
            cx.report(f.span, f.msg.text(&names)).id(f.msg.id());
        }
    }
}

/// `'A', 'B'`, sorted.
fn enum_names(ti: &dyn TypeInfo, enums: &[Sym]) -> String {
    let mut names: Vec<String> = enums.iter().map(|&e| ti.symbol_name(e)).collect();
    names.sort();
    names.dedup();
    names
        .iter()
        .map(|n| format!("'{n}'"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The enum of an enum member type (or of a whole enum type).
fn enum_of(ti: &dyn TypeInfo, t: Ty) -> Option<Sym> {
    let f = ti.flags(t);
    if f.contains(TypeFlags::ENUM_LITERAL) {
        let member = ti.type_symbol(t)?;
        return ti.symbol_parent(member);
    }
    if f.contains(TypeFlags::ENUM) {
        return ti.type_symbol(t);
    }
    None
}

fn is_wild(ti: &dyn TypeInfo, t: Ty) -> bool {
    t == Ty::UNRESOLVED || is_any(ti, t) || is_unknown(ti, t)
}

struct Walk<'c, 'f, 'a> {
    cx: &'c Ctx<'f, 'a>,
    ti: &'a dyn TypeInfo,
    out: Vec<Found>,
    has_enum_memo: FxHashMap<Ty, bool>,
}

impl<'a> Walk<'_, '_, 'a> {
    fn ast(&self) -> &'a ts_r_ast::Ast {
        self.cx.ast()
    }

    fn push(&mut self, span: Span, msg: Msg, enums: Vec<Sym>) {
        self.out.push(Found { span, msg, enums });
    }

    fn visit(&mut self, node: NodeId) {
        let ti = self.ti;
        match *self.cx.node(node) {
            Node::VarDeclarator {
                name,
                type_ann: Some(ta),
                init,
                ..
            } => {
                let Some(target) = ti.type_of_type_node(ta) else {
                    return;
                };
                if let Some(init) = init {
                    self.check_expr(init, target, self.cx.span(node), Msg::Assignment, false);
                }
                self.pattern_defaults(name, target);
            }
            Node::Param {
                pattern,
                type_ann: Some(ta),
                init,
                ..
            } => {
                let Some(target) = ti.type_of_type_node(ta) else {
                    return;
                };
                if let Some(init) = init {
                    let span = Span::new(self.cx.span(pattern).start, self.cx.span(init).end);
                    self.check_expr(init, target, span, Msg::Assignment, false);
                }
                self.pattern_defaults(pattern, target);
            }
            Node::PropertyDef {
                modifiers,
                key,
                type_ann,
                value: Some(value),
                ..
            } => self.class_property(node, modifiers, key, type_ann, value),
            Node::Assign { left, op, right } => self.assignment(node, left, op, right),
            Node::Update { arg, .. } => {
                let Some(target) = self.declared_target(arg) else {
                    return;
                };
                let enums = self.top_enums(target);
                if !enums.is_empty() {
                    self.push(self.cx.span(node), Msg::Mutation, enums);
                }
            }
            Node::Return { arg: Some(arg) } => {
                let Some((target, is_async)) = self.return_target(node) else {
                    return;
                };
                self.check_expr(arg, target, self.cx.span(node), Msg::Return, is_async);
            }
            Node::Arrow {
                flags,
                return_type: Some(rt),
                body,
                ..
            } if flags.contains(FnFlags::EXPRESSION_BODY) => {
                let Some(mut target) = ti.type_of_type_node(rt) else {
                    return;
                };
                let is_async = flags.contains(FnFlags::ASYNC);
                if is_async {
                    target = ti.awaited_type(target);
                }
                self.check_expr(body, target, self.cx.span(body), Msg::Return, is_async);
            }
            Node::Call { .. } | Node::New { .. } | Node::TaggedTemplate { .. } => {
                self.call(node);
            }
            Node::As { expr, ty } | Node::TypeAssertion { ty, expr } => {
                if self.cx.text(ty) == "const" {
                    return;
                }
                let Some(target) = ti.type_of_type_node(ty) else {
                    return;
                };
                self.check_expr(expr, target, self.cx.span(node), Msg::Assertion, false);
            }
            Node::Satisfies { expr, ty } => {
                let Some(target) = ti.type_of_type_node(ty) else {
                    return;
                };
                self.check_expr(expr, target, self.cx.span(node), Msg::Assignment, false);
            }
            Node::JsxAttr {
                name,
                value: Some(value),
            } => {
                let Node::JsxExprContainer { expr: Some(expr) } = *self.cx.node(value) else {
                    return;
                };
                let Some(target) = self.jsx_attr_target(node, name) else {
                    return;
                };
                self.check_expr(expr, target, self.cx.span(expr), Msg::Assignment, false);
            }
            Node::Index { obj, index, .. } => self.access(obj, index),
            _ => {}
        }
    }

    // ---- sites ------------------------------------------------------------------------

    fn class_property(
        &mut self,
        node: NodeId,
        modifiers: Modifiers,
        key: NodeId,
        type_ann: Option<NodeId>,
        value: NodeId,
    ) {
        // Class fields are checked as a whole (literals are not followed).
        let ti = self.ti;
        let src = self.cx.src();
        let span = self.cx.span(node);
        if let Some(ta) = type_ann {
            if let Some(target) = ti.type_of_type_node(ta)
                && let Some(en) = self.leaf(value, target, false)
            {
                self.push(span, Msg::Assignment, en);
            }
            return;
        }
        if modifiers.contains(Modifiers::STATIC) {
            return;
        }
        let ast = self.ast();
        let Some(name) = key_name(ast, src, key) else {
            return;
        };
        if matches!(self.cx.node(key), Node::PrivateName { .. }) {
            return;
        }
        let Some(class) = self.cx.parent(node) else {
            return;
        };
        let heritage = match *self.cx.node(class) {
            Node::ClassDecl { heritage, .. } | Node::ClassExpr { heritage, .. } => heritage,
            _ => return,
        };
        let targets: Vec<Ty> = class_bases(self.cx, class, heritage)
            .into_iter()
            .filter_map(|b| ti.property_of_type(b, &name))
            .map(|p| ti.type_of_symbol(p))
            .collect();
        if targets.is_empty() {
            return;
        }
        // Reported only when no inherited type accepts the value.
        let mut first = None;
        for &t in &targets {
            match self.leaf(value, t, false) {
                None => return,
                Some(en) => {
                    if first.is_none() {
                        first = Some(en);
                    }
                }
            }
        }
        if let Some(en) = first {
            self.push(span, Msg::Assignment, en);
        }
    }

    fn assignment(&mut self, node: NodeId, left: NodeId, op: AssignOp, right: NodeId) {
        let value_op = matches!(
            op,
            AssignOp::Assign
                | AssignOp::CoalesceAssign
                | AssignOp::OrAssign
                | AssignOp::AndAssign
                | AssignOp::BitOrAssign
                | AssignOp::BitAndAssign
                | AssignOp::BitXorAssign
        );
        let left_inner = skip_parens(self.ast(), left);
        let target = match self.cx.node(left_inner) {
            Node::ArrayLit { .. }
            | Node::ObjectLit { .. }
            | Node::ArrayPattern { .. }
            | Node::ObjectPattern { .. } => {
                if op == AssignOp::Assign {
                    self.pattern_assignment(left_inner, right, 0);
                }
                return;
            }
            _ => self.declared_target(left_inner),
        };
        let Some(target) = target else {
            return;
        };
        if value_op {
            self.check_expr(right, target, self.cx.span(node), Msg::Assignment, false);
        } else {
            let enums = self.top_enums(target);
            if !enums.is_empty() {
                self.push(self.cx.span(node), Msg::Mutation, enums);
            }
        }
    }

    /// `[a, b] = [x, y]` and `({ a } = { a: x })`: each literal value against the declared
    /// type of the target it lands on.
    fn pattern_assignment(&mut self, left: NodeId, right: NodeId, depth: u32) {
        if depth > 32 {
            return;
        }
        let ast = self.ast();
        let left = skip_parens(ast, left);
        let right = skip_parens(ast, right);
        match (*self.cx.node(left), *self.cx.node(right)) {
            (
                Node::ArrayLit { elements: l } | Node::ArrayPattern { elements: l },
                Node::ArrayLit { elements: r },
            ) => {
                for (&le, &re) in ast.list(l).iter().zip(ast.list(r)) {
                    if matches!(self.cx.node(le), Node::Spread { .. } | Node::Rest { .. })
                        || matches!(self.cx.node(re), Node::Spread { .. })
                    {
                        return;
                    }
                    self.pattern_element(le, re, self.cx.span(re), depth);
                }
            }
            (
                Node::ObjectLit { props: l } | Node::ObjectPattern { props: l },
                Node::ObjectLit { props: r },
            ) => {
                let src = self.cx.src();
                for &rp in ast.list(r) {
                    let (rname, rvalue) = match *self.cx.node(rp) {
                        Node::Property { key, value, .. } => (key_name(ast, src, key), value),
                        Node::ShorthandProp { name, .. } => {
                            (ident_name(ast, name).map(Into::into), name)
                        }
                        _ => continue,
                    };
                    let Some(rname) = rname else { continue };
                    for &lp in ast.list(l) {
                        let (lname, ltarget) = match *self.cx.node(lp) {
                            Node::Property { key, value, .. } => (key_name(ast, src, key), value),
                            Node::ShorthandProp {
                                name, init: None, ..
                            } => (ident_name(ast, name).map(Into::into), name),
                            Node::BindingProperty {
                                key,
                                value,
                                init: None,
                            } => (key_name(ast, src, key), value.unwrap_or(key)),
                            _ => continue,
                        };
                        if lname.as_deref() == Some(&*rname) {
                            self.pattern_element(ltarget, rvalue, self.cx.span(rp), depth);
                            break;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn pattern_element(&mut self, target: NodeId, value: NodeId, span: Span, depth: u32) {
        let target = skip_parens(self.ast(), target);
        match self.cx.node(target) {
            Node::ArrayLit { .. }
            | Node::ObjectLit { .. }
            | Node::ArrayPattern { .. }
            | Node::ObjectPattern { .. } => self.pattern_assignment(target, value, depth + 1),
            _ => {
                if let Some(t) = self.declared_target(target) {
                    self.check_expr(value, t, span, Msg::Assignment, false);
                }
            }
        }
    }

    /// The declared type of an assignment target.
    fn declared_target(&self, target: NodeId) -> Option<Ty> {
        let ti = self.ti;
        let target = skip_parens(self.ast(), target);
        match self.cx.node(target) {
            Node::Ident { .. } | Node::Member { .. } => {
                ti.declared_type_at(target).or_else(|| ti.type_at(target))
            }
            Node::Index { .. } => ti.type_at(target),
            _ => None,
        }
    }

    /// The annotated return type of the function a `return` belongs to (awaited for async
    /// functions), and whether the function is async.
    fn return_target(&self, ret: NodeId) -> Option<(Ty, bool)> {
        let ti = self.ti;
        let mut n = ret;
        let mut depth = 0;
        let (flags, rt) = loop {
            n = self.cx.parent(n)?;
            depth += 1;
            if depth > 10_000 {
                return None;
            }
            match *self.cx.node(n) {
                Node::FunctionDecl {
                    flags, return_type, ..
                }
                | Node::FunctionExpr {
                    flags, return_type, ..
                }
                | Node::Arrow {
                    flags, return_type, ..
                } => break (flags, return_type?),
                Node::ClassDecl { .. } | Node::ClassExpr { .. } | Node::StaticBlock { .. } => {
                    return None;
                }
                _ => {}
            }
        };
        if flags.contains(FnFlags::GENERATOR) {
            return None;
        }
        let t = ti.type_of_type_node(rt)?;
        let is_async = flags.contains(FnFlags::ASYNC);
        Some(if is_async {
            (ti.awaited_type(t), true)
        } else {
            (t, false)
        })
    }

    /// Defaults inside a binding pattern whose type is `target`.
    fn pattern_defaults(&mut self, pattern: NodeId, target: Ty) {
        self.pattern_defaults_at(pattern, target, 0);
    }

    fn pattern_defaults_at(&mut self, pattern: NodeId, target: Ty, depth: u32) {
        if depth > 32 || !self.has_enum(target, 0) {
            return;
        }
        let ast = self.ast();
        match *self.cx.node(pattern) {
            Node::ObjectPattern { props } => {
                for &p in ast.list(props) {
                    let Node::BindingProperty { key, value, init } = *self.cx.node(p) else {
                        continue;
                    };
                    let Some(name) = key_name(ast, self.cx.src(), key) else {
                        continue;
                    };
                    let Some(pt) = self.property_target(target, &name) else {
                        continue;
                    };
                    if let Some(init) = init {
                        self.check_expr(init, pt, self.cx.span(p), Msg::Assignment, false);
                    }
                    if let Some(v) = value {
                        self.pattern_defaults_at(v, pt, depth + 1);
                    }
                }
            }
            Node::ArrayPattern { elements } => {
                for (i, &e) in ast.list(elements).iter().enumerate() {
                    let Some(et) = self.element_target(target, i) else {
                        continue;
                    };
                    match *self.cx.node(e) {
                        Node::AssignPattern { left, right } => {
                            self.check_expr(right, et, self.cx.span(e), Msg::Assignment, false);
                            self.pattern_defaults_at(left, et, depth + 1);
                        }
                        Node::Rest { .. } => break,
                        _ => self.pattern_defaults_at(e, et, depth + 1),
                    }
                }
            }
            _ => {}
        }
    }

    fn jsx_attr_target(&self, attr: NodeId, name: NodeId) -> Option<Ty> {
        let ti = self.ti;
        let open = self.cx.parent(attr)?;
        let Node::JsxOpening { name: tag, .. } = *self.cx.node(open) else {
            return None;
        };
        let attr_name = ident_name(self.ast(), name)?;
        match *self.cx.node(tag) {
            Node::Ident { .. } => {
                let tag_name = ident_name(self.ast(), tag)?;
                if tag_name.starts_with(|c: char| c.is_ascii_lowercase()) {
                    return None;
                }
            }
            Node::JsxMemberExpr { .. } | Node::Member { .. } => {}
            _ => return None,
        }
        let tt = ti.type_at(tag)?;
        let mut sigs = ti.signatures(tt, false);
        if sigs.is_empty() {
            sigs = ti.signatures(tt, true);
        }
        let &sig = sigs.first()?;
        let props = ti.param_type(sig, 0)?;
        self.property_target(props, attr_name)
    }

    fn call(&mut self, node: NodeId) {
        let ti = self.ti;
        let ast = self.ast();
        let (callee, args, construct, skip_first): (NodeId, Vec<NodeId>, bool, bool) =
            match *self.cx.node(node) {
                Node::Call { callee, args, .. } => (callee, ast.list(args).to_vec(), false, false),
                Node::New { callee, args, .. } => (callee, ast.list(args).to_vec(), true, false),
                Node::TaggedTemplate { tag, quasi, .. } => {
                    let Node::TemplateLit { parts } = *self.cx.node(quasi) else {
                        return;
                    };
                    let exprs: Vec<NodeId> = ast
                        .list(parts)
                        .iter()
                        .copied()
                        .filter(|&p| !matches!(self.cx.node(p), Node::TemplateElement { .. }))
                        .collect();
                    (tag, exprs, false, true)
                }
                _ => return,
            };
        if args.is_empty() || matches!(self.cx.node(callee), Node::Super { .. }) {
            return;
        }
        let Some(callee_t) = ti.type_at(callee) else {
            return;
        };
        if is_wild(ti, callee_t) {
            return;
        }
        let sigs = ti.signatures(callee_t, construct);
        let sig = match ti.resolved_signature(node) {
            Some(r) => {
                // The declared (uninstantiated) signature: generic parameters stay type
                // parameters and are compared through their constraints.
                let decl = ti.signature_decl(r);
                sigs.iter()
                    .copied()
                    .find(|&s| decl.is_some() && ti.signature_decl(s) == decl)
                    .unwrap_or(r)
            }
            None => match sigs[..] {
                [s] => s,
                _ => return,
            },
        };
        let mut pos = usize::from(skip_first);
        let mut exact = true;
        for arg in args {
            if let Node::Spread { arg: inner } = *self.cx.node(arg) {
                let (bad, next, still_exact) = self.spread_arg(sig, inner, pos, exact);
                if let Some(e) = bad {
                    self.push(self.cx.span(arg), Msg::Argument, e);
                }
                pos = next;
                exact = still_exact;
                continue;
            }
            let targets = self.param_targets(sig, pos, exact);
            if exact {
                if let [t] = targets[..] {
                    self.check_expr(arg, t, self.cx.span(arg), Msg::Argument, false);
                }
            } else if let Some(e) = targets.iter().find_map(|&t| self.leaf(arg, t, false)) {
                self.push(self.cx.span(arg), Msg::Argument, e);
            }
            pos += 1;
        }
    }

    /// Checks `...inner` passed at argument position `pos`: the enums of a mismatch, the next
    /// position and whether it is still exact.
    fn spread_arg(
        &mut self,
        sig: Sig,
        inner: NodeId,
        pos: usize,
        exact: bool,
    ) -> (Option<Vec<Sym>>, usize, bool) {
        let ti = self.ti;
        let ast = self.ast();
        let inner_e = skip_parens(ast, inner);
        if let Node::ArrayLit { elements } = *self.cx.node(inner_e) {
            let mut bad = None;
            let mut n = 0;
            for &el in ast.list(elements) {
                match self.cx.node(el) {
                    Node::Spread { .. } => return (bad, pos + n, false),
                    Node::Hole {} => {}
                    _ => {
                        if bad.is_none() {
                            let targets = self.param_targets(sig, pos + n, exact);
                            bad = targets.iter().find_map(|&t| self.leaf(el, t, false));
                        }
                    }
                }
                n += 1;
            }
            return (bad, pos + n, exact);
        }
        let Some(t) = ti.type_at(inner) else {
            return (None, pos, false);
        };
        if is_wild(ti, t) {
            return (None, pos, false);
        }
        let mut bad = None;
        if ti.is_tuple(t) {
            let elems = ti.type_args(t);
            let rest = ti.tuple_rest_index(t);
            let fixed = rest.map_or(elems.len(), |r| r.min(elems.len()));
            for (j, &e) in elems[..fixed].iter().enumerate() {
                if bad.is_some() {
                    break;
                }
                let targets = self.param_targets(sig, pos + j, exact);
                bad = targets.iter().find_map(|&p| self.check_types(e, p));
            }
            match rest.and_then(|r| elems.get(r).copied()) {
                Some(re) => {
                    if bad.is_none() {
                        let targets = self.param_targets(sig, pos + fixed, false);
                        bad = targets.iter().find_map(|&p| self.check_types(re, p));
                    }
                    (bad, pos + fixed, false)
                }
                None => (bad, pos + fixed, exact),
            }
        } else {
            let elem = ti.iterated_type(t, false).or_else(|| ti.array_element(t));
            if let Some(e) = elem {
                let targets = self.param_targets(sig, pos, false);
                bad = targets.iter().find_map(|&p| self.check_types(e, p));
            }
            (bad, pos, false)
        }
    }

    /// The parameter types an argument at `pos` may land on (exactly `pos`, or any position
    /// from `pos` on after a spread of unknown length).
    fn param_targets(&self, sig: Sig, pos: usize, exact: bool) -> Vec<Ty> {
        let ti = self.ti;
        if exact {
            return ti.param_type(sig, pos).into_iter().collect();
        }
        let (count, _) = ti.param_count(sig);
        let mut out: Vec<Ty> = Vec::new();
        for i in pos..count.max(pos) + 8 {
            if let Some(t) = ti.param_type(sig, i)
                && !out.contains(&t)
            {
                out.push(t);
            }
        }
        out
    }

    fn access(&mut self, obj: NodeId, index: NodeId) {
        let ti = self.ti;
        let Some(ot) = ti.type_at(obj) else {
            return;
        };
        if is_wild(ti, ot) {
            return;
        }
        let ot = ti.non_nullable(constrained(ti, ot));
        if is_wild(ti, ot)
            || !ti
                .flags(ot)
                .intersects(TypeFlags::OBJECT | TypeFlags::INTERSECTION)
        {
            return;
        }
        // Properties keyed by enum members, and plain ones.
        let mut enums: Vec<Sym> = Vec::new();
        let mut plain: Vec<String> = Vec::new();
        for p in ti.properties_of(ot) {
            let key = ti.mapped_key_type(p).or_else(|| self.computed_key_type(p));
            match key.and_then(|k| enum_of(ti, k)) {
                Some(e) => {
                    if !enums.contains(&e) {
                        enums.push(e);
                    }
                }
                None => plain.push(ti.symbol_name(p)),
            }
        }
        if enums.is_empty() {
            return;
        }
        let Some(kt) = ti.type_at(index) else {
            return;
        };
        let kt = constrained(ti, kt);
        if is_wild(ti, kt) {
            return;
        }
        let number_index = ti.index_type(ot, true).is_some();
        let string_index = ti.index_type(ot, false).is_some();
        for kp in parts(ti, kt) {
            let kp = constrained(ti, kp);
            if is_wild(ti, kp) {
                return;
            }
            if let Some(e) = enum_of(ti, kp) {
                if enums.contains(&e) {
                    continue;
                }
            } else {
                let f = ti.flags(kp);
                let number = f.intersects(TypeFlags::NUMBER_LIKE);
                let string = f.intersects(TypeFlags::STRING_LIKE);
                if !number && !string {
                    continue;
                }
                if f.intersects(TypeFlags::STRING_LITERAL | TypeFlags::NUMBER_LITERAL)
                    && ti.literal_text(kp).is_some_and(|n| plain.contains(&n))
                {
                    continue;
                }
                if string_index || (number && number_index) {
                    continue;
                }
            }
            self.push(self.cx.span(index), Msg::Access, enums);
            return;
        }
    }

    /// The type of a computed property name (`[E.A]: T`) declared in this file.
    fn computed_key_type(&self, p: Sym) -> Option<Ty> {
        let ti = self.ti;
        for d in ti.symbol_decls(p) {
            if !d.in_this_file {
                continue;
            }
            let key = match *self.cx.node(d.node) {
                Node::PropertySignature { key, .. }
                | Node::PropertyDef { key, .. }
                | Node::Property { key, .. } => key,
                Node::ComputedKey { .. } => d.node,
                _ => continue,
            };
            if let Node::ComputedKey { expr } = *self.cx.node(key) {
                return ti.type_at(expr);
            }
        }
        None
    }

    // ---- values -----------------------------------------------------------------------

    /// Checks `expr` written where `target` is expected. Object and array literals are
    /// followed into their properties and elements (reported there); anything else is
    /// reported at `span`.
    fn check_expr(&mut self, expr: NodeId, target: Ty, span: Span, msg: Msg, awaited: bool) {
        self.check_expr_at(expr, target, span, msg, awaited, 0);
    }

    fn check_expr_at(
        &mut self,
        expr: NodeId,
        target: Ty,
        span: Span,
        msg: Msg,
        awaited: bool,
        depth: u32,
    ) {
        if depth > 32 || !self.has_enum(target, 0) {
            return;
        }
        let ast = self.ast();
        let e = skip_parens(ast, expr);
        match *self.cx.node(e) {
            Node::ObjectLit { props } if self.has_object_part(target) => {
                for &p in ast.list(props) {
                    self.object_member(p, target, depth);
                }
            }
            Node::ArrayLit { elements } if self.has_array_part(target) => {
                let mut index = Some(0usize);
                for &el in ast.list(elements) {
                    match *self.cx.node(el) {
                        Node::Hole {} => {}
                        Node::Spread { arg } => {
                            let et = index
                                .and_then(|i| self.element_target(target, i))
                                .or_else(|| self.array_element_target(target));
                            if let Some(et) = et
                                && let Some(st) = self.ti.type_at(arg)
                                && let Some(elem) = self
                                    .ti
                                    .iterated_type(st, false)
                                    .or_else(|| self.ti.array_element(st))
                                && let Some(en) = self.check_types(elem, et)
                            {
                                self.push(self.cx.span(el), Msg::Assignment, en);
                            }
                            index = None;
                            continue;
                        }
                        _ => {
                            let et = match index {
                                Some(i) => self.element_target(target, i),
                                None => self.array_element_target(target),
                            };
                            if let Some(et) = et {
                                self.check_expr_at(
                                    el,
                                    et,
                                    self.cx.span(el),
                                    Msg::Assignment,
                                    false,
                                    depth + 1,
                                );
                            }
                        }
                    }
                    index = index.map(|i| i + 1);
                }
            }
            _ => {
                if let Some(en) = self.leaf(e, target, awaited) {
                    self.push(span, msg, en);
                }
            }
        }
    }

    fn object_member(&mut self, p: NodeId, target: Ty, depth: u32) {
        let ti = self.ti;
        let ast = self.ast();
        match *self.cx.node(p) {
            Node::Property { key, value, .. } => {
                let name = match *self.cx.node(key) {
                    Node::ComputedKey { expr } => match key_name(ast, self.cx.src(), key) {
                        Some(n) => Some(n.into_owned()),
                        None => ti.type_at(expr).and_then(|t| ti.literal_text(t)),
                    },
                    _ => key_name(ast, self.cx.src(), key).map(|n| n.into_owned()),
                };
                let Some(name) = name else {
                    return;
                };
                if let Some(pt) = self.property_target(target, &name) {
                    self.check_expr_at(
                        value,
                        pt,
                        self.cx.span(p),
                        Msg::Assignment,
                        false,
                        depth + 1,
                    );
                }
            }
            Node::ShorthandProp {
                name, init: None, ..
            } => {
                let Some(n) = ident_name(ast, name) else {
                    return;
                };
                if let Some(pt) = self.property_target(target, n)
                    && let Some(en) = self.leaf(name, pt, false)
                {
                    self.push(self.cx.span(p), Msg::Assignment, en);
                }
            }
            Node::Spread { arg } => {
                if let Some(st) = ti.type_at(arg)
                    && let Some(en) = self.check_types(st, target)
                {
                    self.push(self.cx.span(p), Msg::Assignment, en);
                }
            }
            _ => {}
        }
    }

    /// Checks a value expression that is not followed further.
    fn leaf(&mut self, e: NodeId, target: Ty, awaited: bool) -> Option<Vec<Sym>> {
        if !self.has_enum(target, 0) {
            return None;
        }
        let ti = self.ti;
        let enums = self.top_enums(target);
        if !enums.is_empty() && self.enum_combination(e, &enums, 0) {
            return None;
        }
        let mut src = ti.type_at(e)?;
        if awaited {
            src = ti.awaited_type(src);
        }
        self.check_types(src, target)
    }

    /// `A | B`, `A & ~B`, `A ^ B` of members of `enums` (or a value of one of them).
    fn enum_combination(&self, e: NodeId, enums: &[Sym], depth: u32) -> bool {
        if depth > 64 {
            return false;
        }
        let ti = self.ti;
        let e = skip_parens(self.ast(), e);
        match *self.cx.node(e) {
            Node::Binary {
                left,
                op: BinaryOp::BitOr | BinaryOp::BitAnd | BinaryOp::BitXor,
                right,
            } => {
                self.enum_combination(left, enums, depth + 1)
                    && self.enum_combination(right, enums, depth + 1)
            }
            Node::Unary {
                op: UnaryOp::BitNot,
                arg,
            } => self.enum_combination(arg, enums, depth + 1),
            _ => {
                let Some(t) = ti.type_at(e) else {
                    return false;
                };
                let ps = self.parts_constrained(t);
                !ps.is_empty()
                    && ps
                        .iter()
                        .all(|&p| enum_of(ti, p).is_some_and(|en| enums.contains(&en)))
            }
        }
    }

    // ---- types ------------------------------------------------------------------------

    /// Union members, with type parameters replaced by (the members of) their constraints.
    fn parts_constrained(&self, t: Ty) -> Vec<Ty> {
        let ti = self.ti;
        let mut out = Vec::new();
        for p in parts(ti, t) {
            let c = constrained(ti, p);
            if c == p {
                out.push(p);
            } else {
                for q in parts(ti, c) {
                    let q = constrained(ti, q);
                    if !out.contains(&q) {
                        out.push(q);
                    }
                }
            }
        }
        out
    }

    /// The enums among the members of `target`.
    fn top_enums(&self, target: Ty) -> Vec<Sym> {
        let mut out = Vec::new();
        for p in self.parts_constrained(target) {
            if let Some(e) = enum_of(self.ti, p)
                && !out.contains(&e)
            {
                out.push(e);
            }
        }
        out
    }

    fn has_object_part(&self, t: Ty) -> bool {
        self.parts_constrained(t).iter().any(|&p| {
            self.ti
                .flags(p)
                .intersects(TypeFlags::OBJECT | TypeFlags::INTERSECTION)
        })
    }

    fn has_array_part(&self, t: Ty) -> bool {
        let ti = self.ti;
        self.parts_constrained(t)
            .iter()
            .any(|&p| p != Ty::UNRESOLVED && (ti.is_array(p) || ti.is_tuple(p)))
    }

    /// The type of property `name` in (an object member of) `target`.
    fn property_target(&self, target: Ty, name: &str) -> Option<Ty> {
        let ti = self.ti;
        for p in self.parts_constrained(target) {
            if !ti
                .flags(p)
                .intersects(TypeFlags::OBJECT | TypeFlags::INTERSECTION)
            {
                continue;
            }
            if let Some(s) = ti.property_of_type(p, name) {
                return Some(ti.type_of_symbol(s));
            }
            let numeric = name.parse::<f64>().is_ok();
            if numeric && let Some(t) = ti.index_type(p, true) {
                return Some(t);
            }
            if let Some(t) = ti.index_type(p, false) {
                return Some(t);
            }
        }
        None
    }

    /// The type of element `i` of (an array or tuple member of) `target`.
    fn element_target(&self, target: Ty, i: usize) -> Option<Ty> {
        let ti = self.ti;
        for p in self.parts_constrained(target) {
            if p == Ty::UNRESOLVED {
                continue;
            }
            if ti.is_array(p) {
                return ti.array_element(p);
            }
            if ti.is_tuple(p) {
                let args = ti.type_args(p);
                return match ti.tuple_rest_index(p) {
                    Some(r) if i >= r => args.get(r).copied(),
                    _ => args.get(i).copied(),
                };
            }
        }
        None
    }

    /// The element type of an array member of `target` (for positions after a spread).
    fn array_element_target(&self, target: Ty) -> Option<Ty> {
        let ti = self.ti;
        self.parts_constrained(target)
            .into_iter()
            .find(|&p| p != Ty::UNRESOLVED && ti.is_array(p))
            .and_then(|p| ti.array_element(p))
    }

    /// Element types of an array (`[T]`) or tuple (its elements).
    fn element_types(&self, t: Ty) -> Vec<Ty> {
        let ti = self.ti;
        if ti.is_array(t) {
            ti.array_element(t).into_iter().collect()
        } else if ti.is_tuple(t) {
            ti.type_args(t)
        } else {
            Vec::new()
        }
    }

    fn is_function_like(&self, t: Ty) -> bool {
        let ti = self.ti;
        ti.flags(t).contains(TypeFlags::OBJECT) && !ti.signatures(t, false).is_empty()
    }

    /// Declared in a default library file (compared by type arguments only).
    fn is_lib_type(&self, t: Ty) -> bool {
        let ti = self.ti;
        let Some(s) = ti.type_symbol(t) else {
            return false;
        };
        let decls = ti.symbol_decls(s);
        !decls.is_empty() && decls.iter().all(|d| d.in_lib)
    }

    /// Whether an enum type occurs in `t` (members, elements, type arguments, properties).
    fn has_enum(&mut self, t: Ty, depth: u32) -> bool {
        if depth > MAX_DEPTH || t == Ty::UNRESOLVED {
            return false;
        }
        if let Some(&b) = self.has_enum_memo.get(&t) {
            return b;
        }
        self.has_enum_memo.insert(t, false);
        let r = self.has_enum_uncached(t, depth);
        self.has_enum_memo.insert(t, r);
        r
    }

    fn has_enum_uncached(&mut self, t: Ty, depth: u32) -> bool {
        let ti = self.ti;
        for p in self.parts_constrained(t) {
            if is_wild(ti, p) {
                continue;
            }
            if enum_of(ti, p).is_some() {
                return true;
            }
            if !ti
                .flags(p)
                .intersects(TypeFlags::OBJECT | TypeFlags::INTERSECTION)
            {
                continue;
            }
            if ti.is_array(p) || ti.is_tuple(p) {
                if self
                    .element_types(p)
                    .into_iter()
                    .any(|e| self.has_enum(e, depth + 1))
                {
                    return true;
                }
                continue;
            }
            if ti
                .type_args(p)
                .into_iter()
                .any(|a| self.has_enum(a, depth + 1))
            {
                return true;
            }
            if self.is_lib_type(p) || self.is_function_like(p) {
                continue;
            }
            for prop in ti.properties_of(p) {
                let pt = ti.type_of_symbol(prop);
                if !self.is_function_like(pt) && self.has_enum(pt, depth + 1) {
                    return true;
                }
            }
            for number in [true, false] {
                if let Some(it) = ti.index_type(p, number)
                    && self.has_enum(it, depth + 1)
                {
                    return true;
                }
            }
        }
        false
    }

    /// The enums a value of type `src` written to `target` lacks, if any.
    fn check_types(&mut self, src: Ty, target: Ty) -> Option<Vec<Sym>> {
        let mut seen = FxHashSet::default();
        self.check_types_at(src, target, 0, &mut seen)
    }

    fn check_types_at(
        &mut self,
        src: Ty,
        target: Ty,
        depth: u32,
        seen: &mut FxHashSet<(Ty, Ty)>,
    ) -> Option<Vec<Sym>> {
        let ti = self.ti;
        if depth > MAX_DEPTH || src == target || is_wild(ti, src) || is_wild(ti, target) {
            return None;
        }
        if !seen.insert((src, target)) || !self.has_enum(target, 0) {
            return None;
        }
        let tps = self.parts_constrained(target);
        if tps.iter().any(|&t| is_wild(ti, t)) {
            return None;
        }
        let enums: Vec<Sym> = {
            let mut v = Vec::new();
            for &p in &tps {
                if let Some(e) = enum_of(ti, p)
                    && !v.contains(&e)
                {
                    v.push(e);
                }
            }
            v
        };
        for sp in self.parts_constrained(src) {
            if is_wild(ti, sp) {
                return None;
            }
            let f = ti.flags(sp);
            if f.intersects(TypeFlags::NULLABLE | TypeFlags::VOID | TypeFlags::NEVER)
                || tps.contains(&sp)
            {
                continue;
            }
            if let Some(e) = enum_of(ti, sp) {
                if enums.contains(&e) {
                    continue;
                }
                if !enums.is_empty() && !self.accepted(sp, &tps) {
                    return Some(enums);
                }
                continue;
            }
            if f.intersects(TypeFlags::NUMBER_LIKE | TypeFlags::STRING_LIKE) {
                if !enums.is_empty() && !self.accepted(sp, &tps) {
                    return Some(enums);
                }
                continue;
            }
            if !f.intersects(TypeFlags::OBJECT | TypeFlags::INTERSECTION) {
                continue;
            }
            let mut first = None;
            let mut ok = false;
            for &tp in &tps {
                if !ti
                    .flags(tp)
                    .intersects(TypeFlags::OBJECT | TypeFlags::INTERSECTION)
                {
                    continue;
                }
                if !self.has_enum(tp, 0) {
                    ok = true;
                    break;
                }
                match self.structural(sp, tp, depth, seen) {
                    Some(e) => {
                        if first.is_none() {
                            first = Some(e);
                        }
                    }
                    None => {
                        ok = true;
                        break;
                    }
                }
            }
            if !ok && let Some(e) = first {
                return Some(e);
            }
        }
        None
    }

    /// A non-enum member of the target accepts `sp`.
    fn accepted(&self, sp: Ty, tps: &[Ty]) -> bool {
        let ti = self.ti;
        let sf = ti.flags(sp);
        tps.iter().any(|&tp| {
            if enum_of(ti, tp).is_some() {
                return false;
            }
            let tf = ti.flags(tp);
            if tf.contains(TypeFlags::NUMBER) && sf.intersects(TypeFlags::NUMBER_LIKE) {
                return true;
            }
            if tf.contains(TypeFlags::STRING) && sf.intersects(TypeFlags::STRING_LIKE) {
                return true;
            }
            if tf.intersects(TypeFlags::STRING_LITERAL | TypeFlags::NUMBER_LITERAL) {
                return !sf.contains(TypeFlags::ENUM_LITERAL)
                    && (tf & TypeFlags::LITERAL) == (sf & TypeFlags::LITERAL)
                    && ti.literal_text(tp).is_some()
                    && ti.literal_text(tp) == ti.literal_text(sp);
            }
            if tf.intersects(
                TypeFlags::TEMPLATE_LITERAL
                    | TypeFlags::OBJECT
                    | TypeFlags::INTERSECTION
                    | TypeFlags::NON_PRIMITIVE,
            ) {
                return ti.is_assignable(sp, tp);
            }
            false
        })
    }

    /// Compares object types `sp` and `tp`.
    fn structural(
        &mut self,
        sp: Ty,
        tp: Ty,
        depth: u32,
        seen: &mut FxHashSet<(Ty, Ty)>,
    ) -> Option<Vec<Sym>> {
        let ti = self.ti;
        let t_array = ti.is_array(tp);
        let t_tuple = ti.is_tuple(tp);
        let s_list = ti.is_array(sp) || ti.is_tuple(sp);
        if t_array || t_tuple {
            if !s_list {
                return None;
            }
            let ses = self.element_types(sp);
            if t_array {
                let te = ti.array_element(tp)?;
                return ses
                    .into_iter()
                    .find_map(|se| self.check_types_at(se, te, depth + 1, seen));
            }
            let tes = ti.type_args(tp);
            if ti.is_tuple(sp) {
                return ses
                    .into_iter()
                    .zip(tes)
                    .find_map(|(se, te)| self.check_types_at(se, te, depth + 1, seen));
            }
            let se = ses.first().copied()?;
            return tes
                .into_iter()
                .find_map(|te| self.check_types_at(se, te, depth + 1, seen));
        }
        if let (Some(a), Some(b)) = (ti.reference_target(sp), ti.reference_target(tp))
            && a == b
        {
            let sa = ti.type_args(sp);
            let ta = ti.type_args(tp);
            return sa
                .into_iter()
                .zip(ta)
                .find_map(|(s, t)| self.check_types_at(s, t, depth + 1, seen));
        }
        if self.is_function_like(tp) {
            return None;
        }
        for prop in ti.properties_of(tp) {
            let name = ti.symbol_name(prop);
            let Some(sprop) = ti.property_of_type(sp, &name) else {
                continue;
            };
            let tt = ti.type_of_symbol(prop);
            if self.is_function_like(tt) || !self.has_enum(tt, 0) {
                continue;
            }
            let st = ti.type_of_symbol(sprop);
            if let Some(e) = self.check_types_at(st, tt, depth + 1, seen) {
                return Some(e);
            }
        }
        if let Some(ti_n) = ti.index_type(tp, true) {
            let sources: Vec<Ty> = if s_list {
                self.element_types(sp)
            } else {
                ti.index_type(sp, true)
                    .or_else(|| ti.index_type(sp, false))
                    .into_iter()
                    .collect()
            };
            for s in sources {
                if let Some(e) = self.check_types_at(s, ti_n, depth + 1, seen) {
                    return Some(e);
                }
            }
        }
        if let Some(ti_s) = ti.index_type(tp, false)
            && let Some(ss) = ti.index_type(sp, false)
            && let Some(e) = self.check_types_at(ss, ti_s, depth + 1, seen)
        {
            return Some(e);
        }
        None
    }
}
