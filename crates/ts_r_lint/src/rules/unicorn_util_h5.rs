//! Helpers for the round-5 eslint-plugin-unicorn rules: comment checks for fixes that
//! replace text, expression text that survives being moved into a call, and the tokens of a
//! call's argument list.

use super::prelude::*;
use super::unicorn_util::parenthesized_span;

/// True when replacing `range` would drop a comment: one lies wholly inside `range` but
/// outside every `kept` range.
pub(crate) fn would_remove_comments(cx: &Ctx<'_, '_>, range: Span, kept: &[Span]) -> bool {
    cx.ast().comments_in(range).iter().any(|c| {
        c.span.end <= range.end
            && !kept
                .iter()
                .any(|k| c.span.start >= k.start && c.span.end <= k.end)
    })
}

/// True when `id` is wrapped in at least one pair of parentheses.
pub(crate) fn is_parenthesized(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    cx.parent(id)
        .is_some_and(|p| matches!(cx.node(p), Node::Paren { .. }))
}

/// The text of `id` to be moved somewhere else: without redundant parentheses unless they
/// hold comments (then the parenthesized text as written), wrapped in parentheses when
/// `needs_parens`.
pub(crate) fn expression_text(cx: &Ctx<'_, '_>, id: NodeId, needs_parens: bool) -> String {
    if is_parenthesized(cx, id) {
        let outer = parenthesized_span(cx, id);
        if would_remove_comments(cx, outer, &[cx.span(id)]) {
            return outer.text(cx.src()).to_owned();
        }
    }
    let text = cx.text(id);
    if needs_parens {
        format!("({text})")
    } else {
        text.to_owned()
    }
}

/// [`expression_text`] for a single call argument: a sequence gets parentheses.
pub(crate) fn call_argument_text(cx: &Ctx<'_, '_>, id: NodeId) -> String {
    let seq = matches!(cx.node(id), Node::Seq { .. });
    expression_text(cx, id, seq)
}

/// The `(` and `)` of a call or `new` expression's argument list, as spans of one byte.
pub(crate) fn call_parens(cx: &Ctx<'_, '_>, call: NodeId) -> Option<(Span, Span)> {
    let src = cx.src();
    let (callee, type_args) = match *cx.node(call) {
        Node::Call {
            callee, type_args, ..
        } => (callee, type_args),
        Node::New {
            callee,
            type_args,
            has_args: true,
            ..
        } => (callee, type_args),
        _ => return None,
    };
    let mut from = parenthesized_span(cx, callee).end;
    if let Some(t) = type_args {
        from = from.max(cx.span(t).end);
    }
    let mut open = skip_trivia_forward(src, from as usize);
    if src.as_bytes().get(open..open + 2) == Some(b"?.") {
        open = skip_trivia_forward(src, open + 2);
    }
    if src.as_bytes().get(open) != Some(&b'(') {
        return None;
    }
    let end = cx.span(call).end as usize;
    if end == 0 || src.as_bytes().get(end - 1) != Some(&b')') {
        return None;
    }
    Some((
        Span::new(open as u32, open as u32 + 1),
        Span::new(end as u32 - 1, end as u32),
    ))
}

/// The source between a call's parentheses (a trailing comma included).
pub(crate) fn call_arguments_text<'a>(cx: &Ctx<'_, 'a>, call: NodeId) -> Option<&'a str> {
    let (open, close) = call_parens(cx, call)?;
    cx.src().get(open.end as usize..close.start as usize)
}

/// The span of the operator `op` that follows `left` (with its parentheses).
pub(crate) fn operator_after(cx: &Ctx<'_, '_>, left: NodeId, op: &str) -> Option<Span> {
    let src = cx.src();
    let at = skip_trivia_forward(src, parenthesized_span(cx, left).end as usize);
    src[at..]
        .starts_with(op)
        .then(|| Span::new(at as u32, (at + op.len()) as u32))
}

