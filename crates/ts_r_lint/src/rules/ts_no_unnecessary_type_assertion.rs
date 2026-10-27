//! `@typescript-eslint/no-unnecessary-type-assertion`: disallow type assertions (`as T`,
//! `<T>x`, `x!`) that do not change the type of the expression.

use super::prelude::*;
use super::ts_no_misused_promises::jsx_attr_type;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-unnecessary-type-assertion",
        "Disallow type assertions that do not change the type of an expression",
        TS_REC_TC,
        Category::Suspicious,
    )
    .typed()
    .fixable(),
    create: create::<NoUnnecessaryTypeAssertion>,
};

#[derive(Debug)]
struct NoUnnecessaryTypeAssertion {
    types_to_ignore: Vec<String>,
    check_literal_const: bool,
}

impl FromOptions for NoUnnecessaryTypeAssertion {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(NoUnnecessaryTypeAssertion {
            types_to_ignore: str_list(o, "typesToIgnore"),
            check_literal_const: bool_field(o, "checkLiteralConstAssertions", false),
        })
    }
}

const UNNECESSARY: &str =
    "This assertion is unnecessary since it does not change the type of the expression.";
const CONTEXTUAL: &str =
    "This assertion is unnecessary since the receiver accepts the original type of the expression.";

impl Rule for NoUnnecessaryTypeAssertion {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::NonNull, NodeKind::As, NodeKind::TypeAssertion]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        match *cx.node(node) {
            Node::NonNull { expr } => self.non_null(cx, node, expr),
            Node::As { expr, ty } | Node::TypeAssertion { ty, expr } => {
                self.assertion(cx, node, expr, ty)
            }
            _ => {}
        }
    }
}

impl NoUnnecessaryTypeAssertion {
    fn non_null(&self, cx: &mut Ctx<'_, '_>, node: NodeId, expr: NodeId) {
        let ti = cx.types();
        let Some(t) = constrained_type_at(ti, expr) else {
            return;
        };
        if has_unresolved_part(ti, t) {
            return;
        }
        let fix = || {
            let end = cx.span(node).end;
            Fix::delete(Span::new(end - 1, end))
        };
        let nullish = nullish_flags(ti, t);
        let opaque = is_any(ti, t) || is_unknown(ti, t);
        // `y! = 0`: the target's declared type is what is written to.
        if let Some(p) = cx.parent(node)
            && let Node::Assign {
                left,
                op: AssignOp::Assign,
                ..
            } = *cx.node(p)
            && left == node
        {
            let declared = ti
                .declared_type_at(skip_parens(cx.ast(), expr))
                .unwrap_or(t);
            if !nullish_flags(ti, declared).is_empty() {
                let f = fix();
                cx.report_node(node, CONTEXTUAL)
                    .id("contextuallyUnnecessary")
                    .fix(f);
            }
            return;
        }
        if nullish.is_empty() && !opaque {
            if possibly_unassigned(cx, expr) {
                return;
            }
            let f = fix();
            cx.report_node(node, UNNECESSARY)
                .id("unnecessaryAssertion")
                .fix(f);
            return;
        }
        let Some(r) = receiver(cx, node, true, 0) else {
            return;
        };
        if r == Ty::UNRESOLVED || has_unresolved_part(ti, r) {
            return;
        }
        // Any receiver accepts an `any` (or error-typed) value; an error-typed receiver (an
        // argument of an unresolved callee) does not accept a nullable one (observed).
        let accepts = if is_any(ti, t) {
            true
        } else if ti.is_error(r) {
            false
        } else if is_any(ti, r) || is_unknown(ti, r) {
            true
        } else if opaque {
            false
        } else {
            let rf = nullish_flags(ti, r);
            nullish.iter().all(|f| {
                rf.contains(f) || (*f == TypeFlags::UNDEFINED && rf.contains(&TypeFlags::VOID))
            })
        };
        if accepts {
            let f = fix();
            cx.report_node(node, CONTEXTUAL)
                .id("contextuallyUnnecessary")
                .fix(f);
        }
    }

