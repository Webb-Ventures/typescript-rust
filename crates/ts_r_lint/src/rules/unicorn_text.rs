//! Source-text helpers for eslint-plugin-unicorn fixes: parentheses as ESTree sees them
//! (a node's own parentheses are `Paren` parents here), comments a replacement would drop,
//! when a moved expression needs new parentheses, and when replacement text starting with
//! `(`/`[`/... needs a `;` before it.

#![allow(dead_code)] // A shared toolkit: each rule uses part of it.

use super::prelude::*;
use ts_r_ast::Ast;

/// The node has its own parentheses.
pub(crate) fn is_parenthesized(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let id = skip_parens(cx.ast(), id);
    cx.parent(id)
        .is_some_and(|p| matches!(cx.node(p), Node::Paren { .. }))
}

/// The span of `id` with all its parentheses.
pub(crate) fn paren_span(cx: &Ctx<'_, '_>, id: NodeId) -> Span {
    let mut id = skip_parens(cx.ast(), id);
    while let Some(p) = cx.parent(id) {
        if !matches!(cx.node(p), Node::Paren { .. }) {
            break;
        }
        id = p;
    }
    cx.span(id)
}

/// The text of `id` with all its parentheses.
pub(crate) fn paren_text<'a>(cx: &Ctx<'a, '_>, id: NodeId) -> &'a str {
    paren_span(cx, id).text(cx.src())
}

fn inside(inner: Span, outer: Span) -> bool {
    inner.start >= outer.start && inner.end <= outer.end
}

/// `wouldRemoveComments`: a comment inside `span` that none of `kept` contains.
pub(crate) fn would_remove_comments(cx: &Ctx<'_, '_>, span: Span, kept: &[Span]) -> bool {
    cx.ast()
        .comments_in(span)
        .iter()
        .any(|c| inside(c.span, span) && kept.iter().all(|k| !inside(c.span, *k)))
}

/// `getExpressionText`: the node's text (wrapped when `parens`), or its parenthesized text
/// as is when the dropped parentheses would take comments with them.
pub(crate) fn expression_text(cx: &Ctx<'_, '_>, id: NodeId, parens: bool) -> String {
    let id = skip_parens(cx.ast(), id);
    if is_parenthesized(cx, id) && would_remove_comments(cx, paren_span(cx, id), &[cx.span(id)]) {
        return paren_text(cx, id).to_owned();
    }
    let text = cx.text(id);
    if parens {
        format!("({text})")
    } else {
        text.to_owned()
    }
}

/// `getCallArgumentText`: the text for one call argument (a sequence is wrapped).
pub(crate) fn call_argument_text(cx: &Ctx<'_, '_>, id: NodeId) -> String {
    let id = skip_parens(cx.ast(), id);
    expression_text(cx, id, matches!(cx.node(id), Node::Seq { .. }))
}

fn starts_with_word(text: &str, word: &str) -> bool {
    text.strip_prefix(word)
        .is_some_and(|rest| !rest.starts_with(is_ident_char))
}

/// `startsWithDeclarationLikeToken`: the text starts with `{`, `class`, `function`,
/// `async function` or `let [`.
pub(crate) fn starts_with_declaration_like_token(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let src = cx.src();
    let span = cx.span(id);
    let text = span.text(src);
    if text.starts_with('{')
        || starts_with_word(text, "class")
        || starts_with_word(text, "function")
    {
        return true;
    }
    let after = |word: &str| -> &str {
        let pos = skip_trivia_forward(src, span.start as usize + word.len());
        &src[pos.min(src.len())..]
    };
    (starts_with_word(text, "async") && starts_with_word(after("async"), "function"))
        || (starts_with_word(text, "let") && after("let").starts_with('['))
}

