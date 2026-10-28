//! `unicorn/prefer-simplified-conditions`: De Morgan rewrites that remove negations,
//! absorption (`a || (a && b)` is `a`) and factoring a common leading term
//! (`a && b || a && c` is `a && (b || c)`).
//!
//! - `!(a && b)` becomes `!a || !b` when the operands' negations weigh less than the
//!   original (`negatedConditions: "expand"`: always). A double negation of a boolean (or
//!   in a control-flow test) drops; an equality flips its operator.
//! - Absorption needs a plain replacement (identifier, literal, `this`, static string);
//!   dropping the other operand needs it to be a literal, a parameter/function/`var`
//!   name (or a negation of one) and the value used as a boolean (or both booleans). No
//!   fix in a reference context (callee, tag, `delete` operand).
//! - Factoring needs simple, chain-free operands (names, literals, calls, plain members,
//!   `!`, `===`/`!==` of those) and a boolean context or boolean operands; operands with
//!   side effects (other than `Array.isArray`-like static checks) make it a suggestion.
//!
//! Comments inside the expression withhold the fix.

use super::prelude::*;
use super::unicorn_conditions::is_ts_wrapper;
use super::unicorn_conditions::{negated_expression_text, unwrap_ts_and_parens};
use super::unicorn_no_duplicate_logical_operands::is_reference_context;
use super::unicorn_no_redundant_comparison::contains_optional_chain;
use super::unicorn_no_top_level_side_effects::is_global_reference;
use super::unicorn_static::has_side_effect;
use super::unicorn_text::{
    is_parenthesized, needs_parens_at_expression_start, needs_semicolon, paren_text,
    static_string_value,
};
use super::unicorn_types::is_boolean;
use super::unicorn_util::is_boolean_context;
use super::unicorn_util::space_around_keyword;
use super::unicorn_util::{is_same_reference, logical_operands};
use crate::semantic::DeclKind;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-simplified-conditions",
        "Prefer simplified conditions.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create::<PreferSimplifiedConditions>,
};

#[derive(Debug)]
struct PreferSimplifiedConditions {
    expand: bool,
}

impl FromOptions for PreferSimplifiedConditions {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let obj = obj_opt(opts, 0);
        Ok(PreferSimplifiedConditions {
            expand: obj.get("negatedConditions").and_then(Value::as_str) == Some("expand"),
        })
    }
}

const MAX_DEPTH: u32 = 200;

fn negation_arg(ast: &Ast, id: NodeId) -> Option<NodeId> {
    match *ast.node(skip_parens(ast, id)) {
        Node::Unary {
            op: UnaryOp::Not,
            arg,
        } => Some(arg),
        _ => None,
    }
}

fn logical(ast: &Ast, id: NodeId) -> Option<(NodeId, BinaryOp, NodeId)> {
    match *ast.node(skip_parens(ast, id)) {
        Node::Binary { left, op, right }
            if matches!(
                op,
                BinaryOp::LogicalAnd | BinaryOp::LogicalOr | BinaryOp::Coalesce
            ) =>
        {
            Some((left, op, right))
        }
        _ => None,
    }
}

fn negated_logical(op: BinaryOp) -> Option<BinaryOp> {
    match op {
        BinaryOp::LogicalAnd => Some(BinaryOp::LogicalOr),
        BinaryOp::LogicalOr => Some(BinaryOp::LogicalAnd),
        _ => None,
    }
}

fn equality(ast: &Ast, id: NodeId) -> Option<(NodeId, BinaryOp, NodeId)> {
    match *ast.node(skip_parens(ast, id)) {
        Node::Binary { left, op, right }
            if matches!(
                op,
                BinaryOp::Eq | BinaryOp::NotEq | BinaryOp::StrictEq | BinaryOp::StrictNotEq
            ) =>
        {
            Some((left, op, right))
        }
        _ => None,
    }
}

