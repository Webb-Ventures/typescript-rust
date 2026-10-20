//! `@typescript-eslint/no-misused-promises`: disallow promises where they are not handled
//! (boolean conditions, void-returning callbacks, object spreads).

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-misused-promises",
        "Disallow Promises in places not designed to handle them",
        TS_REC_TC,
        Category::Correctness,
    )
    .typed(),
    create: create::<NoMisusedPromises>,
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FlagUnions {
    None,
    Strict,
    All,
}

#[derive(Debug)]
struct NoMisusedPromises {
    conditionals: bool,
    flag_unions: FlagUnions,
    spreads: bool,
    arguments: bool,
    attributes: bool,
    inherited: bool,
    properties: bool,
    returns: bool,
    variables: bool,
}

impl FromOptions for NoMisusedPromises {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        let (conditionals, flag_unions) = match o.get("checksConditionals") {
            Some(Value::Bool(b)) => (*b, FlagUnions::None),
            Some(Value::Object(c)) => (
                true,
                match c.get("flagUnions").and_then(Value::as_str) {
                    Some("strict") => FlagUnions::Strict,
                    Some("all") => FlagUnions::All,
                    _ => FlagUnions::None,
                },
            ),
            _ => (true, FlagUnions::None),
        };
        let void = |k: &str| match o.get("checksVoidReturn") {
            Some(Value::Bool(b)) => *b,
            Some(Value::Object(c)) => c.get(k).and_then(Value::as_bool).unwrap_or(true),
            _ => true,
        };
        Ok(NoMisusedPromises {
            conditionals,
            flag_unions,
            spreads: bool_field(o, "checksSpreads", true),
            arguments: void("arguments"),
            attributes: void("attributes"),
            inherited: void("inheritedMethods"),
            properties: void("properties"),
            returns: void("returns"),
            variables: void("variables"),
        })
    }
}

const CONDITIONAL: &str = "Expected non-Promise value in a boolean conditional.";
const PREDICATE: &str = "Expected a non-Promise value to be returned.";
const SPREAD: &str = "Expected a non-Promise value to be spread in an object.";
const ARGUMENT: &str = "Promise returned in function argument where a void return was expected.";
const VARIABLE: &str =
    "Promise-returning function provided to variable where a void return was expected.";
const PROPERTY: &str =
    "Promise-returning function provided to property where a void return was expected.";
const RETURN_VALUE: &str =
    "Promise-returning function provided to return value where a void return was expected.";
const ATTRIBUTE: &str =
    "Promise-returning function provided to attribute where a void return was expected.";