    fn assertion(&self, cx: &mut Ctx<'_, '_>, node: NodeId, expr: NodeId, ty: NodeId) {
        let ast = cx.ast();
        let ti = cx.types();
        if matches!(
            cx.node(ty),
            Node::KeywordType {
                kind: KeywordTypeKind::Const
            }
        ) {
            if !self.check_literal_const || !const_context(cx, node) {
                return;
            }
            if let Node::TemplateLit { parts } = *cx.node(skip_parens(ast, expr))
                && ast.list(parts).len() > 1
            {
                return;
            }
            let Some(t) = ti.type_at(expr) else { return };
            if is_literal(ti, t) {
                let f = self.fix(cx, node, expr, false);
                cx.report_node(node, UNNECESSARY)
                    .id("unnecessaryAssertion")
                    .fix(f);
            }
            return;
        }
        let ty_text = cx.span(ty).text(cx.src());
        if self.types_to_ignore.iter().any(|t| t == ty_text) {
            return;
        }
        // `x as unknown as T` / `x as any as T`: compare `x` with `T`.
        let mut inner = skip_parens(ast, expr);
        let mut double = false;
        for _ in 0..64 {
            match *cx.node(inner) {
                Node::As { expr: e, ty: t } | Node::TypeAssertion { ty: t, expr: e }
                    if matches!(
                        cx.node(t),
                        Node::KeywordType {
                            kind: KeywordTypeKind::Any | KeywordTypeKind::Unknown
                        }
                    ) =>
                {
                    inner = skip_parens(ast, e);
                    double = true;
                }
                _ => break,
            }
        }
        if inferred_from_assertion(cx, inner) {
            cx.report_node(
                node,
                "The type arguments for this generic call may be inferred from the assertion. Specify them explicitly instead.",
            )
            .id("contextuallyInferredTypeArguments");
            return;
        }
        let Some(cast) = ti.type_of_type_node(ty) else {
            return;
        };
        let Some(uncast) = ti.type_at(inner) else {
            return;
        };
        if has_unresolved_part(ti, cast) || has_unresolved_part(ti, uncast) {
            return;
        }
        // Asserting a value of an unresolved type to that same unresolved type changes
        // nothing (observed: `declare const foo: Foo; <Foo>foo` is reported, an error-typed
        // `member.id as TSESTree.StringLiteral` is not).
        if ti.is_error(cast) && cast == uncast {
            let f = self.fix(cx, node, inner, double);
            cx.report_node(node, UNNECESSARY)
                .id("unnecessaryAssertion")
                .fix(f);
            return;
        }
        if ti.is_error(cast) || ti.is_error(uncast) {
            return;
        }
        // An array literal asserted to a tuple is typed as that tuple.
        if ti.is_tuple(cast) && matches!(cx.node(inner), Node::ArrayLit { .. }) {
            return;
        }
        // `NonNullable<T>` (`T & {}`) of a `T` whose constraint is never nullish is `T`
        // (observed: `node as NonNullable<T>` with `T extends Node`).
        let non_nullable_of_uncast = ti.flags(cast).contains(TypeFlags::INTERSECTION)
            && ti.flags(uncast).contains(TypeFlags::TYPE_PARAMETER)
            && {
                let ms = ti.intersection_members(cast);
                ms.len() == 2
                    && ms.contains(&uncast)
                    && ms.iter().any(|&m| {
                        m != uncast
                            && !ti.object_flags(m).contains(ObjectFlags::MAPPED)
                            && is_empty_object(ti, m)
                    })
            }
            && {
                let c = constrained(ti, uncast);
                c != Ty::UNRESOLVED
                    && !is_unknown(ti, c)
                    && !is_any(ti, c)
                    && nullish_flags(ti, c).is_empty()
            };
        if non_nullable_of_uncast || same_type(ti, uncast, cast, 0) {
            if is_empty_object(ti, cast) {
                return;
            }
            if is_literal(ti, cast) && !const_context(cx, node) {
                return;
            }
            let f = self.fix(cx, node, inner, double);
            cx.report_node(node, UNNECESSARY)
                .id("unnecessaryAssertion")
                .fix(f);
            return;
        }
        if is_any(ti, uncast) || ti.flags(uncast).intersects(TypeFlags::INSTANTIABLE) {
            return;
        }
        let Some(r) = receiver(cx, node, false, 0) else {
            return;
        };
        if r == Ty::UNRESOLVED
            || has_unresolved_part(ti, r)
            || ti.flags(r).intersects(TypeFlags::INSTANTIABLE)
        {
            return;
        }
        if !is_unsafe_assignment(ti, uncast, r) && ti.is_assignable(uncast, r) {
            let f = self.fix(cx, node, inner, double);
            cx.report_node(node, CONTEXTUAL)
                .id("contextuallyUnnecessary")
                .fix(f);
        }
    }