/// A literal number with the given value (through parentheses).
pub(crate) fn is_number_literal(cx: &Ctx<'_, '_>, id: NodeId, value: f64) -> bool {
    matches!(*cx.node(skip_parens(cx.ast(), id)), Node::NumberLit { value: v } if v.get() == value)
}

/// `@eslint-community/eslint-utils`' `hasSideEffect` with default options: calls, `new`,
/// assignments, updates, `delete`, `await`, `yield`, `import()` and tagged templates have
/// side effects; reading members does not.
pub(crate) fn has_side_effect(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    fn walk(ast: &ts_r_ast::Ast, id: NodeId, depth: u32) -> bool {
        if depth > 200 {
            return true;
        }
        match ast.node(id) {
            Node::Call { .. }
            | Node::New { .. }
            | Node::Assign { .. }
            | Node::Update { .. }
            | Node::Await { .. }
            | Node::Yield { .. }
            | Node::ImportCall { .. }
            | Node::TaggedTemplate { .. }
            | Node::Unary {
                op: UnaryOp::Delete,
                ..
            } => true,
            Node::FunctionExpr { .. } | Node::Arrow { .. } | Node::ClassExpr { .. } => false,
            n => {
                let mut found = false;
                n.for_each_child(ast, &mut |c| {
                    if !found {
                        found = walk(ast, c, depth + 1);
                    }
                });
                found
            }
        }
    }
    walk(cx.ast(), id, 0)
}

/// An optional link (`?.`) in the chain `id` ends, not cut off by parentheses (TS
/// wrappers without parentheses are looked through): `a?.b` yes, `(a?.b)` no.
pub(crate) fn has_unparenthesized_optional_chain(ast: &ts_r_ast::Ast, id: NodeId) -> bool {
    let mut id = id;
    for _ in 0..100_000 {
        match *ast.node(id) {
            Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::NonNull { expr }
            | Node::TypeAssertion { expr, .. } => id = expr,
            Node::Member { obj, optional, .. } | Node::Index { obj, optional, .. } => {
                if optional {
                    return true;
                }
                id = obj;
            }
            Node::Call {
                callee, optional, ..
            } => {
                if optional {
                    return true;
                }
                id = callee;
            }
            _ => return false,
        }
    }
    false
}

/// unicorn's `getStaticStringValue`: a string literal or a template without expressions.
pub(crate) fn static_string(ast: &ts_r_ast::Ast, id: NodeId) -> Option<&str> {
    match *ast.node(skip_parens(ast, id)) {
        Node::StringLit { value } => Some(ast.atom(value)),
        Node::TemplateLit { parts } => match ast.list(parts) {
            [only] => match *ast.node(*only) {
                Node::TemplateElement { cooked, .. } => Some(ast.atom(cooked)),
                _ => None,
            },
            _ => None,
        },
        _ => None,
    }
}

/// The name of member `id` (through parentheses): `a.b`, or `a["b"]` / ``a[`b`]`` with a
/// static string key.
pub(crate) fn static_member_name(ast: &ts_r_ast::Ast, id: NodeId) -> Option<&str> {
    match *ast.node(skip_parens(ast, id)) {
        Node::Member { prop, .. } => ident_name(ast, prop),
        Node::Index { index, .. } => static_string(ast, index),
        _ => None,
    }
}

/// The object of member `id` (through parentheses).
pub(crate) fn member_object(ast: &ts_r_ast::Ast, id: NodeId) -> Option<NodeId> {
    match *ast.node(skip_parens(ast, id)) {
        Node::Member { obj, .. } | Node::Index { obj, .. } => Some(obj),
        _ => None,
    }
}