impl Rule for NoMisusedPromises {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::If,
            NodeKind::While,
            NodeKind::DoWhile,
            NodeKind::For,
            NodeKind::Conditional,
            NodeKind::Unary,
            NodeKind::Binary,
            NodeKind::Call,
            NodeKind::New,
            NodeKind::VarDeclarator,
            NodeKind::Assign,
            NodeKind::ObjectLit,
            NodeKind::Return,
            NodeKind::Arrow,
            NodeKind::JsxAttr,
            NodeKind::ClassDecl,
            NodeKind::ClassExpr,
            NodeKind::Interface,
        ]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        match *cx.node(node) {
            Node::If { test, .. }
            | Node::While { test, .. }
            | Node::DoWhile { test, .. }
            | Node::Conditional { test, .. }
                if self.conditionals =>
            {
                self.check_test(cx, test);
            }
            Node::For {
                test: Some(test), ..
            } if self.conditionals => self.check_test(cx, test),
            Node::Unary {
                op: UnaryOp::Not,
                arg,
            } if self.conditionals => self.check_test(cx, arg),
            Node::Binary {
                left,
                op: BinaryOp::LogicalAnd | BinaryOp::LogicalOr,
                ..
            } if self.conditionals => {
                if in_test_chain(cx, node) {
                    return;
                }
                let l = skip_parens(cx.ast(), left);
                if !is_logical(cx.node(l)) {
                    self.check_value(cx, l);
                }
            }
            Node::Call { callee, args, .. } => {
                if self.conditionals {
                    self.check_predicate(cx, callee, args);
                }
                if self.arguments {
                    self.check_arguments(cx, node, callee, args, false);
                }
            }
            Node::New {
                callee,
                args,
                has_args: true,
                ..
            } if self.arguments => self.check_arguments(cx, node, callee, args, true),
            Node::VarDeclarator {
                name,
                type_ann,
                init: Some(init),
                ..
            } if self.variables => {
                let ti = cx.types();
                let Some(vt) = ti.type_at(init) else { return };
                let dispose = ti.well_known_symbol("dispose");
                let dispose_returns_thenable = || {
                    dispose.as_deref().is_some_and(|d| {
                        parts(ti, vt).into_iter().any(|p| {
                            ti.property_of_type(p, d)
                                .is_some_and(|s| returns_thenable(ti, ti.type_of_symbol(s)))
                        })
                    })
                };
                // `using x = v` disposes `v` with a void-returning `[Symbol.dispose]()`.
                if is_using(cx, node, false) {
                    if dispose_returns_thenable() {
                        cx.report_node(init, VARIABLE).id("voidReturnVariable");
                    }
                    return;
                }
                if type_ann.is_none() {
                    return;
                }
                let Some(t) = ti.type_at(name) else { return };
                let flagged = if expects_void_fn(ti, &[t]) {
                    returns_thenable(ti, vt)
                } else {
                    dispose.as_deref().is_some_and(|d| {
                        let target: Vec<Ty> = parts(ti, ti.non_nullable(t))
                            .into_iter()
                            .filter_map(|p| ti.property_of_type(p, d))
                            .map(|s| ti.type_of_symbol(s))
                            .collect();
                        !target.is_empty()
                            && expects_void_fn(ti, &target)
                            && dispose_returns_thenable()
                    })
                };
                if flagged {
                    cx.report_node(init, VARIABLE).id("voidReturnVariable");
                }
            }
            Node::Assign {
                left,
                op: AssignOp::Assign,
                right,
            } if self.variables => {
                let ti = cx.types();
                if let Some(t) = ti.type_at(left)
                    && expects_void_fn(ti, &[t])
                    && returns_thenable_value(ti, right)
                {
                    cx.report_node(right, VARIABLE).id("voidReturnVariable");
                }
            }
            Node::ObjectLit { props } => {
                if self.spreads {
                    self.check_spreads(cx, props);
                }
                if self.properties {
                    self.check_properties(cx, node, props);
                }
            }
            Node::Return { arg: Some(arg) } if self.returns => self.check_return(cx, node, arg),
            Node::Arrow { flags, body, .. }
                if self.returns && flags.contains(FnFlags::EXPRESSION_BODY) =>
            {
                self.check_return(cx, body, body)
            }
            Node::JsxAttr {
                name,
                value: Some(v),
            } if self.attributes => {
                let Node::JsxExprContainer { expr: Some(e) } = *cx.node(v) else {
                    return;
                };
                let ti = cx.types();
                if let Some(ctx) = ti
                    .contextual_type(e)
                    .or_else(|| jsx_attr_type(cx, node, name))
                    && expects_void_fn(ti, &[ctx])
                    && returns_thenable_value(ti, e)
                {
                    cx.report_node(v, ATTRIBUTE).id("voidReturnAttribute");
                }
            }
            Node::ClassDecl {
                heritage, members, ..
            }
            | Node::ClassExpr {
                heritage, members, ..
            } if self.inherited => {
                let bases = class_bases(cx, node, heritage);
                self.check_inherited(cx, &bases, members);
            }
            Node::Interface { extends, body, .. } if self.inherited => {
                let ti = cx.types();
                let bases: Vec<Ty> = cx
                    .ast()
                    .list(extends)
                    .iter()
                    .filter_map(|&b| ti.type_of_type_node(b))
                    .collect();
                self.check_inherited(cx, &bases, body);
            }
            _ => {}
        }
    }
}

