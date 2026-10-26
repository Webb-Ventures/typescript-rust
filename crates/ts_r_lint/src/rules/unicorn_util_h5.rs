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

/// `@eslint-community/eslint-utils`' `hasSideEffect` with default options (calls, `new`,
/// writes, `await`, `yield`, `import()`, `delete`; member reads and nested functions
/// don't count), from the shared engine in `unicorn_static`.
pub(crate) fn has_side_effect(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    super::unicorn_static::has_side_effect(cx.ast(), id)
}

/// unicorn's `hasPossibleSideEffect`: [`has_side_effect`], or a tagged template or spread
/// anywhere inside (even in nested functions).
pub(crate) fn has_possible_side_effect(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    fn contains(ast: &ts_r_ast::Ast, id: NodeId, depth: u32) -> bool {
        if depth > 500 {
            return true;
        }
        if matches!(
            ast.node(id),
            Node::TaggedTemplate { .. } | Node::Spread { .. }
        ) {
            return true;
        }
        let mut found = false;
        ast.for_each_child(id, |c| found = found || contains(ast, c, depth + 1));
        found
    }
    has_side_effect(cx, id) || contains(cx.ast(), id, 0)
}

/// unicorn's `isSideEffectFreeFunction`: an inline arrow or function expression whose
/// parameters and body have no possible side effect.
pub(crate) fn is_side_effect_free_function(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let id = skip_parens(ast, id);
    let (params, body) = match *ast.node(id) {
        Node::Arrow { params, body, .. } => (params, Some(body)),
        Node::FunctionExpr { params, body, .. } => (params, body),
        _ => return false,
    };
    ast.list(params)
        .iter()
        .chain(body.as_ref())
        .all(|&c| !has_possible_side_effect(cx, c))
}

/// A call `obj.name(...)` in unicorn's `isMethodCall` sense: not an optional call, a
/// non-computed member (parentheses around the callee are looked through), `name` one of
/// `names`, an argument count in `min..=max`, and no spread among the first `max`
/// arguments when `max` is given. `optional_member: false` also rejects `obj?.name`.
pub(crate) fn method_call_of<'a>(
    ast: &'a ts_r_ast::Ast,
    id: NodeId,
    names: &[&str],
    min: usize,
    max: Option<usize>,
    optional_member: Option<bool>,
) -> Option<(NodeId, NodeId, &'a [NodeId])> {
    let Node::Call {
        callee,
        optional: false,
        args,
        ..
    } = *ast.node(id)
    else {
        return None;
    };
    let Node::Member {
        obj,
        prop,
        optional,
    } = *ast.node(skip_parens(ast, callee))
    else {
        return None;
    };
    if optional_member == Some(false) && optional {
        return None;
    }
    let name = ident_name(ast, prop)?;
    if !names.contains(&name) {
        return None;
    }
    let args = ast.list(args);
    if args.len() < min || max.is_some_and(|m| args.len() > m) {
        return None;
    }
    if let Some(m) = max
        && args
            .iter()
            .take(m)
            .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
    {
        return None;
    }
    Some((obj, prop, args))
}

/// The parent of `id` through parentheses, and the outermost parenthesized form of `id`
/// (the node the parent holds).
pub(crate) fn parent_through_parens(cx: &Ctx<'_, '_>, id: NodeId) -> Option<(NodeId, NodeId)> {
    let mut cur = id;
    for _ in 0..10_000 {
        let p = cx.parent(cur)?;
        if matches!(cx.node(p), Node::Paren { .. }) {
            cur = p;
            continue;
        }
        return Some((p, cur));
    }
    None
}

/// unicorn's `isGlobalBooleanCall`: `Boolean(x)` with one non-spread argument, not an
/// optional call, `Boolean` not declared in scope.
pub(crate) fn is_global_boolean_call(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let Node::Call {
        callee,
        optional: false,
        args,
        ..
    } = *ast.node(id)
    else {
        return false;
    };
    let &[arg] = ast.list(args) else {
        return false;
    };
    if ident_name(ast, callee) != Some("Boolean") || matches!(ast.node(arg), Node::Spread { .. }) {
        return false;
    }
    let sem = cx.semantic();
    sem.symbol_of_ident(callee)
        .is_none_or(|s| sem.symbol(s).decls.is_empty())
}