/// `shouldAddParenthesesToExpressionStart`: putting `expr` where `replaced` is would make a
/// statement (or arrow body, `export default`, `for` head) start with a block or
/// declaration token.
pub(crate) fn needs_parens_at_expression_start(
    cx: &Ctx<'_, '_>,
    expr: NodeId,
    replaced: NodeId,
) -> bool {
    let ast = cx.ast();
    let text = cx.text(expr);
    let regexp = text.starts_with('/');
    if !regexp && !starts_with_declaration_like_token(cx, expr) {
        return false;
    }
    let mut node = replaced;
    for _ in 0..10_000 {
        let Some(parent) = cx.parent(node) else {
            return false;
        };
        if matches!(ast.node(parent), Node::Paren { .. }) {
            return false;
        }
        match *ast.node(parent) {
            Node::Arrow { body, .. } if body == node => return text.starts_with('{'),
            Node::ExportDefaultExpr { .. } => {
                return !regexp && !text.starts_with('{') && !starts_with_word(text, "let");
            }
            Node::For { init: Some(i), .. } if i == node => return starts_with_word(text, "let"),
            _ => {}
        }
        if matches!(ast.node(parent), Node::ExprStmt { .. }) {
            return !regexp;
        }
        if cx.span(parent).start != cx.span(node).start {
            return false;
        }
        node = parent;
    }
    false
}

/// `shouldAddParenthesesToMemberExpressionObject`: `expr` (unparenthesized) needs
/// parentheses to be the object of a member access placed where `replaced` is.
pub(crate) fn member_object_needs_parens(cx: &Ctx<'_, '_>, expr: NodeId, replaced: NodeId) -> bool {
    if needs_parens_at_expression_start(cx, expr, replaced) {
        return true;
    }
    match *cx.node(expr) {
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
        | Node::NonNull { .. }
        | Node::StringLit { .. }
        | Node::NullLit {}
        | Node::BoolLit { .. }
        | Node::RegexLit { .. }
        | Node::BigIntLit { .. } => false,
        Node::New { has_args, .. } => !has_args,
        Node::NumberLit { .. } => cx.text(expr).bytes().all(|b| b.is_ascii_digit()),
        _ => true,
    }
}

/// `getMemberExpressionObjectText`: the text of `expr` as a member object, keeping its
/// parentheses and adding them when needed.
pub(crate) fn member_object_text(cx: &Ctx<'_, '_>, expr: NodeId, replaced: NodeId) -> String {
    let expr = skip_parens(cx.ast(), expr);
    let text = paren_text(cx, expr);
    if !is_parenthesized(cx, expr) && member_object_needs_parens(cx, expr, replaced) {
        format!("({text})")
    } else {
        text.to_owned()
    }
}

fn is_erased_assertion(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(id),
        Node::As { .. } | Node::Satisfies { .. } | Node::TypeAssertion { .. }
    )
}

fn is_ts_expression(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(id),
        Node::As { .. }
            | Node::Satisfies { .. }
            | Node::TypeAssertion { .. }
            | Node::NonNull { .. }
            | Node::Instantiation { .. }
    )
}

/// The parent, skipping parentheses, and the child on the path (outermost paren).
fn parent_skip_paren(cx: &Ctx<'_, '_>, id: NodeId) -> Option<(NodeId, NodeId)> {
    let mut cur = id;
    loop {
        let p = cx.parent(cur)?;
        if matches!(cx.node(p), Node::Paren { .. }) {
            cur = p;
        } else {
            return Some((p, cur));
        }
    }
}

/// `shouldAddParenthesesToUnaryExpressionReplacement`: replacing `node` with `!x` needs
/// parentheses where it stands (a `**` left operand, member object, callee, tag or
/// superclass), unless it is already parenthesized.
pub(crate) fn unary_replacement_needs_parens(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    // The outermost TS wrapper around `node` (parentheses included).
    let mut expr = node;
    while let Some((p, _)) = parent_skip_paren(cx, expr) {
        if is_ts_expression(ast, p) {
            expr = p;
        } else {
            break;
        }
    }
    let Some((parent, _)) = parent_skip_paren(cx, expr) else {
        return false;
    };
    let exp_left = matches!(*ast.node(parent), Node::Binary { op: BinaryOp::Exp, left, .. }
        if skip_parens(ast, left) == expr);
    let mut cur = node;
    for _ in 0..10_000 {
        if is_parenthesized(cx, cur) && (!exp_left || !is_erased_assertion(ast, cur)) {
            return false;
        }
        if cur == expr {
            break;
        }
        match parent_skip_paren(cx, cur) {
            Some((p, _)) => cur = p,
            None => break,
        }
    }
    exp_left
        || match *ast.node(parent) {
            Node::Member { obj, .. } | Node::Index { obj, .. } => skip_parens(ast, obj) == expr,
            Node::Call { callee, .. } | Node::New { callee, .. } => {
                skip_parens(ast, callee) == expr
            }
            Node::TaggedTemplate { tag, .. } => skip_parens(ast, tag) == expr,
            Node::ClassDecl { .. } | Node::ClassExpr { .. } => true,
            Node::ExprWithTypeArgs { .. } | Node::HeritageClause { .. } => true,
            _ => false,
        }
}