/// The value of `e` may be a thenable; conditional branches and logical operands are looked
/// at separately because the checker reduces `cond ? {} : promise` to `{}`.
fn sometimes_thenable_expr(cx: &Ctx<'_, '_>, e: NodeId, depth: u32) -> bool {
    if depth > 64 {
        return false;
    }
    let e = skip_parens(cx.ast(), e);
    let ti = cx.types();
    if let Some(t) = ti.type_at(e)
        && thenable(ti, constrained(ti, t)) == Some(true)
    {
        return true;
    }
    match *cx.node(e) {
        Node::Conditional { cons, alt, .. } => {
            sometimes_thenable_expr(cx, cons, depth + 1)
                || sometimes_thenable_expr(cx, alt, depth + 1)
        }
        Node::Binary {
            left,
            op: BinaryOp::LogicalAnd | BinaryOp::LogicalOr | BinaryOp::Coalesce,
            right,
        } => {
            sometimes_thenable_expr(cx, left, depth + 1)
                || sometimes_thenable_expr(cx, right, depth + 1)
        }
        _ => false,
    }
}

fn is_logical(n: &Node) -> bool {
    matches!(
        n,
        Node::Binary {
            op: BinaryOp::LogicalAnd | BinaryOp::LogicalOr | BinaryOp::Coalesce,
            ..
        }
    )
}

/// `node` (a logical expression) is part of a test expression that [`check_test`] walks.
fn in_test_chain(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let mut cur = node;
    for _ in 0..10_000 {
        let Some(p) = cx.parent(cur) else {
            return false;
        };
        match *cx.node(p) {
            Node::Paren { .. } => {}
            Node::Binary {
                op: BinaryOp::LogicalAnd | BinaryOp::LogicalOr,
                ..
            } => {}
            Node::Binary {
                op: BinaryOp::Coalesce,
                right,
                ..
            } if right == cur => {}
            Node::If { test, .. }
            | Node::While { test, .. }
            | Node::DoWhile { test, .. }
            | Node::Conditional { test, .. } => return test == cur,
            Node::For { test, .. } => return test == Some(cur),
            Node::Unary {
                op: UnaryOp::Not, ..
            } => return true,
            _ => return false,
        }
        cur = p;
    }
    false
}

/// `declarator` is in a `using` declaration (or `await using` when `with_await`).
fn is_using(cx: &Ctx<'_, '_>, declarator: NodeId, with_await: bool) -> bool {
    cx.parent(declarator).is_some_and(|p| match cx.node(p) {
        Node::VarDecl { kind, .. } => {
            *kind == VarKind::Using || (with_await && *kind == VarKind::AwaitUsing)
        }
        _ => false,
    })
}

/// `promise.finally(..)` (also `promise[name]` where `name` is the literal `"finally"`): its
/// callback's result is awaited.
fn is_thenable_finally(cx: &Ctx<'_, '_>, callee: NodeId) -> bool {
    let ast = cx.ast();
    let ti = cx.types();
    let callee = skip_parens(ast, callee);
    let (obj, named) = match *cx.node(callee) {
        Node::Member { obj, prop, .. } => (obj, ident_name(ast, prop) == Some("finally")),
        Node::Index { obj, index, .. } => (
            obj,
            ti.type_at(index)
                .and_then(|t| ti.literal_text(t))
                .is_some_and(|s| s == "finally"),
        ),
        _ => return false,
    };
    named && ti.type_at(obj).is_some_and(|t| is_thenable(ti, t))
}

/// Some call signature of `t` returns a type with a thenable member.
fn returns_thenable(ti: &dyn TypeInfo, t: Ty) -> bool {
    for p in intersection_flat(ti, t) {
        for s in ti.signatures(p, false) {
            if thenable(ti, ti.return_type(s)) == Some(true) {
                return true;
            }
        }
    }
    false
}

/// The value at `node` is a function returning a thenable.
fn returns_thenable_value(ti: &dyn TypeInfo, node: NodeId) -> bool {
    ti.type_at(node).is_some_and(|t| returns_thenable(ti, t))
}

/// Union members of `t` with intersections flattened.
fn intersection_flat(ti: &dyn TypeInfo, t: Ty) -> Vec<Ty> {
    let mut out = Vec::new();
    for p in parts(ti, t) {
        if ti.flags(p).contains(TypeFlags::INTERSECTION) {
            out.extend(ti.intersection_members(p));
        } else {
            out.push(p);
        }
    }
    out
}

