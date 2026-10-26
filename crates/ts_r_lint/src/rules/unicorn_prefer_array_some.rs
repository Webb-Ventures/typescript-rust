//! `unicorn/prefer-array-some`: `.some(…)` over `.find(…)` / `.findLast(…)` used as a boolean,
//! `.findIndex(…) !== -1`-style checks and `.filter(…).length > 0`.
//!
//! - `find` / `findLast` (1–2 arguments, nothing optional, no type arguments): when the
//!   call is the left side of `=== undefined`-style comparisons (`==`/`!=`/`===`/`!==` with
//!   `undefined`, `==`/`!=` with `null`), used for its truthiness (`is_boolean_context`), or
//!   held in a lone `const` (no annotation, not exported) whose every other reference is a
//!   read in a boolean context (or, when the receiver isn't known to be an indexed
//!   collection, the argument of `Boolean(...)`). Suggestion only: rename to `some`, drop the
//!   comparison and put `!` before an `==`/`===` one; none when a comment would be dropped.
//! - `findIndex` / `findLastIndex` (1–2 arguments) compared `!== -1`, `!= -1`, `> -1`,
//!   `=== -1`, `== -1`, `>= 0` or `< 0`: the comparison is removed from after the token
//!   before the operator; `===`, `==`, `<` get a `!` (parenthesized when the binary is a
//!   member object, callee, tag, superclass or `**` base; a `;` when needed). No fix when a
//!   comment would go. `findLastIndex` is only fixed for side-effect-free inline callbacks
//!   (it walks backwards), otherwise suggested.
//! - `x.filter(f).length > 0` / `!== 0` (literal `0`): not for `$`-prefixed identifiers or a
//!   missing / non-function first argument. Fixed for side-effect-free inline callbacks,
//!   suggested otherwise.
//!
//! Receivers known not to be indexed collections are skipped.

use super::prelude::*;
use super::unicorn_types::{is_indexed_collection, is_known_non_indexed_collection};
use super::unicorn_util::parenthesized_span;
use super::unicorn_util_h5::{
    is_boolean_context, is_global_boolean_call, is_side_effect_free_function,
    is_value_not_function, method_call_of, needs_semicolon, parent_through_parens,
};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-array-some",
        "Prefer `.some(…)` over `.filter(…).length` check and `.{find,findLast,findIndex,findLastIndex}(…)`.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create_default::<PreferArraySome>,
};

#[derive(Debug, Default)]
struct PreferArraySome;

impl Rule for PreferArraySome {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call, NodeKind::Binary]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        match *cx.node(node) {
            Node::Call { .. } => check_find(cx, node),
            Node::Binary { .. } => {
                check_find_index(cx, node);
                check_filter(cx, node);
            }
            _ => {}
        }
    }
}

fn is_undefined_ident(ast: &ts_r_ast::Ast, id: NodeId) -> bool {
    ident_name(ast, skip_parens(ast, id)) == Some("undefined")
}

/// The comparison `call` is the left side of, when it checks for `undefined` / `null`.
fn undefined_check(cx: &Ctx<'_, '_>, call: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let (parent, held) = parent_through_parens(cx, call)?;
    let Node::Binary { left, op, right } = *cx.node(parent) else {
        return None;
    };
    if left != held {
        return None;
    }
    let ok = match op {
        BinaryOp::Eq | BinaryOp::NotEq => {
            is_undefined_ident(ast, right)
                || matches!(ast.node(skip_parens(ast, right)), Node::NullLit {})
        }
        BinaryOp::StrictEq | BinaryOp::StrictNotEq => is_undefined_ident(ast, right),
        _ => false,
    };
    ok.then_some(parent)
}

fn check_find(cx: &mut Ctx<'_, '_>, node: NodeId) {
    let ast = cx.ast();
    let Some((obj, prop, _)) =
        method_call_of(ast, node, &["find", "findLast"], 1, Some(2), Some(false))
    else {
        return;
    };
    if matches!(
        *cx.node(node),
        Node::Call {
            type_args: Some(_),
            ..
        }
    ) || is_known_non_indexed_collection(cx, obj)
    {
        return;
    }
    let compare = undefined_check(cx, node);
    if compare.is_none()
        && !is_boolean_context(cx, node)
        && !result_used_only_as_boolean(cx, node, obj)
    {
        return;
    }
    let method = ident_name(ast, prop).unwrap_or_default();
    let call_range = parenthesized_span(cx, node);
    let drops_comments = compare.is_some_and(|b| {
        !ast.comments_in(Span::new(call_range.end, cx.span(b).end))
            .is_empty()
    });
    let fix = (!drops_comments).then(|| {
        let mut fix = Fix::replace(ast.span(prop), "some");
        if let Some(b) = compare {
            fix = fix.and(Span::new(call_range.end, cx.span(b).end), "");
            if matches!(
                cx.node(b),
                Node::Binary {
                    op: BinaryOp::Eq | BinaryOp::StrictEq,
                    ..
                }
            ) {
                fix = fix.and(Span::at(call_range.start), "!");
            }
        }
        fix
    });
    let r = cx
        .report_node(prop, format!("Prefer `.some(…)` over `.{method}(…)`."))
        .id("some");
    if let Some(f) = fix {
        r.suggest(
            "some-suggestion",
            format!("Replace `.{method}(…)` with `.some(…)`."),
            f,
        );
    }
}