fn negated_equality(op: BinaryOp) -> &'static str {
    match op {
        BinaryOp::Eq => "!=",
        BinaryOp::NotEq => "==",
        BinaryOp::StrictEq => "!==",
        _ => "===",
    }
}

fn parent(cx: &Ctx<'_, '_>, id: NodeId) -> Option<NodeId> {
    parent_skip_parens(cx.ast(), cx.file.parents, id)
}

/// `isControlFlowTest`: the test of an `if`/`?:`/loop, through logical operands.
fn is_control_flow_test(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let mut cur = skip_parens(ast, id);
    for _ in 0..10_000 {
        let Some(p) = parent(cx, cur) else {
            return false;
        };
        let held = |t: NodeId| skip_parens(ast, t) == cur;
        match *ast.node(p) {
            Node::If { test, .. }
            | Node::Conditional { test, .. }
            | Node::While { test, .. }
            | Node::DoWhile { test, .. } => return held(test),
            Node::For { test, .. } => return test.is_some_and(held),
            _ if logical(ast, p).is_some() => cur = p,
            _ => return false,
        }
    }
    false
}

fn is_literal(node: &Node) -> bool {
    matches!(
        node,
        Node::StringLit { .. }
            | Node::NumberLit { .. }
            | Node::BigIntLit { .. }
            | Node::BoolLit { .. }
            | Node::NullLit {}
            | Node::RegexLit { .. }
    )
}

fn negation_weight(ast: &Ast, id: NodeId, depth: u32) -> u32 {
    if depth > MAX_DEPTH {
        return 0;
    }
    let id = unwrap_ts_and_parens(ast, id);
    if let Some(arg) = negation_arg(ast, id) {
        return 1 + negation_weight(ast, arg, depth + 1);
    }
    match *ast.node(id) {
        Node::Binary { left, op, right } => {
            let own = u32::from(matches!(op, BinaryOp::StrictNotEq | BinaryOp::NotEq));
            own + negation_weight(ast, left, depth + 1) + negation_weight(ast, right, depth + 1)
        }
        _ => 0,
    }
}

fn negated_expression_weight(cx: &Ctx<'_, '_>, id: NodeId, truthiness: bool) -> u32 {
    let ast = cx.ast();
    let id = unwrap_ts_and_parens(ast, id);
    if let Some(arg) = negation_arg(ast, id) {
        return if truthiness || is_boolean(cx, arg) {
            negation_weight(ast, arg, 0)
        } else {
            1 + negation_weight(ast, id, 0)
        };
    }
    if let Some((left, op, right)) = equality(ast, id) {
        let own = u32::from(matches!(op, BinaryOp::StrictEq | BinaryOp::Eq));
        return own + negation_weight(ast, left, 0) + negation_weight(ast, right, 0);
    }
    1 + negation_weight(ast, id, 0)
}

/// `shouldAddParenthesesToLogicalReplacement`: a `!`/logical replacement of `node` needs
/// parentheses where it sits.
fn replacement_needs_parens(cx: &Ctx<'_, '_>, node: NodeId, op: BinaryOp) -> bool {
    if is_parenthesized(cx, node) {
        return false;
    }
    let Some(p) = parent(cx, node) else {
        return false;
    };
    if let Some((_, parent_op, _)) = logical(cx.ast(), p) {
        return parent_op != op;
    }
    is_ts_wrapper(cx.node(p))
        || matches!(
            cx.node(p),
            Node::Unary { .. } | Node::Await { .. } | Node::Binary { .. }
        )
}

fn replacement_text(cx: &Ctx<'_, '_>, node: NodeId, op: BinaryOp, text: String) -> String {
    let text = if replacement_needs_parens(cx, node, op) {
        format!("({text})")
    } else {
        text
    };
    if needs_semicolon(cx, cx.span(node).start, &text) {
        format!(";{text}")
    } else {
        text
    }
}