/// Some call signature of `t` returns `void` (or a union with `void`).
fn returns_void(ti: &dyn TypeInfo, t: Ty) -> bool {
    for p in intersection_flat(ti, t) {
        for s in ti.signatures(p, false) {
            let r = ti.return_type(s);
            if parts(ti, r)
                .into_iter()
                .any(|x| x != Ty::UNRESOLVED && ti.flags(x).contains(TypeFlags::VOID))
            {
                return true;
            }
        }
    }
    false
}

/// Some candidate expects a void-returning function and none accepts a thenable-returning
/// one.
fn expects_void_fn(ti: &dyn TypeInfo, candidates: &[Ty]) -> bool {
    let mut void = false;
    for &c in candidates {
        if c == Ty::UNRESOLVED || is_any(ti, c) {
            continue;
        }
        if returns_thenable(ti, c) {
            return false;
        }
        if returns_void(ti, c) {
            void = true;
        }
    }
    void
}

impl NoMisusedPromises {
    /// A boolean test: logical operands are tests too.
    fn check_test(&self, cx: &mut Ctx<'_, '_>, e: NodeId) {
        let e = skip_parens(cx.ast(), e);
        match *cx.node(e) {
            Node::Binary {
                left,
                op: BinaryOp::LogicalAnd | BinaryOp::LogicalOr,
                right,
            } => {
                self.check_test(cx, left);
                self.check_test(cx, right);
            }
            Node::Binary {
                op: BinaryOp::Coalesce,
                right,
                ..
            } => self.check_test(cx, right),
            _ => self.check_value(cx, e),
        }
    }

    fn check_value(&self, cx: &mut Ctx<'_, '_>, e: NodeId) {
        let ti = cx.types();
        let Some(t) = ti.type_at(e) else { return };
        if self.flagged(ti, t) {
            cx.report_node(e, CONDITIONAL).id("conditional");
        }
    }

    fn flagged(&self, ti: &dyn TypeInfo, t: Ty) -> bool {
        let t = constrained(ti, t);
        let ps = parts(ti, t);
        let mut promises = Vec::new();
        let mut others = Vec::new();
        for &p in &ps {
            match thenable(ti, p) {
                None => return false,
                Some(true) => promises.push(p),
                Some(false) => others.push(p),
            }
        }
        if promises.is_empty() {
            return false;
        }
        if others.is_empty() {
            return true;
        }
        match self.flag_unions {
            FlagUnions::None => false,
            FlagUnions::All => true,
            FlagUnions::Strict => {
                let mut awaited: Vec<Ty> = Vec::new();
                for p in promises {
                    awaited.extend(parts(ti, ti.awaited_type(p)));
                }
                let same =
                    |a: Ty, b: Ty| a == b || (ti.is_assignable(a, b) && ti.is_assignable(b, a));
                awaited.iter().all(|&a| others.iter().any(|&o| same(a, o)))
                    && others.iter().all(|&o| awaited.iter().any(|&a| same(a, o)))
            }
        }
    }

    /// `arr.filter(async () => ..)` and the other array predicate methods.
    fn check_predicate(&self, cx: &mut Ctx<'_, '_>, callee: NodeId, args: ts_r_ast::NodeList) {
        let ast = cx.ast();
        let callee = skip_parens(ast, callee);
        let Node::Member { obj, prop, .. } = *cx.node(callee) else {
            return;
        };
        if !matches!(
            ident_name(ast, prop),
            Some("filter" | "find" | "findIndex" | "findLast" | "findLastIndex" | "some" | "every")
        ) {
            return;
        }
        let Some(&arg) = ast.list(args).first() else {
            return;
        };
        if matches!(cx.node(arg), Node::Spread { .. }) {
            return;
        }
        let ti = cx.types();
        let Some(ot) = ti.type_at(obj) else { return };
        let arrays = parts(ti, constrained(ti, ot));
        let array_like = !arrays.is_empty()
            && arrays.iter().all(|&p| {
                intersection_flat(ti, p)
                    .into_iter()
                    .any(|q| ti.is_array(q) || ti.is_tuple(q))
            });
        if array_like && returns_thenable_value(ti, arg) {
            cx.report_node(arg, PREDICATE).id("predicate");
        }
    }