    /// Removes the assertion around `inner`.
    fn fix(&self, cx: &Ctx<'_, '_>, node: NodeId, inner: NodeId, double: bool) -> Fix {
        let ast = cx.ast();
        let src = cx.src();
        let span = cx.span(node);
        let start = expr_start(ast, cx.file.parents, node);
        let wrap = needs_parens_at(ast, start, leftmost_expr(ast, inner));
        if double {
            let text = cx.span(inner).text(src);
            let low = cx.parent(node).is_some_and(|p| {
                matches!(cx.node(p), Node::Binary { .. } | Node::Unary { .. })
                    && precedence(ast, inner) < precedence(ast, p)
            });
            let text = if wrap || low {
                format!("({text})")
            } else {
                text.to_string()
            };
            return Fix::replace(span, text);
        }
        match *cx.node(node) {
            Node::As { expr, .. } => {
                // From the whitespace before `as` to the end.
                let after = skip_trivia_forward(src, cx.span(expr).end as usize) as u32;
                let bytes = src.as_bytes();
                let mut from = after as usize;
                while from > 0 && matches!(bytes[from - 1], b' ' | b'\t' | b'\n' | b'\r') {
                    from -= 1;
                }
                Fix::delete(Span::new(from as u32, span.end))
            }
            Node::TypeAssertion { ty, expr } => {
                let close = skip_trivia_forward(src, cx.span(ty).end as usize);
                let close = if src.as_bytes().get(close) == Some(&b'>') {
                    close + 1
                } else {
                    cx.span(skip_parens(ast, inner)).start as usize
                };
                let head = Span::new(span.start, close as u32);
                // `<T>({...})` keeps its own parentheses.
                if wrap && !matches!(cx.node(expr), Node::Paren { .. }) {
                    Fix::replace(head, "(").and(Span::new(span.end, span.end), ")")
                } else {
                    Fix::delete(head)
                }
            }
            _ => Fix::delete(Span::new(span.end, span.end)),
        }
    }
}

/// `inner` is a call to a generic function without arguments or type arguments, whose type
/// arguments the assertion around it decides (through `await`, `!`, `?.` and `,`).
fn inferred_from_assertion(cx: &Ctx<'_, '_>, inner: NodeId) -> bool {
    let ast = cx.ast();
    let mut e = skip_parens(ast, inner);
    for _ in 0..64 {
        match *cx.node(e) {
            Node::Await { arg } | Node::NonNull { expr: arg } => e = skip_parens(ast, arg),
            Node::Seq { exprs } => match ast.list(exprs).last() {
                Some(&l) => e = skip_parens(ast, l),
                None => return false,
            },
            _ => break,
        }
    }
    let Node::Call {
        callee,
        args,
        type_args: None,
        ..
    } = *cx.node(e)
    else {
        return false;
    };
    if !ast.list(args).is_empty() {
        return false;
    }
    let ti = cx.types();
    let Some(ct) = ti.type_at(callee) else {
        return false;
    };
    let sigs: Vec<Sig> = parts(ti, ti.non_nullable(ct))
        .into_iter()
        .flat_map(|p| ti.signatures(p, false))
        .collect();
    !sigs.is_empty()
        && sigs
            .iter()
            .all(|&s| !ti.signature_type_params(s).is_empty() && ti.param_count(s) == (0, false))
}