/// `node`'s text with its operator replaced.
fn replaced_operator_text(cx: &Ctx<'_, '_>, node: NodeId, left: NodeId, op: &str) -> String {
    let src = cx.src();
    let span = cx.span(node);
    let op_start = skip_trivia_forward(src, super::unicorn_text::paren_span(cx, left).end as usize);
    let Node::Binary { op: old, .. } = *cx.node(node) else {
        return cx.text(node).to_owned();
    };
    let op_end = op_start + old.as_str().len();
    format!(
        "{}{op}{}",
        &src[span.start as usize..op_start],
        &src[op_end..span.end as usize]
    )
}

fn negated_operand_text(cx: &Ctx<'_, '_>, id: NodeId, truthiness: bool) -> String {
    let ast = cx.ast();
    let id = skip_parens(ast, id);
    if let Some(arg) = negation_arg(ast, id) {
        if truthiness || is_boolean(cx, arg) {
            return paren_text(cx, arg).to_owned();
        }
        return format!("!{}", paren_text(cx, id));
    }
    if let Some((left, op, _)) = equality(ast, id) {
        return replaced_operator_text(cx, id, left, negated_equality(op));
    }
    negated_expression_text(cx, id)
}

fn de_morgan_fix(cx: &Ctx<'_, '_>, node: NodeId, left: NodeId, op: BinaryOp, right: NodeId) -> Fix {
    let ast = cx.ast();
    let src = cx.src();
    let new_op = negated_logical(op).unwrap_or(op);
    let truthiness = is_control_flow_test(cx, node);
    let mut text = format!(
        "{} {} {}",
        negated_operand_text(cx, left, truthiness),
        new_op.as_str(),
        negated_operand_text(cx, right, truthiness)
    );
    let left_inner = skip_parens(ast, left);
    let leading = if let Some((l, _, _)) = equality(ast, left_inner) {
        Some(l)
    } else {
        negation_arg(ast, left_inner).filter(|&a| truthiness || is_boolean(cx, a))
    };
    if let Some(lead) = leading
        && !is_parenthesized(cx, lead)
        && !replacement_needs_parens(cx, node, new_op)
        && needs_parens_at_expression_start(cx, skip_parens(ast, lead), node)
    {
        text = format!("({text})");
    }
    let text = replacement_text(cx, node, new_op, text);
    let span = cx.span(node);
    let statement = parent(cx, node).filter(|&p| {
        matches!(*ast.node(p), Node::Return { arg: Some(a) } | Node::Throw { arg: a } if skip_parens(ast, a) == node)
    });
    let after_bang = skip_trivia_forward(src, span.start as usize + 1);
    let broken =
        src[span.start as usize..after_bang].contains(['\n', '\r', '\u{2028}', '\u{2029}']);
    match statement {
        Some(stmt) if broken && !is_parenthesized(cx, node) => {
            let stmt_span = cx.span(stmt);
            let keyword_end = stmt_span.start
                + if matches!(ast.node(stmt), Node::Return { .. }) {
                    6
                } else {
                    5
                };
            let close = if src[..stmt_span.end as usize].ends_with(';') {
                stmt_span.end - 1
            } else {
                stmt_span.end
            };
            Fix::replace(Span::new(keyword_end, keyword_end), " (")
                .and(span, text)
                .and(Span::new(close, close), ")")
        }
        _ => {
            let mut fix = Fix::replace(span, text);
            for (s, t) in space_around_keyword(cx, node) {
                fix = fix.and(s, t);
            }
            fix
        }
    }
}

fn is_simple_replacement(ast: &Ast, id: NodeId) -> bool {
    let id = unwrap_ts_and_parens(ast, id);
    matches!(ast.node(id), Node::Ident { .. } | Node::This {})
        || is_literal(ast.node(id))
        || static_string_value(ast, id).is_some()
}