    fn check_arguments(
        &self,
        cx: &mut Ctx<'_, '_>,
        call: NodeId,
        callee: NodeId,
        args: ts_r_ast::NodeList,
        construct: bool,
    ) {
        let ast = cx.ast();
        let ti = cx.types();
        if is_thenable_finally(cx, callee) {
            return;
        }
        let Some(ct) = ti.type_at(callee) else { return };
        let mut sigs: Vec<Sig> = Vec::new();
        for p in intersection_flat(ti, ti.non_nullable(ct)) {
            for s in ti.signatures(p, construct) {
                if ti.signature_type_params(s).is_empty() {
                    sigs.push(s);
                }
            }
        }
        if let Some(r) = ti.resolved_signature(call)
            && !sigs.contains(&r)
        {
            sigs.push(r);
        }
        if sigs.is_empty() {
            return;
        }
        for (i, &arg) in ast.list(args).iter().enumerate() {
            let (value_ty, spread) = match *cx.node(arg) {
                Node::Spread { arg: a } => {
                    let Some(t) = ti.type_at(a) else { break };
                    (ti.iterated_type(t, false), true)
                }
                _ => (ti.type_at(arg), false),
            };
            let Some(vt) = value_ty else {
                if spread {
                    break;
                }
                continue;
            };
            if returns_thenable(ti, vt) {
                let candidates: Vec<Ty> =
                    sigs.iter().filter_map(|&s| ti.param_type(s, i)).collect();
                if expects_void_fn(ti, &candidates) {
                    cx.report_node(arg, ARGUMENT).id("voidReturnArgument");
                }
            }
            if spread {
                break;
            }
        }
    }

    fn check_spreads(&self, cx: &mut Ctx<'_, '_>, props: ts_r_ast::NodeList) {
        let ast = cx.ast();
        for &p in ast.list(props) {
            let Node::Spread { arg } = *cx.node(p) else {
                continue;
            };
            let arg = skip_parens(ast, arg);
            if sometimes_thenable_expr(cx, arg, 0) {
                cx.report_node(arg, SPREAD).id("spread");
            }
        }
    }

    fn check_properties(&self, cx: &mut Ctx<'_, '_>, obj: NodeId, props: ts_r_ast::NodeList) {
        let ti = cx.types();
        let Some(ctx) = ti.contextual_type(obj) else {
            return;
        };
        let ctx_parts: Vec<Ty> = parts(ti, ti.non_nullable(ctx));
        let ast = cx.ast();
        let src = cx.src();
        for &p in ast.list(props) {
            let (key, value) = match *cx.node(p) {
                Node::Property { key, value, .. } => (key, value),
                Node::ShorthandProp { name, .. } => (name, name),
                Node::MethodDef { key, value, .. } => (key, value),
                _ => continue,
            };
            let Some(name) = static_key_name(ast, src, key) else {
                continue;
            };
            let mut candidates = Vec::new();
            for &c in &ctx_parts {
                if let Some(s) = ti.property_of_type(c, &name) {
                    candidates.push(ti.type_of_symbol(s));
                } else if let Some(t) = ti.index_type(c, false) {
                    candidates.push(t);
                }
            }
            if !expects_void_fn(ti, &candidates) {
                continue;
            }
            let value_ty = match *cx.node(p) {
                Node::ShorthandProp { name, .. } => ti.type_at(name),
                _ => ti.type_at(value),
            };
            if !value_ty.is_some_and(|t| returns_thenable(ti, t)) {
                continue;
            }
            let span = property_report_span(cx, p, value);
            cx.report(span, PROPERTY).id("voidReturnProperty");
        }
    }