/// The deepest non-parenthesis node whose span contains `pos`.
fn node_at(cx: &Ctx<'_, '_>, pos: u32) -> Option<NodeId> {
    let ast = cx.ast();
    let mut cur = ast.root;
    if !(ast.span(cur).start <= pos && pos < ast.span(cur).end) {
        return None;
    }
    for _ in 0..100_000 {
        let mut next = None;
        ast.for_each_child(cur, |c| {
            let s = ast.span(c);
            if next.is_none() && s.start <= pos && pos < s.end {
                next = Some(c);
            }
        });
        match next {
            Some(n) => cur = n,
            None => break,
        }
    }
    while matches!(ast.node(cur), Node::Paren { .. }) {
        cur = cx.parent(cur)?;
    }
    Some(cur)
}

const KEYWORDS: &[&str] = &[
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "export",
    "extends",
    "finally",
    "for",
    "function",
    "if",
    "import",
    "in",
    "instanceof",
    "new",
    "return",
    "super",
    "switch",
    "this",
    "throw",
    "try",
    "typeof",
    "var",
    "void",
    "while",
    "with",
    "yield",
];

/// `needsSemicolon`: text `code` placed at `pos` (the start of a replaced node) needs a
/// `;` before it so it doesn't continue the previous token's statement.
pub(crate) fn needs_semicolon(cx: &Ctx<'_, '_>, pos: u32, code: &str) -> bool {
    if code.is_empty() || !code.starts_with(['[', '(', '/', '`', '+', '-', '*', ',', '.', '<']) {
        return false;
    }
    let ast = cx.ast();
    let src = cx.src();
    let end = skip_trivia_backward(ast, src, pos as usize);
    if end == 0 {
        return false;
    }
    let before = &src[..end];
    let Some(last) = before.chars().next_back() else {
        return false;
    };
    if last == ';' {
        return false;
    }
    let tok_start = (end - last.len_utf8()) as u32;
    let Some(deepest) = node_at(cx, tok_start) else {
        return false;
    };
    let end = end as u32;
    let mut n = Some(deepest);
    while let Some(id) = n {
        if cx.span(id).end != end {
            break;
        }
        if matches!(
            cx.node(id),
            Node::ExprStmt { .. }
                | Node::VarDecl { .. }
                | Node::Return { .. }
                | Node::Throw { .. }
                | Node::ExportDefaultExpr { .. }
        ) {
            return true;
        }
        n = cx.parent(id);
    }
    match last {
        ']' => return true,
        '}' => {
            let p = cx.parent(deepest).map(|p| cx.node(p));
            return (matches!(cx.node(deepest), Node::Block { .. })
                && matches!(p, Some(Node::FunctionExpr { .. })))
                || matches!(
                    cx.node(deepest),
                    Node::ClassExpr { .. } | Node::ObjectLit { .. }
                );
        }
        ')' => {
            let body = match *cx.node(deepest) {
                Node::If { cons, .. } => Some(cons),
                Node::For { body, .. }
                | Node::ForIn { body, .. }
                | Node::ForOf { body, .. }
                | Node::While { body, .. }
                | Node::DoWhile { body, .. }
                | Node::With { body, .. } => Some(body),
                _ => None,
            };
            if let Some(b) = body
                && skip_trivia_backward(ast, src, cx.span(b).start as usize) as u32 == end
            {
                return false;
            }
            return true;
        }
        '`' => return true,
        _ => {}
    }
    if cx.span(deepest).end == end
        && matches!(
            cx.node(deepest),
            Node::StringLit { .. }
                | Node::NumberLit { .. }
                | Node::BigIntLit { .. }
                | Node::NullLit {}
                | Node::BoolLit { .. }
                | Node::RegexLit { .. }
        )
    {
        return true;
    }
    if (last == '!' && matches!(cx.node(deepest), Node::NonNull { .. }))
        || (last == '>'
            && matches!(cx.node(deepest), Node::TypeArgs { .. })
            && cx
                .parent(deepest)
                .is_some_and(|p| matches!(cx.node(p), Node::Instantiation { .. })))
    {
        return true;
    }
    if is_ident_char(last) {
        let word_start = before
            .char_indices()
            .rev()
            .take_while(|&(_, c)| is_ident_char(c))
            .last()
            .map_or(0, |(i, _)| i);
        let word = &before[word_start..];
        if word.starts_with(|c: char| c.is_ascii_digit()) {
            return true;
        }
        if matches!(word, "null" | "true" | "false") {
            return true;
        }
        if KEYWORDS.contains(&word) {
            return false;
        }
        if word == "of" && matches!(cx.node(deepest), Node::ForOf { .. }) {
            return false;
        }
        return !(word == "await" && matches!(cx.node(deepest), Node::Await { .. }));
    }
    false
}