/// unicorn's `isFindResultVariableUsedOnlyAsBoolean`.
fn result_used_only_as_boolean(cx: &Ctx<'_, '_>, call: NodeId, receiver: NodeId) -> bool {
    let ast = cx.ast();
    let Some((decl, held)) = parent_through_parens(cx, call) else {
        return false;
    };
    let Node::VarDeclarator {
        name,
        init: Some(init),
        type_ann: None,
        ..
    } = *cx.node(decl)
    else {
        return false;
    };
    if init != held || !matches!(ast.node(name), Node::Ident { .. }) {
        return false;
    }
    let Some(list) = cx.parent(decl) else {
        return false;
    };
    match *cx.node(list) {
        Node::VarDecl {
            kind: VarKind::Const,
            modifiers,
            ..
        } if !modifiers.contains(Modifiers::EXPORT) => {}
        _ => return false,
    }
    let sem = cx.semantic();
    let Some(sym) = sem.symbol_of_decl(name) else {
        return false;
    };
    let symbol = sem.symbol(sym);
    if symbol.decls.len() != 1 {
        return false;
    }
    let refs: Vec<_> = symbol
        .refs
        .iter()
        .map(|&r| sem.reference(r))
        .filter(|r| r.node != name)
        .collect();
    if refs.is_empty() {
        return false;
    }
    let indexed = is_indexed_collection(cx, receiver);
    refs.iter().all(|r| {
        r.is_read()
            && !r.is_write()
            && if indexed {
                is_boolean_context(cx, r.node)
            } else {
                parent_through_parens(cx, r.node)
                    .is_some_and(|(p, _)| is_global_boolean_call(cx, p))
            }
    })
}

fn is_negative_one(ast: &ts_r_ast::Ast, id: NodeId) -> bool {
    matches!(*ast.node(id), Node::Unary { op: UnaryOp::Minus, arg }
        if matches!(*ast.node(skip_parens(ast, arg)), Node::NumberLit { value } if value.get() == 1.0))
}

fn is_zero(ast: &ts_r_ast::Ast, id: NodeId) -> bool {
    matches!(*ast.node(id), Node::NumberLit { value } if value.get() == 0.0)
}

fn check_find_index(cx: &mut Ctx<'_, '_>, node: NodeId) {
    let ast = cx.ast();
    let Node::Binary { left, op, right } = *cx.node(node) else {
        return;
    };
    let left_call = skip_parens(ast, left);
    let Some((obj, prop, args)) = method_call_of(
        ast,
        left_call,
        &["findIndex", "findLastIndex"],
        1,
        Some(2),
        Some(false),
    ) else {
        return;
    };
    let right = skip_parens(ast, right);
    let (op_text, negate) = match op {
        BinaryOp::StrictNotEq if is_negative_one(ast, right) => ("!==", false),
        BinaryOp::NotEq if is_negative_one(ast, right) => ("!=", false),
        BinaryOp::Gt if is_negative_one(ast, right) => (">", false),
        BinaryOp::StrictEq if is_negative_one(ast, right) => ("===", true),
        BinaryOp::Eq if is_negative_one(ast, right) => ("==", true),
        BinaryOp::GtEq if is_zero(ast, right) => (">=", false),
        BinaryOp::Lt if is_zero(ast, right) => ("<", true),
        _ => return,
    };
    if is_known_non_indexed_collection(cx, obj) {
        return;
    }
    let method = ident_name(ast, prop).unwrap_or_default();
    let fix = find_index_fix(cx, node, left, prop, op_text, negate);
    let side_effect_free = is_side_effect_free_function(cx, args[0]);
    let r = cx
        .report_node(prop, format!("Prefer `.some(…)` over `.{method}(…)`."))
        .id("some");
    if let Some(f) = fix {
        if method == "findLastIndex" && !side_effect_free {
            r.suggest(
                "some-suggestion",
                format!("Replace `.{method}(…)` with `.some(…)`."),
                f,
            );
        } else {
            r.fix(f);
        }
    }
}