/// `@eslint-community/eslint-utils`' `getFunctionHeadLocation` as a span: an arrow's `=>`;
/// otherwise from the start of the function (or of the property, method or field that
/// holds it, as its value or computed key) to the `(` of its parameters.
pub(crate) fn utils_function_head_span(cx: &Ctx<'_, '_>, f: NodeId) -> Span {
    let ast = cx.ast();
    let src = cx.src();
    let head = function_head_span(ast, src, cx.file.parents, f);
    if !matches!(cx.node(f), Node::Arrow { .. })
        && let Some(p) = cx.parent(f)
        && matches!(cx.node(p), Node::ComputedKey { .. })
        && let Some(holder) = cx.parent(p)
        && matches!(
            cx.node(holder),
            Node::Property { .. } | Node::MethodDef { .. } | Node::PropertyDef { .. }
        )
    {
        return Span::new(cx.span(holder).start, head.end);
    }
    head
}

/// `@eslint-community/eslint-utils`' `getFunctionNameWithKind` (which differs from
/// ESLint's: class fields holding functions are methods, `export default` functions are
/// named `'default'`, an object property's function is a method).
pub(crate) fn utils_function_name_with_kind(cx: &Ctx<'_, '_>, f: NodeId) -> String {
    let ast = cx.ast();
    let (is_async, is_gen, own_name, is_arrow) = match *cx.node(f) {
        Node::FunctionDecl { flags, name, .. } | Node::FunctionExpr { flags, name, .. } => (
            flags.contains(FnFlags::ASYNC),
            flags.contains(FnFlags::GENERATOR),
            name.and_then(|n| ident_name(ast, n)),
            false,
        ),
        Node::Arrow { flags, .. } => (flags.contains(FnFlags::ASYNC), false, None, true),
        _ => return String::new(),
    };
    let direct = parent_skip_parens(ast, cx.file.parents, f);
    // (holder, key, is class member, method kind)
    let holder = direct.and_then(|p| match *cx.node(p) {
        Node::Property { key, value, .. } if value == f => Some((p, key, false, None)),
        Node::MethodDef { key, kind, .. } => {
            let in_object = cx
                .parent(p)
                .is_some_and(|o| matches!(cx.node(o), Node::ObjectLit { .. }));
            Some((p, key, !in_object, Some(kind)))
        }
        Node::PropertyDef {
            key,
            value: Some(v),
            ..
        } if skip_parens(ast, v) == f => Some((p, key, true, None)),
        _ => None,
    });
    let mut tokens: Vec<String> = Vec::new();
    if let Some((p, key, true, _)) = holder {
        if cx.node(p).modifiers().contains(Modifiers::STATIC) {
            tokens.push("static".into());
        }
        if matches!(cx.node(key), Node::PrivateName { .. }) {
            tokens.push("private".into());
        }
    }
    if is_async {
        tokens.push("async".into());
    }
    if is_gen {
        tokens.push("generator".into());
    }
    match holder {
        Some((_, _, _, Some(MethodKind::Constructor))) => return "constructor".into(),
        Some((_, _, _, Some(MethodKind::Get))) => tokens.push("getter".into()),
        Some((_, _, _, Some(MethodKind::Set))) => tokens.push("setter".into()),
        Some(_) => tokens.push("method".into()),
        None => {
            if is_arrow {
                tokens.push("arrow".into());
            }
            tokens.push("function".into());
        }
    }
    if let Some((_, key, _, _)) = holder {
        if let Node::PrivateName { name } = *cx.node(key) {
            tokens.push(format!("#{}", ast.atom(name)));
        } else if let Some(name) = utils_property_name(cx, key) {
            tokens.push(format!("'{name}'"));
        } else {
            let text = match *cx.node(key) {
                Node::ComputedKey { expr } => cx.text(expr),
                _ => cx.text(key),
            };
            if !text.contains('\n') {
                tokens.push(format!("[{text}]"));
            }
        }
    } else if let Some(name) = own_name {
        tokens.push(format!("'{name}'"));
    } else if matches!(cx.node(f), Node::FunctionDecl { modifiers, .. } if modifiers.contains(Modifiers::DEFAULT))
    {
        tokens.push("'default'".into());
    } else if let Some(p) = direct {
        match *cx.node(p) {
            Node::VarDeclarator { name, .. } if ident_name(ast, name).is_some() => {
                tokens.push(format!("'{}'", ident_name(ast, name).unwrap_or_default()));
            }
            Node::Assign {
                left,
                op: AssignOp::Assign,
                ..
            }
            | Node::AssignPattern { left, .. }
                if ident_name(ast, left).is_some() =>
            {
                tokens.push(format!("'{}'", ident_name(ast, left).unwrap_or_default()));
            }
            Node::Param {
                pattern,
                init: Some(_),
                ..
            } if ident_name(ast, pattern).is_some() => {
                tokens.push(format!(
                    "'{}'",
                    ident_name(ast, pattern).unwrap_or_default()
                ));
            }
            Node::BindingProperty {
                key,
                value: None,
                init: Some(_),
            } => {
                if let Some(n) = ident_name(ast, key) {
                    tokens.push(format!("'{n}'"));
                }
            }
            Node::ShorthandProp {
                name,
                init: Some(_),
                ..
            } => {
                if let Some(n) = ident_name(ast, name) {
                    tokens.push(format!("'{n}'"));
                }
            }
            Node::ExportDefaultExpr { .. } => tokens.push("'default'".into()),
            _ => {}
        }
    }
    tokens.join(" ")
}