fn is_identifier_safe_to_drop(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    if is_global_reference(cx, id) {
        return false;
    }
    let sem = cx.semantic();
    let Some(sym) = sem.reference_at(id).and_then(|r| sem.reference(r).symbol()) else {
        return false;
    };
    sem.symbol(sym).decls.first().is_some_and(|d| {
        matches!(
            d.kind,
            DeclKind::Param | DeclKind::Function | DeclKind::FunctionExprName | DeclKind::Var
        )
    })
}

fn is_safe_to_drop(cx: &Ctx<'_, '_>, id: NodeId, depth: u32) -> bool {
    let ast = cx.ast();
    let id = unwrap_ts_and_parens(ast, id);
    if depth > MAX_DEPTH {
        return false;
    }
    if is_literal(ast.node(id)) || static_string_value(ast, id).is_some() {
        return true;
    }
    if matches!(ast.node(id), Node::Ident { .. }) {
        return is_identifier_safe_to_drop(cx, id);
    }
    negation_arg(ast, id).is_some_and(|a| is_safe_to_drop(cx, a, depth + 1))
}

fn can_drop(cx: &Ctx<'_, '_>, node: NodeId, replacement: NodeId, removable: NodeId) -> bool {
    is_safe_to_drop(cx, removable, 0)
        && (is_boolean_context(cx, node)
            || (is_boolean(cx, replacement) && is_boolean(cx, removable)))
}

fn is_factoring_operand(ast: &Ast, id: NodeId, depth: u32) -> bool {
    if depth > MAX_DEPTH {
        return false;
    }
    let id = unwrap_ts_and_parens(ast, id);
    match *ast.node(id) {
        Node::Ident { .. } | Node::This {} | Node::Super {} | Node::Call { .. } => true,
        ref n if is_literal(n) => true,
        Node::Member { obj, .. } => is_factoring_operand(ast, obj, depth + 1),
        Node::Index { obj, index, .. } => {
            is_factoring_operand(ast, obj, depth + 1) && is_factoring_operand(ast, index, depth + 1)
        }
        Node::TemplateLit { parts } => ast.list(parts).len() <= 1,
        Node::Unary {
            op: UnaryOp::Not,
            arg,
        } => is_factoring_operand(ast, arg, depth + 1),
        Node::Binary {
            left,
            op: BinaryOp::StrictEq | BinaryOp::StrictNotEq,
            right,
        } => {
            is_factoring_operand(ast, left, depth + 1)
                && is_factoring_operand(ast, right, depth + 1)
        }
        _ => false,
    }
}

fn is_safe_static_call(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    const METHODS: &[(&str, &str)] = &[
        ("Array", "isArray"),
        ("ArrayBuffer", "isView"),
        ("Error", "isError"),
        ("Number", "isFinite"),
        ("Number", "isInteger"),
        ("Number", "isNaN"),
        ("Number", "isSafeInteger"),
    ];
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
    let Node::Member {
        obj,
        optional: false,
        prop,
    } = *ast.node(skip_parens(ast, callee))
    else {
        return false;
    };
    let args = ast.list(args);
    let obj = skip_parens(ast, obj);
    let (Some(o), Some(m)) = (ident_name(ast, obj), ident_name(ast, prop)) else {
        return false;
    };
    args.len() == 1
        && METHODS.contains(&(o, m))
        && is_global_reference(cx, obj)
        && args.iter().all(|&a| {
            !matches!(ast.node(a), Node::Spread { .. })
                && is_factoring_operand(ast, a, 0)
                && !has_side_effect(ast, a)
        })
}

fn is_safe_factoring_operand(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let id = unwrap_ts_and_parens(cx.ast(), id);
    !has_side_effect(cx.ast(), id) || is_safe_static_call(cx, id)
}