fn is_logical(node: &Node) -> bool {
    matches!(
        node,
        Node::Binary {
            op: BinaryOp::LogicalAnd | BinaryOp::LogicalOr,
            ..
        }
    )
}

/// unicorn's `isBooleanContext`: `id`'s value is only used for its truthiness: the operand
/// of `!`, the argument of `Boolean(...)`, or a control-flow test (`if`, `?:`, `while`,
/// `do-while`, `for`), through `&&` / `||` operands (TS wrappers are not looked through).
pub(crate) fn is_boolean_context(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let mut cur = id;
    for _ in 0..10_000 {
        if is_global_boolean_call(cx, cur) {
            return true;
        }
        let Some((parent, held)) = parent_through_parens(cx, cur) else {
            return false;
        };
        match *cx.node(parent) {
            Node::Unary {
                op: UnaryOp::Not, ..
            } => return true,
            Node::Call { .. } if is_global_boolean_call(cx, parent) => {
                return cx.ast().list(match *cx.node(parent) {
                    Node::Call { args, .. } => args,
                    _ => return false,
                })[0]
                    == held;
            }
            Node::If { test, .. }
            | Node::Conditional { test, .. }
            | Node::While { test, .. }
            | Node::DoWhile { test, .. } => return test == held,
            Node::For { test, .. } => return test == Some(held),
            ref n if is_logical(n) => cur = parent,
            _ => return false,
        }
    }
    false
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

/// The text of `id` with all its parentheses (unicorn's `getParenthesizedText`).
pub(crate) fn parenthesized_text<'a>(cx: &Ctx<'_, 'a>, id: NodeId) -> &'a str {
    let s = parenthesized_span(cx, id);
    &cx.src()[s.start as usize..s.end as usize]
}

/// True when the source at `start` begins with a token that would make a statement a block
/// or a declaration: `{`, `class`, `function`, `async function`, `let [`.
pub(crate) fn starts_like_declaration(src: &str, start: u32) -> bool {
    let rest = src.get(start as usize..).unwrap_or("");
    let word = |r: &'_ str, w: &str| -> Option<usize> {
        r.strip_prefix(w)
            .filter(|t| !t.starts_with(is_ident_char))
            .map(|_| w.len())
    };
    let after = |len: usize| {
        let p = skip_trivia_forward(rest, len);
        &rest[p..]
    };
    rest.starts_with('{')
        || word(rest, "class").is_some()
        || word(rest, "function").is_some()
        || word(rest, "async").is_some_and(|l| word(after(l), "function").is_some())
        || word(rest, "let").is_some_and(|l| after(l).starts_with('['))
}

/// The node whose text starts at `id`'s first token, through the left-hand sides.
fn first_token_node(ast: &ts_r_ast::Ast, id: NodeId) -> NodeId {
    let mut id = id;
    for _ in 0..10_000 {
        let next = match *ast.node(id) {
            Node::Paren { .. } => return id,
            _ => leading_child(ast, id),
        };
        match next {
            Some(n) if ast.span(n).start == ast.span(id).start => id = n,
            _ => return id,
        }
    }
    id
}