/// The nullish members (`null`, `undefined`, `void`) of `t`.
fn nullish_flags(ti: &dyn TypeInfo, t: Ty) -> Vec<TypeFlags> {
    let mut out = Vec::new();
    for p in parts(ti, t) {
        if p == Ty::UNRESOLVED {
            continue;
        }
        let f = ti.flags(p);
        for k in [TypeFlags::NULL, TypeFlags::UNDEFINED, TypeFlags::VOID] {
            if f.contains(k) && !out.contains(&k) {
                out.push(k);
            }
        }
    }
    out
}

fn is_literal(ti: &dyn TypeInfo, t: Ty) -> bool {
    t != Ty::UNRESOLVED
        && ti.flags(t).intersects(
            TypeFlags::STRING_LITERAL
                | TypeFlags::NUMBER_LITERAL
                | TypeFlags::BIGINT_LITERAL
                | TypeFlags::BOOLEAN_LITERAL
                | TypeFlags::ENUM_LITERAL,
        )
        && !ti.flags(t).contains(TypeFlags::UNION)
}

/// `{}`: an object type with no members at all.
fn is_empty_object(ti: &dyn TypeInfo, t: Ty) -> bool {
    ti.flags(t).contains(TypeFlags::OBJECT)
        && ti.properties_of(t).is_empty()
        && ti.signatures(t, false).is_empty()
        && ti.signatures(t, true).is_empty()
        && ti.index_type(t, false).is_none()
        && ti.index_type(t, true).is_none()
}

/// Whether `a` and `b` are the same type: the same id, or unions/object types with the same
/// members.
fn same_type(ti: &dyn TypeInfo, a: Ty, b: Ty, depth: u32) -> bool {
    if a == b {
        return true;
    }
    if depth > 8 || a == Ty::UNRESOLVED || b == Ty::UNRESOLVED {
        return false;
    }
    let (fa, fb) = (ti.flags(a), ti.flags(b));
    // Fresh and regular literal types, and `undefined` from optional properties.
    if is_literal(ti, a) && is_literal(ti, b) {
        return fa == fb && ti.type_to_string(a) == ti.type_to_string(b);
    }
    let structured = TypeFlags::OBJECT
        | TypeFlags::UNION
        | TypeFlags::INTERSECTION
        | TypeFlags::INSTANTIABLE
        | TypeFlags::ENUM
        | TypeFlags::STRING_LITERAL
        | TypeFlags::NUMBER_LITERAL
        | TypeFlags::BIGINT_LITERAL
        | TypeFlags::BOOLEAN_LITERAL
        | TypeFlags::ENUM_LITERAL
        | TypeFlags::UNIQUE_ES_SYMBOL
        | TypeFlags::TEMPLATE_LITERAL
        | TypeFlags::STRING_MAPPING;
    if fa == fb && !fa.intersects(structured) && !fa.is_empty() {
        return true;
    }
    if fa.contains(TypeFlags::UNION) && fb.contains(TypeFlags::UNION) {
        // Compared as sets: an optional property's `undefined` can sit next to a declared
        // `undefined` (`a?: string | undefined`).
        let (ma, mb) = (ti.union_members(a), ti.union_members(b));
        return ma
            .iter()
            .all(|&x| mb.iter().any(|&y| same_type(ti, x, y, depth + 1)))
            && mb
                .iter()
                .all(|&y| ma.iter().any(|&x| same_type(ti, x, y, depth + 1)));
    }
    if !(fa.contains(TypeFlags::OBJECT) && fb.contains(TypeFlags::OBJECT)) {
        return false;
    }
    if let (Some(ta), Some(tb)) = (ti.reference_target(a), ti.reference_target(b)) {
        let (xa, xb) = (ti.type_args(a), ti.type_args(b));
        return ta == tb
            && xa.len() == xb.len()
            && xa
                .iter()
                .zip(xb.iter())
                .all(|(&x, &y)| same_type(ti, x, y, depth + 1));
    }
    for construct in [false, true] {
        if !ti.signatures(a, construct).is_empty() || !ti.signatures(b, construct).is_empty() {
            return false;
        }
    }
    for number in [false, true] {
        match (ti.index_type(a, number), ti.index_type(b, number)) {
            (None, None) => {}
            (Some(x), Some(y)) if same_type(ti, x, y, depth + 1) => {}
            _ => return false,
        }
    }
    let literal_object = ti.object_flags(a).contains(ObjectFlags::OBJECT_LITERAL);
    let (pa, pb) = (ti.properties_of(a), ti.properties_of(b));
    if pa.len() != pb.len() {
        return false;
    }
    pa.iter().all(|&s| {
        let name = ti.symbol_name(s);
        let Some(o) = ti.property_of_type(b, &name) else {
            return false;
        };
        let (fs, fo) = (ti.symbol_facts(s), ti.symbol_facts(o));
        // An object literal's properties are widened (`{ a: 'x' }` is `{ a: string }`).
        let mut ts = ti.type_of_symbol(s);
        if literal_object && is_literal(ti, ts) {
            ts = ti.base_of_literal(ts);
        }
        fs.optional == fo.optional
            && fs.readonly == fo.readonly
            && same_type(ti, ts, ti.type_of_symbol(o), depth + 1)
    })
}