fn is_same_condition(cx: &Ctx<'_, '_>, a: NodeId, b: NodeId) -> bool {
    let ast = cx.ast();
    is_same_reference(cx, a, b)
        || (is_factoring_operand(ast, a, 0)
            && is_factoring_operand(ast, b, 0)
            && cx.text(skip_parens(ast, a)) == cx.text(skip_parens(ast, b)))
}

/// `a || (a && b)` and the like: the operand the whole expression equals.
fn absorption_term(cx: &Ctx<'_, '_>, node: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let (left, op, right) = logical(ast, node)?;
    let inner = negated_logical(op)?;
    if let Some((rl, rop, rr)) = logical(ast, right)
        && rop == inner
        && is_simple_replacement(ast, left)
    {
        if is_same_condition(cx, left, rl) {
            return Some(left);
        }
        if is_same_condition(cx, left, rr) && can_drop(cx, node, left, rl) {
            return Some(left);
        }
    }
    if let Some((ll, lop, lr)) = logical(ast, left)
        && lop == inner
        && is_simple_replacement(ast, right)
    {
        if is_same_condition(cx, ll, right) && can_drop(cx, node, right, lr) {
            return Some(right);
        }
        if is_same_condition(cx, lr, right) && can_drop(cx, node, right, ll) {
            return Some(right);
        }
    }
    None
}

struct Factoring {
    common: NodeId,
    left_rest: Vec<NodeId>,
    right_rest: Vec<NodeId>,
    before: Vec<NodeId>,
    after: Vec<NodeId>,
    inner: BinaryOp,
    suggest: bool,
}

fn factoring_terms(cx: &Ctx<'_, '_>, node: NodeId) -> Option<Factoring> {
    let ast = cx.ast();
    let (left, op, right) = logical(ast, node)?;
    let inner = negated_logical(op)?;
    logical(ast, left)?;
    logical(ast, right)?;
    let mut before = logical_operands(ast, left, op);
    let mut after = logical_operands(ast, right, op);
    let left_ops = logical_operands(ast, before.pop()?, inner);
    let right_ops = logical_operands(ast, after.remove(0), inner);
    if left_ops.len() < 2
        || right_ops.len() < 2
        || !is_same_condition(cx, left_ops[0], right_ops[0])
    {
        return None;
    }
    if left_ops.len() == right_ops.len()
        && left_ops
            .iter()
            .zip(&right_ops)
            .all(|(&a, &b)| is_same_condition(cx, a, b))
    {
        return None;
    }
    let all: Vec<NodeId> = left_ops.iter().chain(&right_ops).copied().collect();
    let factorable = all
        .iter()
        .all(|&o| is_factoring_operand(ast, o, 0) && !contains_optional_chain(ast, o))
        && (is_boolean_context(cx, node) || all.iter().all(|&o| is_boolean(cx, o)));
    if !factorable {
        return None;
    }
    Some(Factoring {
        common: left_ops[0],
        left_rest: left_ops[1..].to_vec(),
        right_rest: right_ops[1..].to_vec(),
        before,
        after,
        inner,
        suggest: all.iter().any(|&o| !is_safe_factoring_operand(cx, o)),
    })
}