    fn check_return(&self, cx: &mut Ctx<'_, '_>, at: NodeId, value: NodeId) {
        let Some(func) = ancestors_inclusive(cx.file.parents, at)
            .skip(1)
            .find(|&a| is_function(cx.node(a).kind()))
        else {
            return;
        };
        let (is_async, has_return_type) = match *cx.node(func) {
            Node::FunctionDecl {
                flags, return_type, ..
            }
            | Node::FunctionExpr {
                flags, return_type, ..
            }
            | Node::Arrow {
                flags, return_type, ..
            } => (flags.contains(FnFlags::ASYNC), return_type.is_some()),
            _ => return,
        };
        if is_async {
            return;
        }
        let ti = cx.types();
        // The function's declared return type, or the return type its context expects.
        let ret = if has_return_type {
            ti.type_at(func)
                .and_then(|ft| ti.signatures(ft, false).first().map(|&s| ti.return_type(s)))
        } else {
            ti.contextual_type(func).and_then(|ct| {
                ti.signatures(ti.non_nullable(ct), false)
                    .first()
                    .map(|&s| ti.return_type(s))
            })
        };
        let Some(ret) = ret else { return };
        if expects_void_fn(ti, &[ret]) && returns_thenable_value(ti, value) {
            cx.report_node(value, RETURN_VALUE)
                .id("voidReturnReturnValue");
        }
    }

    fn check_inherited(&self, cx: &mut Ctx<'_, '_>, bases: &[Ty], members: ts_r_ast::NodeList) {
        if bases.is_empty() {
            return;
        }
        let ti = cx.types();
        let base_types: Vec<(Ty, String)> = bases
            .iter()
            .filter_map(|&t| {
                if t == Ty::UNRESOLVED || is_any(ti, t) {
                    return None;
                }
                Some((t, ti.type_to_string(t)))
            })
            .collect();
        if base_types.is_empty() {
            return;
        }
        let ast = cx.ast();
        let src = cx.src();
        for &m in ast.list(members) {
            let (key, mods) = match *cx.node(m) {
                Node::MethodDef {
                    key,
                    modifiers,
                    kind: MethodKind::Method,
                    ..
                } => (key, modifiers),
                Node::PropertyDef { key, modifiers, .. } => (key, modifiers),
                Node::MethodSignature { key, modifiers, .. } => (key, modifiers),
                Node::PropertySignature { key, modifiers, .. } => (key, modifiers),
                _ => continue,
            };
            if mods.contains(Modifiers::STATIC) {
                continue;
            }
            let Some(name) = static_key_name(ast, src, key) else {
                continue;
            };
            let Some(mt) = ti.type_at(m) else { continue };
            if !returns_thenable(ti, mt) {
                continue;
            }
            for (bt, bname) in &base_types {
                let Some(s) = ti.property_of_type(*bt, &name) else {
                    continue;
                };
                let pt = ti.type_of_symbol(s);
                if expects_void_fn(ti, &[pt]) {
                    cx.report_node(
                        m,
                        format!(
                            "Promise-returning method provided where a void return was expected by extended/implemented type '{bname}'."
                        ),
                    )
                    .id("voidReturnInheritedMethod");
                }
            }
        }
    }
}

/// The types a class extends (the base instance type) and implements.
pub(crate) fn class_bases(
    cx: &Ctx<'_, '_>,
    class: NodeId,
    heritage: ts_r_ast::NodeList,
) -> Vec<Ty> {
    let ast = cx.ast();
    let ti = cx.types();
    let mut out = Vec::new();
    for &h in ast.list(heritage) {
        let Node::HeritageClause { kind, types } = *cx.node(h) else {
            continue;
        };
        match kind {
            ts_r_ast::HeritageKind::Extends => {
                let name = match *cx.node(class) {
                    Node::ClassDecl { name, .. } | Node::ClassExpr { name, .. } => name,
                    _ => None,
                };
                let sym = name
                    .and_then(|n| ti.symbol_at(n))
                    .or_else(|| ti.type_at(class).and_then(|t| ti.type_symbol(t)));
                let base = sym
                    .and_then(|s| ti.declared_type_of_symbol(s))
                    .and_then(|inst| ti.base_types(inst).first().copied())
                    .or_else(|| {
                        let &first = ast.list(types).first()?;
                        ti.type_of_type_node(first)
                    });
                if let Some(b) = base {
                    out.push(b);
                }
            }
            ts_r_ast::HeritageKind::Implements => {
                out.extend(
                    ast.list(types)
                        .iter()
                        .filter_map(|&t| ti.type_of_type_node(t)),
                );
            }
        }
    }
    out
}