/// eslint-utils' `getPropertyName` of a key without a scope: an identifier, a string or
/// number literal, or a computed key with a static value (literals, templates, `+`).
pub(crate) fn utils_property_name(cx: &Ctx<'_, '_>, key: NodeId) -> Option<String> {
    let ast = cx.ast();
    match *cx.node(key) {
        Node::Ident { name } => Some(ast.atom(name).to_owned()),
        Node::StringLit { value } => Some(ast.atom(value).to_owned()),
        Node::NumberLit { value } => Some(number_to_string(value.get())),
        Node::ComputedKey { expr } => static_value_string(ast, cx.src(), expr)
            .map(|c| c.into_owned())
            .or_else(|| literal_concat(cx, expr, 0)),
        _ => None,
    }
}

fn literal_concat(cx: &Ctx<'_, '_>, id: NodeId, depth: u32) -> Option<String> {
    if depth > 16 {
        return None;
    }
    let ast = cx.ast();
    match *ast.node(skip_parens(ast, id)) {
        Node::Binary {
            left,
            op: BinaryOp::Add,
            right,
        } => {
            let l = static_value_string(ast, cx.src(), left)
                .map(|c| c.into_owned())
                .or_else(|| literal_concat(cx, left, depth + 1))?;
            let r = static_value_string(ast, cx.src(), right)
                .map(|c| c.into_owned())
                .or_else(|| literal_concat(cx, right, depth + 1))?;
            Some(l + &r)
        }
        _ => None,
    }
}

/// unicorn's `isNodeValueNotDomNode`: a literal, array, object, function, class, template
/// or `undefined` (through parentheses).
pub(crate) fn is_value_not_dom_node(ast: &ts_r_ast::Ast, id: NodeId) -> bool {
    let id = skip_parens(ast, id);
    matches!(
        ast.node(id),
        Node::ArrayLit { .. }
            | Node::Arrow { .. }
            | Node::ClassExpr { .. }
            | Node::FunctionExpr { .. }
            | Node::StringLit { .. }
            | Node::NumberLit { .. }
            | Node::BigIntLit { .. }
            | Node::BoolLit { .. }
            | Node::NullLit {}
            | Node::RegexLit { .. }
            | Node::ObjectLit { .. }
            | Node::TemplateLit { .. }
    ) || ident_name(ast, id) == Some("undefined")
}

/// unicorn's `isValueNotUsable`: the expression is a statement of its own, or a sequence
/// element whose value is dropped.
pub(crate) fn is_value_not_usable(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let mut id = id;
    for _ in 0..10_000 {
        let Some(p) = parent_skip_parens(ast, cx.file.parents, id) else {
            return false;
        };
        match *cx.node(p) {
            Node::ExprStmt { .. } => return true,
            Node::Seq { exprs } => {
                if ast.list(exprs).last().map(|&e| skip_parens(ast, e)) != Some(id) {
                    return true;
                }
                id = p;
            }
            _ => return false,
        }
    }
    false
}