/// Any comment entirely inside `span`.
pub(crate) fn has_comment_in(cx: &Ctx<'_, '_>, span: Span) -> bool {
    would_remove_comments(cx, span, &[])
}

/// `hasPossibleSideEffect`: eslint-utils' `hasSideEffect`, or a tagged template or spread
/// anywhere inside.
pub(crate) fn has_possible_side_effect(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    if super::unicorn_static::has_side_effect(ast, id) {
        return true;
    }
    let mut stack = vec![id];
    let mut guard = 0;
    while let Some(n) = stack.pop() {
        guard += 1;
        if guard > 1_000_000 {
            return true;
        }
        if matches!(
            ast.node(n),
            Node::TaggedTemplate { .. } | Node::Spread { .. }
        ) {
            return true;
        }
        ast.for_each_child(n, |c| stack.push(c));
    }
    false
}

/// A string literal or an expression-free template: its value.
pub(crate) fn static_string_value(ast: &Ast, id: NodeId) -> Option<String> {
    match *ast.node(id) {
        Node::StringLit { value } => Some(ast.atom(value).to_owned()),
        Node::TemplateLit { parts } => match ast.list(parts) {
            [e] => match *ast.node(*e) {
                Node::TemplateElement {
                    cooked,
                    invalid_escape: false,
                } => Some(ast.atom(cooked).to_owned()),
                _ => None,
            },
            _ => None,
        },
        _ => None,
    }
}

fn unwrap_chain_and_ts(ast: &Ast, mut id: NodeId) -> NodeId {
    loop {
        match *ast.node(id) {
            Node::Paren { expr }
            | Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::NonNull { expr }
            | Node::TypeAssertion { expr, .. }
            | Node::Instantiation { expr, .. } => id = expr,
            _ => return id,
        }
    }
}

/// The static property name of a member access (`a.b`, `a["b"]`, ``a[`b`]``, `a[1]`).
fn static_member_name(ast: &Ast, id: NodeId) -> Option<String> {
    match *ast.node(id) {
        Node::Member { prop, .. } => match *ast.node(prop) {
            Node::Ident { name } => Some(ast.atom(name).to_owned()),
            _ => None,
        },
        Node::Index { index, .. } => {
            let index = skip_parens(ast, index);
            match *ast.node(index) {
                Node::NumberLit { value } => Some(number_to_string(value.get())),
                _ => static_string_value(ast, index),
            }
        }
        _ => None,
    }
}

/// `isSameReference`: the two expressions name the same value (`a`, `this.x`, `a["b"]` and
/// `a.b`, equal literals).
pub(crate) fn is_same_reference(cx: &Ctx<'_, '_>, a: NodeId, b: NodeId) -> bool {
    same_reference(cx.ast(), a, b, 0)
}

fn same_reference(ast: &Ast, a: NodeId, b: NodeId, depth: u32) -> bool {
    if depth > 200 {
        return false;
    }
    let a = unwrap_chain_and_ts(ast, a);
    let b = unwrap_chain_and_ts(ast, b);
    if let Some(s) = static_string_value(ast, a) {
        return static_string_value(ast, b).as_deref() == Some(s.as_str());
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
            if let Some(name) = static_member_name(ast, a) {
                return same_reference(ast, o1, o2, depth + 1)
                    && static_member_name(ast, b).as_deref() == Some(name.as_str());
            }
            match (*ast.node(a), *ast.node(b)) {
                (Node::Member { prop: p1, .. }, Node::Member { prop: p2, .. })
                | (Node::Index { index: p1, .. }, Node::Index { index: p2, .. }) => {
                    same_reference(ast, o1, o2, depth + 1) && same_reference(ast, p1, p2, depth + 1)
                }
                _ => false,
            }
        }
        _ => false,
    }
}