/// unicorn's `shouldAddParenthesesToExpressionStart`: `expr`, written where `replacement`
/// is, would begin a statement (or arrow body, `export default`, `for` initializer) with a
/// token that changes how it parses (a declaration-like start, or a regex starting a
/// statement is fine).
pub(crate) fn expression_start_needs_parens(
    cx: &Ctx<'_, '_>,
    expr: NodeId,
    replacement: NodeId,
) -> bool {
    let ast = cx.ast();
    let src = cx.src();
    let start = cx.span(expr).start;
    let regex = matches!(ast.node(first_token_node(ast, expr)), Node::RegexLit { .. });
    if !regex && !starts_like_declaration(src, start) {
        return false;
    }
    let first_is = |w: &str| {
        src[start as usize..].starts_with(w)
            && !src[start as usize + w.len()..].starts_with(is_ident_char)
    };
    let mut node = replacement;
    for _ in 0..10_000 {
        let Some(parent) = cx.parent(node) else {
            return false;
        };
        match *cx.node(parent) {
            Node::Paren { .. } => return false,
            Node::Arrow { body, .. } if body == node => {
                return src[start as usize..].starts_with('{');
            }
            Node::ExportDefaultExpr { .. } => {
                return !regex && !src[start as usize..].starts_with('{') && !first_is("let");
            }
            Node::For {
                init: Some(init), ..
            } if init == node => return first_is("let"),
            Node::ExprStmt { .. } => return !regex,
            _ => {}
        }
        if cx.span(parent).start != cx.span(node).start {
            return false;
        }
        node = parent;
    }
    false
}

/// `new Foo` without its own argument parentheses.
fn is_new_without_parens(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let Node::New {
        callee,
        has_args,
        args,
        ..
    } = *cx.node(id)
    else {
        return false;
    };
    if has_args && !cx.ast().list(args).is_empty() {
        return false;
    }
    !(has_args && cx.text(id).trim_end().ends_with(')') && cx.span(callee).end < cx.span(id).end)
}

/// unicorn's `shouldAddParenthesesToMemberExpressionObject`: `id` (not parenthesized) needs
/// parentheses as the object of a member access written where `replacement` is.
pub(crate) fn member_object_needs_parens(
    cx: &Ctx<'_, '_>,
    id: NodeId,
    replacement: NodeId,
) -> bool {
    if expression_start_needs_parens(cx, id, replacement) {
        return true;
    }
    match *cx.node(id) {
        Node::Ident { .. }
        | Node::Member { .. }
        | Node::Index { .. }
        | Node::Call { .. }
        | Node::TemplateLit { .. }
        | Node::This {}
        | Node::Super {}
        | Node::ArrayLit { .. }
        | Node::MetaProperty { .. }
        | Node::ImportCall { .. }
        | Node::TaggedTemplate { .. }
        | Node::NonNull { .. } => false,
        Node::New { .. } => is_new_without_parens(cx, id),
        Node::NumberLit { .. } => cx.text(id).bytes().all(|b| b.is_ascii_digit()),
        Node::StringLit { .. }
        | Node::BoolLit { .. }
        | Node::NullLit {}
        | Node::RegexLit { .. }
        | Node::BigIntLit { .. } => false,
        _ => true,
    }
}

/// unicorn's `getMemberExpressionObjectText`: `id` with its parentheses, wrapped in new ones
/// when it needs them as a member object written where `replacement` is.
pub(crate) fn member_object_text(cx: &Ctx<'_, '_>, id: NodeId, replacement: NodeId) -> String {
    let text = parenthesized_text(cx, id);
    if !is_parenthesized(cx, id) && member_object_needs_parens(cx, id, replacement) {
        format!("({text})")
    } else {
        text.to_owned()
    }
}

/// unicorn's `isInUnparenthesizedNewExpressionCallee`: `id` is (through member objects,
/// tags and TS wrappers, none parenthesized) the callee of a `new`.
pub(crate) fn in_unparenthesized_new_callee(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let mut node = id;
    for _ in 0..10_000 {
        let Some(parent) = cx.parent(node) else {
            return false;
        };
        match *cx.node(parent) {
            Node::New { callee, .. } => return callee == node,
            Node::Member { obj, .. } | Node::Index { obj, .. } if obj == node => {}
            Node::TaggedTemplate { tag, .. } if tag == node => {}
            Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::NonNull { expr }
            | Node::TypeAssertion { expr, .. }
            | Node::Instantiation { expr, .. }
                if expr == node => {}
            _ => return false,
        }
        node = parent;
    }
    false
}