fn factoring_text(cx: &Ctx<'_, '_>, node: NodeId, f: &Factoring) -> String {
    let ast = cx.ast();
    let Some((_, op, _)) = logical(ast, node) else {
        return cx.text(node).to_owned();
    };
    let group = |ops: &[NodeId]| {
        let text = ops
            .iter()
            .map(|&o| paren_text(cx, o))
            .collect::<Vec<_>>()
            .join(&format!(" {} ", f.inner.as_str()));
        if ops.len() > 1 {
            format!("({text})")
        } else {
            text
        }
    };
    let mut replacement = format!(
        "{} {} ({} {} {})",
        paren_text(cx, f.common),
        f.inner.as_str(),
        group(&f.left_rest),
        op.as_str(),
        group(&f.right_rest)
    );
    if f.before.is_empty() && f.after.is_empty() {
        if !is_parenthesized(cx, f.common)
            && !replacement_needs_parens(cx, node, f.inner)
            && needs_parens_at_expression_start(cx, f.common, node)
        {
            replacement = format!("({replacement})");
        }
        return replacement_text(cx, node, f.inner, replacement);
    }
    let parts: Vec<String> = f
        .before
        .iter()
        .map(|&o| paren_text(cx, o).to_owned())
        .chain(std::iter::once(format!("({replacement})")))
        .chain(f.after.iter().map(|&o| paren_text(cx, o).to_owned()))
        .collect();
    let mut text = parts.join(&format!(" {} ", op.as_str()));
    if !f.before.is_empty()
        && !is_parenthesized(cx, f.before[0])
        && cx.src()[cx.span(node).start as usize..].starts_with('(')
    {
        text = format!("({text})");
    }
    replacement_text(cx, node, op, text)
}

fn logical_fix(cx: &Ctx<'_, '_>, node: NodeId, text: String) -> Fix {
    let mut fix = Fix::replace(cx.span(node), text);
    for (s, t) in space_around_keyword(cx, node) {
        fix = fix.and(s, t);
    }
    fix
}

fn report(cx: &mut Ctx<'_, '_>, node: NodeId, fix: Option<Fix>, suggest: bool) {
    let span = cx.span(node);
    let fix = fix.filter(|_| cx.ast().comments_in(span).is_empty());
    let r = cx
        .report(span, "Prefer a simplified condition.")
        .id("prefer-simplified-conditions");
    match fix {
        Some(fix) if suggest => {
            r.suggest("suggestion", "Simplify the condition.", fix);
        }
        Some(fix) => {
            r.fix(fix);
        }
        None => {}
    }
}

impl PreferSimplifiedConditions {
    fn should_apply_de_morgan(&self, cx: &Ctx<'_, '_>, node: NodeId, arg: NodeId) -> bool {
        if self.expand {
            return true;
        }
        let ast = cx.ast();
        let Some((left, _, right)) = logical(ast, arg) else {
            return false;
        };
        let truthiness = is_control_flow_test(cx, node);
        let original = 1 + negation_weight(ast, arg, 0);
        let replacement = negated_expression_weight(cx, left, truthiness)
            + negated_expression_weight(cx, right, truthiness);
        replacement < original
    }
}

impl Rule for PreferSimplifiedConditions {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Unary, NodeKind::Binary]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let parent_negation = parent(cx, node).is_some_and(|p| negation_arg(ast, p).is_some());
        if parent_negation {
            return;
        }
        if let Some(arg) = negation_arg(ast, node) {
            let inside_negated_logical = parent(cx, node).is_some_and(|p| {
                logical(ast, p).is_some()
                    && parent(cx, p).is_some_and(|g| negation_arg(ast, g).is_some())
            });
            let Some((left, op, right)) = logical(ast, arg) else {
                return;
            };
            if inside_negated_logical
                || negated_logical(op).is_none()
                || !self.should_apply_de_morgan(cx, node, arg)
            {
                return;
            }
            let fix = de_morgan_fix(cx, node, left, op, right);
            report(cx, node, Some(fix), false);
            return;
        }
        if logical(ast, node).is_none() {
            return;
        }
        if let Some(term) = absorption_term(cx, node) {
            let fix = (!is_reference_context(cx, node)).then(|| {
                let Some((_, op, _)) = logical(ast, node) else {
                    return logical_fix(cx, node, cx.text(node).to_owned());
                };
                let text = replacement_text(cx, node, op, paren_text(cx, term).to_owned());
                logical_fix(cx, node, text)
            });
            report(cx, node, fix, false);
            return;
        }
        if let Some(f) = factoring_terms(cx, node) {
            let fix = logical_fix(cx, node, factoring_text(cx, node, &f));
            report(cx, node, Some(fix), f.suggest);
        }
    }
}