/// `node` initializes a `const` or a `readonly` class property, where literal types are not
/// widened.
fn const_context(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let mut cur = node;
    let mut parent = cx.parent(cur);
    while let Some(p) = parent {
        if !matches!(cx.node(p), Node::Paren { .. }) {
            break;
        }
        cur = p;
        parent = cx.parent(cur);
    }
    let Some(p) = parent else { return false };
    match *cx.node(p) {
        Node::VarDeclarator { init: Some(i), .. } if i == cur => cx.parent(p).is_some_and(|d| {
            matches!(
                cx.node(d),
                Node::VarDecl {
                    kind: VarKind::Const,
                    ..
                }
            )
        }),
        Node::PropertyDef {
            modifiers,
            value: Some(v),
            ..
        } if v == cur => modifiers.contains(Modifiers::READONLY),
        _ => false,
    }
}

/// A `let`/`var` read that may happen before the variable is assigned (a declaration
/// without initializer whose type is not narrowed by an assignment yet), or a `var`
/// declared in a block that does not enclose the read.
fn possibly_unassigned(cx: &Ctx<'_, '_>, expr: NodeId) -> bool {
    let ast = cx.ast();
    let e = skip_parens(ast, expr);
    if !matches!(cx.node(e), Node::Ident { .. }) {
        return false;
    }
    let ti = cx.types();
    let Some(sym) = ti.symbol_at(e) else {
        return false;
    };
    let decls = ti.symbol_decls(sym);
    let [d] = decls[..] else { return false };
    if !d.in_this_file {
        return false;
    }
    let declarator = match cx.node(d.node) {
        Node::VarDeclarator { .. } => d.node,
        _ => match cx.parent(d.node) {
            Some(p) if matches!(cx.node(p), Node::VarDeclarator { .. }) => p,
            _ => return false,
        },
    };
    let Node::VarDeclarator { init, definite, .. } = *cx.node(declarator) else {
        return false;
    };
    let Some(decl) = cx.parent(declarator) else {
        return false;
    };
    let Node::VarDecl {
        kind, modifiers, ..
    } = *cx.node(decl)
    else {
        return false;
    };
    if modifiers.contains(Modifiers::DECLARE) || !matches!(kind, VarKind::Let | VarKind::Var) {
        return false;
    }
    if init.is_none() && !definite {
        let declared = ti.declared_type_at(e);
        let flow = ti.type_at(e);
        return declared.is_some() && declared == flow;
    }
    if kind == VarKind::Var {
        // The block holding the declaration must enclose the read.
        let Some(block) = ancestors_inclusive(cx.file.parents, decl)
            .skip(1)
            .find(|&a| matches!(cx.node(a), Node::Block { .. } | Node::Program { .. }))
        else {
            return false;
        };
        return !ancestors_inclusive(cx.file.parents, expr).any(|a| a == block);
    }
    false
}