/// unicorn's `shouldAddParenthesesToNewExpressionCallee` for a member access on `object`:
/// the object chain (member objects, tags, TS wrappers; stopping at parentheses) ends in a
/// call, which `new` would take as its arguments.
pub(crate) fn member_on_needs_new_parens(cx: &Ctx<'_, '_>, object: NodeId) -> bool {
    let ast = cx.ast();
    let mut node = object;
    if is_parenthesized(cx, node) {
        return false;
    }
    for _ in 0..10_000 {
        node = match *ast.node(node) {
            Node::Paren { .. } => return false,
            Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::NonNull { expr }
            | Node::TypeAssertion { expr, .. }
            | Node::Instantiation { expr, .. } => expr,
            Node::Member { obj, .. } | Node::Index { obj, .. } => obj,
            Node::TaggedTemplate { tag, .. } => tag,
            Node::Call { .. } => return true,
            _ => return false,
        };
    }
    false
}

/// The deepest node whose span contains byte `pos`.
fn deepest_node_at(ast: &ts_r_ast::Ast, pos: u32) -> Option<NodeId> {
    let mut node = ast.root;
    let s = ast.span(node);
    if !(s.start <= pos && pos < s.end) {
        return None;
    }
    for _ in 0..100_000 {
        let mut next = None;
        ast.for_each_child(node, |c| {
            let s = ast.span(c);
            if next.is_none() && s.start <= pos && pos < s.end {
                next = Some(c);
            }
        });
        match next {
            Some(n) => node = n,
            None => return Some(node),
        }
    }
    Some(node)
}

/// unicorn's `needsSemicolon`: writing `code` at `pos` needs a `;` before it so it can't
/// continue the token before it (`foo\n[a]`), judged from that token and the node it ends.
pub(crate) fn needs_semicolon(cx: &Ctx<'_, '_>, pos: u32, code: &str) -> bool {
    if !code.starts_with(['[', '(', '/', '`', '+', '-', '*', ',', '.', '<']) {
        return false;
    }
    let ast = cx.ast();
    let src = cx.src();
    let end = skip_trivia_backward(ast, src, pos as usize) as u32;
    if end == 0 {
        return false;
    }
    let last = src[..end as usize].chars().next_back().unwrap_or(' ');
    if last == ';' {
        return false;
    }
    // The token's first byte: identifiers and numbers run back over word characters.
    let token_start = if is_ident_char(last) {
        let word = src[..end as usize]
            .char_indices()
            .rev()
            .take_while(|&(_, c)| is_ident_char(c) || c == '.')
            .last()
            .map_or(0, |(i, _)| i);
        word as u32
    } else {
        end - last.len_utf8() as u32
    };
    let Some(deepest) = deepest_node_at(ast, token_start) else {
        return false;
    };
    let mut node = Some(deepest);
    while let Some(n) = node {
        if cx.span(n).end != end {
            break;
        }
        if matches!(
            cx.node(n),
            Node::ExprStmt { .. }
                | Node::VarDecl { .. }
                | Node::Return { .. }
                | Node::Throw { .. }
                | Node::ExportDefaultExpr { .. }
        ) {
            return true;
        }
        node = cx.parent(n);
    }
    let parent_kind = cx.parent(deepest).map(|p| cx.node(p));
    match last {
        ']' => return true,
        '}' => {
            if matches!(cx.node(deepest), Node::Block { .. })
                && matches!(parent_kind, Some(Node::FunctionExpr { .. }))
                || matches!(cx.node(deepest), Node::ClassExpr { .. })
            {
                return true;
            }
        }
        ')' => {
            let body = match *cx.node(deepest) {
                Node::If { cons, .. } => Some(cons),
                Node::For { body, .. }
                | Node::ForIn { body, .. }
                | Node::ForOf { body, .. }
                | Node::While { body, .. }
                | Node::With { body, .. } => Some(body),
                Node::DoWhile { .. } => None,
                _ => None,
            };
            if let Some(body) = body
                && skip_trivia_backward(ast, src, cx.span(body).start as usize) as u32 == end
            {
                return false;
            }
            return true;
        }
        _ => {}
    }
    match cx.node(deepest) {
        Node::StringLit { .. }
        | Node::NullLit {}
        | Node::BoolLit { .. }
        | Node::NumberLit { .. }
        | Node::BigIntLit { .. }
        | Node::RegexLit { .. }
        | Node::ObjectLit { .. } => return true,
        Node::TemplateLit { .. } | Node::TaggedTemplate { .. } if last == '`' => return true,
        Node::NonNull { .. } if last == '!' => return true,
        Node::TypeArgs { .. } if matches!(parent_kind, Some(Node::Instantiation { .. })) => {
            return true;
        }
        _ => {}
    }
    if is_ident_char(last) {
        let word = &src[token_start as usize..end as usize];
        if is_reserved_word(word) {
            return false;
        }
        if word == "of" && matches!(cx.node(deepest), Node::ForOf { .. }) {
            return false;
        }
        return !(word == "await" && matches!(cx.node(deepest), Node::Await { .. }));
    }
    false
}