/// Parentheses and TS wrappers stripped (unicorn's `unwrapChainAndTypeScriptExpression`;
/// optional chains have no node of their own here).
pub(crate) fn unwrap_ts(ast: &ts_r_ast::Ast, id: NodeId) -> NodeId {
    super::unicorn_util::skip_wrappers(ast, id)
}

/// The static name of a member access for reference comparison: `a.b` gives `b`, a
/// computed key gives the string form of its static value (`a[1]` and `a["1"]` agree).
fn reference_property_name(cx: &Ctx<'_, '_>, id: NodeId) -> Option<String> {
    let ast = cx.ast();
    match *ast.node(id) {
        Node::Member { prop, .. } => match *ast.node(prop) {
            Node::Ident { name } => Some(ast.atom(name).to_owned()),
            _ => None,
        },
        Node::Index { index, .. } => static_value_string(ast, cx.src(), index)
            .map(|c| c.into_owned())
            .or_else(|| literal_concat(cx, index, 0)),
        _ => None,
    }
}

/// unicorn's `isSameReference`: the same identifier, `this`, `super`, literal value, static
/// string, or member chain (static keys compared by value), through parentheses and TS
/// wrappers.
pub(crate) fn is_same_reference(cx: &Ctx<'_, '_>, a: NodeId, b: NodeId) -> bool {
    fn same(cx: &Ctx<'_, '_>, a: NodeId, b: NodeId, depth: u32) -> bool {
        if depth > 64 {
            return false;
        }
        let ast = cx.ast();
        let a = unwrap_ts(ast, a);
        let b = unwrap_ts(ast, b);
        if let Some(sa) = static_string(ast, a) {
            return static_string(ast, b) == Some(sa);
        }
        match (*ast.node(a), *ast.node(b)) {
            (Node::This {}, Node::This {}) | (Node::Super {}, Node::Super {}) => true,
            (Node::Ident { name: x }, Node::Ident { name: y })
            | (Node::PrivateName { name: x }, Node::PrivateName { name: y }) => {
                ast.atom(x) == ast.atom(y)
            }
            (Node::NumberLit { value: x }, Node::NumberLit { value: y }) => x.get() == y.get(),
            (Node::BigIntLit { value: x }, Node::BigIntLit { value: y }) => {
                parse_bigint(ast.atom(x)) == parse_bigint(ast.atom(y))
            }
            (Node::BoolLit { value: x }, Node::BoolLit { value: y }) => x == y,
            (Node::NullLit {}, Node::NullLit {}) => true,
            (
                Node::RegexLit {
                    pattern: p1,
                    flags: f1,
                },
                Node::RegexLit {
                    pattern: p2,
                    flags: f2,
                },
            ) => ast.atom(p1) == ast.atom(p2) && ast.atom(f1) == ast.atom(f2),
            (
                Node::Member { obj: o1, .. } | Node::Index { obj: o1, .. },
                Node::Member { obj: o2, .. } | Node::Index { obj: o2, .. },
            ) => {
                if let Some(name) = reference_property_name(cx, a) {
                    return same(cx, o1, o2, depth + 1)
                        && reference_property_name(cx, b).as_deref() == Some(name.as_str());
                }
                match (*ast.node(a), *ast.node(b)) {
                    (Node::Index { index: i1, .. }, Node::Index { index: i2, .. }) => {
                        same(cx, o1, o2, depth + 1) && same(cx, i1, i2, depth + 1)
                    }
                    (Node::Member { prop: p1, .. }, Node::Member { prop: p2, .. }) => {
                        same(cx, o1, o2, depth + 1) && same(cx, p1, p2, depth + 1)
                    }
                    _ => false,
                }
            }
            _ => false,
        }
    }
    same(cx, a, b, 0)
}