fn find_index_fix(
    cx: &Ctx<'_, '_>,
    binary: NodeId,
    left: NodeId,
    prop: NodeId,
    op_text: &str,
    negate: bool,
) -> Option<Fix> {
    let ast = cx.ast();
    let src = cx.src();
    let left_end = parenthesized_span(cx, skip_parens(ast, left)).end;
    let op_start = skip_trivia_forward(src, left_end as usize) as u32;
    if !src[op_start as usize..].starts_with(op_text) {
        return None;
    }
    // The end of the token or comment just before the operator.
    let comments = ast.comments_between(left_end, op_start);
    let (start, line_comment) = match comments.last() {
        Some(c) => (c.span.end, src[c.span.start as usize..].starts_with("//")),
        None => (left_end, false),
    };
    let end = cx.span(binary).end;
    if line_comment || !ast.comments_in(Span::new(start, end)).is_empty() {
        return None;
    }
    let mut fix = Fix::replace(ast.span(prop), "some").and(Span::new(start, end), "");
    if negate {
        let mut prefix = "!".to_owned();
        if unary_replacement_needs_parens(cx, binary) {
            prefix = "(!".to_owned();
            fix = fix.and(Span::at(end), ")");
        }
        let begin = cx.span(binary).start;
        if needs_semicolon(cx, begin, &prefix) {
            prefix = format!(";{prefix}");
        }
        fix = fix.and(Span::at(begin), prefix);
    }
    Some(fix)
}

/// unicorn's `shouldAddParenthesesToUnaryExpressionReplacement`.
fn unary_replacement_needs_parens(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    // Climb TS wrappers (through parentheses) to the outermost one.
    let mut expression = node;
    let mut chain = vec![node];
    for _ in 0..10_000 {
        let Some((p, _)) = parent_through_parens(cx, expression) else {
            break;
        };
        match *cx.node(p) {
            Node::As { .. }
            | Node::Satisfies { .. }
            | Node::NonNull { .. }
            | Node::TypeAssertion { .. }
            | Node::Instantiation { .. } => {
                expression = p;
                chain.push(p);
            }
            _ => break,
        }
    }
    let Some((parent, held)) = parent_through_parens(cx, expression) else {
        return false;
    };
    let exp_left =
        matches!(*cx.node(parent), Node::Binary { op: BinaryOp::Exp, left, .. } if left == held);
    for &c in &chain {
        let parenthesized = cx
            .parent(c)
            .is_some_and(|p| matches!(cx.node(p), Node::Paren { .. }));
        let erased = matches!(
            cx.node(c),
            Node::As { .. } | Node::Satisfies { .. } | Node::TypeAssertion { .. }
        );
        if parenthesized && (!exp_left || !erased) {
            return false;
        }
    }
    exp_left
        || match *cx.node(parent) {
            Node::Member { obj, .. } | Node::Index { obj, .. } => obj == held,
            Node::Call { callee, .. } | Node::New { callee, .. } => callee == held,
            Node::TaggedTemplate { tag, .. } => tag == held,
            Node::ExprWithTypeArgs { expr, .. } => expr == held,
            _ => false,
        }
}

fn check_filter(cx: &mut Ctx<'_, '_>, node: NodeId) {
    let ast = cx.ast();
    let Node::Binary { left, op, right } = *cx.node(node) else {
        return;
    };
    if !matches!(op, BinaryOp::Gt | BinaryOp::StrictNotEq) {
        return;
    }
    let right = skip_parens(ast, right);
    if !matches!(ast.node(right), Node::NumberLit { .. }) || cx.text(right) != "0" {
        return;
    }
    let length = skip_parens(ast, left);
    let Node::Member {
        obj: filter_call,
        prop: length_prop,
        optional: false,
    } = *ast.node(length)
    else {
        return;
    };
    if ident_name(ast, length_prop) != Some("length") {
        return;
    }
    let filter_call = skip_parens(ast, filter_call);
    let Some((receiver, filter_prop, args)) =
        method_call_of(ast, filter_call, &["filter"], 0, None, Some(false))
    else {
        return;
    };
    if ident_name(ast, receiver).is_some_and(|n| n.starts_with('$'))
        || is_known_non_indexed_collection(cx, receiver)
    {
        return;
    }
    let Some(&first) = args.first() else {
        return;
    };
    if is_value_not_function(ast, skip_parens(ast, first)) {
        return;
    }
    let end = cx.span(node).end;
    let filter_end = parenthesized_span(cx, filter_call).end;
    let fix = ast
        .comments_in(Span::new(filter_end, end))
        .is_empty()
        .then(|| {
            let length_end = parenthesized_span(cx, length).end;
            Fix::replace(ast.span(filter_prop), "some")
                .and(Span::new(filter_end, cx.span(length).end), "")
                .and(Span::new(length_end, end), "")
        });
    let side_effect_free = is_side_effect_free_function(cx, first);
    let r = cx
        .report_node(
            filter_prop,
            "Prefer `.some(…)` over non-zero length check from `.filter(…)`.",
        )
        .id("filter");
    if let Some(f) = fix {
        if side_effect_free {
            r.fix(f);
        } else {
            r.suggest(
                "some-suggestion",
                "Replace `.filter(…)` with `.some(…)`.",
                f,
            );
        }
    }
}