/// Words espree tokenizes as keywords rather than identifiers.
fn is_reserved_word(w: &str) -> bool {
    matches!(
        w,
        "break"
            | "case"
            | "catch"
            | "class"
            | "const"
            | "continue"
            | "debugger"
            | "default"
            | "delete"
            | "do"
            | "else"
            | "export"
            | "extends"
            | "finally"
            | "for"
            | "function"
            | "if"
            | "import"
            | "in"
            | "instanceof"
            | "new"
            | "return"
            | "super"
            | "switch"
            | "this"
            | "throw"
            | "try"
            | "typeof"
            | "var"
            | "void"
            | "while"
            | "with"
            | "yield"
            | "true"
            | "false"
            | "null"
    )
}

/// unicorn's `isLeftHandSide`: `id` (with its parentheses and TS wrappers) is written to:
/// an assignment or update target, a destructuring element, a `for-in/of` head, or a
/// `delete` operand.
pub(crate) fn is_left_hand_side(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let mut node = id;
    for _ in 0..10_000 {
        let Some(parent) = cx.parent(node) else {
            return false;
        };
        match *cx.node(parent) {
            Node::Paren { .. }
            | Node::As { .. }
            | Node::Satisfies { .. }
            | Node::NonNull { .. }
            | Node::TypeAssertion { .. }
            | Node::Instantiation { .. } => node = parent,
            Node::Update { .. } => return true,
            Node::Unary {
                op: UnaryOp::Delete,
                ..
            } => return true,
            Node::AssignPattern { left, .. } => return left == node,
            Node::Rest { .. } | Node::ArrayPattern { .. } => return true,
            _ => return is_assignment_target(ast, cx.file.parents, node),
        }
    }
    false
}

/// unicorn's `isNodeValueNotFunction`: literals, arrays, objects, classes, templates,
/// unary/update/arithmetic expressions, assignments, `await`, `new`, tagged templates,
/// `this`, `undefined` and calls other than `.bind(...)` can't (or most likely don't)
/// produce a function.
pub(crate) fn is_value_not_function(ast: &ts_r_ast::Ast, id: NodeId) -> bool {
    match *ast.node(id) {
        Node::ArrayLit { .. }
        | Node::ClassExpr { .. }
        | Node::StringLit { .. }
        | Node::NumberLit { .. }
        | Node::BigIntLit { .. }
        | Node::BoolLit { .. }
        | Node::NullLit {}
        | Node::RegexLit { .. }
        | Node::ObjectLit { .. }
        | Node::TemplateLit { .. }
        | Node::Unary { .. }
        | Node::Update { .. }
        | Node::Assign { .. }
        | Node::Await { .. }
        | Node::New { .. }
        | Node::TaggedTemplate { .. }
        | Node::This {} => true,
        Node::Binary { op, .. } => !matches!(
            op,
            BinaryOp::LogicalOr | BinaryOp::LogicalAnd | BinaryOp::Coalesce
        ),
        Node::Call {
            callee, optional, ..
        } => {
            let bind = matches!(*ast.node(skip_parens(ast, callee)),
                Node::Member { prop, optional: false, .. } if ident_name(ast, prop) == Some("bind"));
            optional || !bind
        }
        _ => ident_name(ast, id) == Some("undefined"),
    }
}