/// The type a value at `node` is passed to: a parameter, an annotated variable or property,
/// a property of an object literal with such a receiver, a declared return type or a JSX
/// attribute. Generic parameters count only when they are a bare type parameter with a
/// constraint (the inferred type may depend on the value otherwise).
fn receiver(cx: &Ctx<'_, '_>, node: NodeId, declarations: bool, depth: u32) -> Option<Ty> {
    if depth > 16 {
        return None;
    }
    let ti = cx.types();
    let ast = cx.ast();
    let mut cur = node;
    let mut p = cx.parent(cur)?;
    while matches!(cx.node(p), Node::Paren { .. }) {
        cur = p;
        p = cx.parent(cur)?;
    }
    match *cx.node(p) {
        Node::Call {
            callee,
            args,
            type_args,
            ..
        }
        | Node::New {
            callee,
            args,
            type_args,
            ..
        } => {
            let list = ast.list(args);
            let i = list.iter().position(|&a| a == cur)?;
            if list[..i]
                .iter()
                .any(|&a| matches!(cx.node(a), Node::Spread { .. }))
            {
                return None;
            }
            let construct = matches!(cx.node(p), Node::New { .. });
            argument_receiver(cx, p, callee, i, type_args.is_some(), construct)
        }
        Node::VarDeclarator {
            type_ann: Some(t),
            init: Some(i),
            ..
        } if i == cur && declarations => ti.type_of_type_node(t),
        Node::PropertyDef {
            type_ann: Some(t),
            value: Some(v),
            ..
        } if v == cur && declarations => ti.type_of_type_node(t),
        Node::Property { key, value, .. } if value == cur => {
            let obj = cx.parent(p)?;
            let Node::ObjectLit { props } = *cx.node(obj) else {
                return None;
            };
            let r = receiver(cx, obj, true, depth + 1)?;
            let name = static_key_name(ast, cx.src(), key)?;
            property_receiver(cx, props, r, &name)
        }
        Node::Return { arg: Some(a) } if a == cur => {
            let f = ancestors_inclusive(cx.file.parents, p)
                .skip(1)
                .find(|&a| is_function(cx.node(a).kind()))?;
            declared_return(cx, f)
        }
        Node::Arrow { body, .. } if body == cur => {
            declared_return(cx, p).or_else(|| literal_in_callback(cx, node, p))
        }
        Node::JsxExprContainer { .. } => {
            let attr = cx.parent(p)?;
            let Node::JsxAttr { name, .. } = *cx.node(attr) else {
                return None;
            };
            jsx_attr_type(cx, attr, name)
        }
        _ => None,
    }
}

/// An asserted literal returned by an arrow passed to a call (`[].map(() => <T>{...})`) is
/// received by the arrow's contextual return type, as instantiated by the call; other
/// expressions in that generic context are not (`mapDefined(() => b as A)`, observed).
fn literal_in_callback(cx: &Ctx<'_, '_>, node: NodeId, arrow: NodeId) -> Option<Ty> {
    let ast = cx.ast();
    let (Node::As { expr, .. } | Node::TypeAssertion { expr, .. }) = *cx.node(node) else {
        return None;
    };
    let literal = matches!(
        cx.node(skip_parens(ast, expr)),
        Node::ObjectLit { .. }
            | Node::NullLit {}
            | Node::BoolLit { .. }
            | Node::NumberLit { .. }
            | Node::BigIntLit { .. }
            | Node::StringLit { .. }
            | Node::RegexLit { .. }
            | Node::ClassExpr { .. }
            | Node::FunctionExpr { .. }
            | Node::Arrow { .. }
            | Node::JsxElement { .. }
    );
    let mut cur = arrow;
    let mut p = cx.parent(cur)?;
    while matches!(cx.node(p), Node::Paren { .. }) {
        cur = p;
        p = cx.parent(cur)?;
    }
    let in_call = match *cx.node(p) {
        Node::Call { args, .. } | Node::New { args, .. } => ast.list(args).contains(&cur),
        _ => false,
    };
    if !(literal && in_call) {
        return None;
    }
    cx.types().contextual_type(node)
}

fn declared_return(cx: &Ctx<'_, '_>, f: NodeId) -> Option<Ty> {
    let ti = cx.types();
    let (flags, rt) = match *cx.node(f) {
        Node::FunctionDecl {
            flags, return_type, ..
        }
        | Node::FunctionExpr {
            flags, return_type, ..
        }
        | Node::Arrow {
            flags, return_type, ..
        } => (flags, return_type?),
        _ => return None,
    };
    if flags.contains(FnFlags::GENERATOR) {
        return None;
    }
    let t = ti.type_of_type_node(rt)?;
    Some(if flags.contains(FnFlags::ASYNC) {
        ti.awaited_type(t)
    } else {
        t
    })
}