/// The declared type of JSX attribute `attr` from a function component's props parameter.
pub(crate) fn jsx_attr_type(cx: &Ctx<'_, '_>, attr: NodeId, name: NodeId) -> Option<Ty> {
    let open = cx.parent(attr)?;
    let Node::JsxOpening { name: tag, .. } = *cx.node(open) else {
        return None;
    };
    let ast = cx.ast();
    let attr_name = ident_name(ast, name)?;
    // An intrinsic element (`<div>`): its props come from `JSX.IntrinsicElements` (the
    // factory's `JSX` namespace, else the global one), `key` and the like from
    // `JSX.IntrinsicAttributes`.
    if let Node::Ident { .. } = *cx.node(tag) {
        let tag_name = cx.span(tag).text(cx.src());
        if tag_name.starts_with(|c: char| c.is_ascii_lowercase()) || tag_name.contains('-') {
            let ti = cx.types();
            let jsx = ti.jsx_namespace(tag)?;
            let elements = ti.export_of(jsx, "IntrinsicElements")?;
            let et = ti.declared_type_of_symbol(elements)?;
            let props = ti.type_of_symbol(ti.property_of_type(et, tag_name)?);
            if let Some(p) = ti.property_of_type(props, attr_name) {
                return Some(ti.type_of_symbol(p));
            }
            let attrs = ti.export_of(jsx, "IntrinsicAttributes")?;
            let at = ti.declared_type_of_symbol(attrs)?;
            return ti
                .property_of_type(at, attr_name)
                .map(|p| ti.type_of_symbol(p));
        }
    }
    if !matches!(
        cx.node(tag),
        Node::Ident { .. } | Node::JsxMemberExpr { .. } | Node::Member { .. }
    ) {
        return None;
    }
    let ti = cx.types();
    let tt = ti.type_at(tag)?;
    let mut out = None;
    for s in ti.signatures(tt, false) {
        if !ti.signature_type_params(s).is_empty() {
            return None;
        }
        let props = ti.param_type(s, 0)?;
        let p = ti.property_of_type(props, attr_name)?;
        let t = ti.type_of_symbol(p);
        if out.is_some_and(|o| o != t) {
            return None;
        }
        out = Some(t);
    }
    out
}

/// Where a property whose value is a promise-returning function is reported: the return
/// type annotation when there is one; for a function value, from the property start to its
/// parameter list; else the value (or the key of a shorthand property).
fn property_report_span(cx: &Ctx<'_, '_>, prop: NodeId, value: NodeId) -> Span {
    let ast = cx.ast();
    let value_inner = skip_parens(ast, value);
    let (params, return_type) = match *cx.node(value_inner) {
        Node::FunctionExpr {
            params,
            return_type,
            ..
        }
        | Node::Arrow {
            params,
            return_type,
            ..
        } => (Some(params), return_type),
        _ => (None, None),
    };
    if let Some(rt) = return_type {
        // Report the type itself, after any `:` the span may start with.
        let span = cx.span(rt);
        let src = cx.src();
        let start = skip_trivia_forward(
            src,
            span.start as usize + usize::from(src[span.start as usize..].starts_with(':')),
        ) as u32;
        return Span::new(start, span.end);
    }
    let Some(params) = params else {
        if let Node::ShorthandProp { name, .. } = *cx.node(prop) {
            return cx.span(name);
        }
        return cx.span(value);
    };
    let start = cx.span(prop).start;
    let open = match ast.list(params).first() {
        Some(&p) => find_char(cx.src(), start, b'(').filter(|&o| o <= cx.span(p).start),
        None => find_char(cx.src(), start, b'('),
    };
    match open {
        Some(o) => Span::new(start, o),
        None => cx.span(value),
    }
}