fn argument_receiver(
    cx: &Ctx<'_, '_>,
    call: NodeId,
    callee: NodeId,
    i: usize,
    explicit_type_args: bool,
    construct: bool,
) -> Option<Ty> {
    let ti = cx.types();
    let Some(sig) = ti.resolved_signature(call) else {
        // An argument of an error-typed callee (an unresolved import) is received as the
        // error type, unless the call is inside an argument of a generic call (observed).
        let error =
            ti.type_at(callee).is_some_and(|t| ti.is_error(t)) && !in_generic_argument(cx, call);
        return error.then_some(Ty::ERROR);
    };
    let inst = ti.param_type(sig, i)?;
    if explicit_type_args {
        return Some(inst);
    }
    let decl = ti.signature_decl(sig);
    let ct = ti.type_at(callee)?;
    let declared = parts(ti, ti.non_nullable(ct)).into_iter().find_map(|p| {
        ti.signatures(p, construct).into_iter().find(|&s| {
            !ti.signature_type_params(s).is_empty()
                && decl.is_some()
                && ti.signature_decl(s) == decl
        })
    });
    let Some(ds) = declared else {
        return Some(inst);
    };
    let d = ti.param_type(ds, i)?;
    if ti.flags(d).contains(TypeFlags::TYPE_PARAMETER) {
        let c = ti.base_constraint(d);
        if c == Ty::UNRESOLVED || is_unknown(ti, c) || is_any(ti, c) {
            return None;
        }
        return Some(c);
    }
    (d == inst).then_some(inst)
}

/// The receiver of property `name` of an object literal (with properties `props`) passed to
/// `r`. A union receiver is narrowed by the literal's discriminant properties; it must come
/// down to one member.
fn property_receiver(cx: &Ctx<'_, '_>, props: ts_r_ast::NodeList, r: Ty, name: &str) -> Option<Ty> {
    let ti = cx.types();
    let ast = cx.ast();
    let src = cx.src();
    let r = constrained(ti, r);
    let mut members = parts(ti, ti.non_nullable(r));
    if members.len() > 1 {
        let discriminants: Vec<(String, Ty)> = ast
            .list(props)
            .iter()
            .filter_map(|&p| {
                let Node::Property { key, value, .. } = *cx.node(p) else {
                    return None;
                };
                let t = ti.type_at(value)?;
                if !is_literal(ti, t) {
                    return None;
                }
                Some((static_key_name(ast, src, key)?.into_owned(), t))
            })
            .collect();
        members.retain(|&m| {
            discriminants.iter().all(|(k, vt)| {
                let Some(s) = ti.property_of_type(m, k) else {
                    return true;
                };
                let pt = ti.type_of_symbol(s);
                let literal_prop = parts(ti, pt).into_iter().all(|x| is_literal(ti, x));
                !literal_prop || ti.is_assignable(*vt, pt)
            })
        });
    }
    let [m] = members[..] else { return None };
    match ti.property_of_type(m, name) {
        Some(s) => Some(ti.type_of_symbol(s)),
        None => ti.index_type(m, false),
    }
}

/// `node` is inside an argument of a call (or `new`) whose callee has a generic signature.
fn in_generic_argument(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ti = cx.types();
    let ast = cx.ast();
    let mut cur = node;
    for _ in 0..10_000 {
        let Some(p) = cx.parent(cur) else {
            return false;
        };
        if let Node::Call { callee, args, .. } | Node::New { callee, args, .. } = *cx.node(p)
            && ast.list(args).contains(&cur)
            && let Some(ct) = ti.type_at(callee)
        {
            let construct = matches!(cx.node(p), Node::New { .. });
            let generic = parts(ti, ti.non_nullable(ct)).into_iter().any(|t| {
                ti.signatures(t, construct)
                    .into_iter()
                    .any(|s| !ti.signature_type_params(s).is_empty())
            });
            if generic {
                return true;
            }
        }
        cur = p;
    }
    false
}
