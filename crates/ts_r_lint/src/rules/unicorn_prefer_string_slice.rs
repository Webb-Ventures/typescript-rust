//! `unicorn/prefer-string-slice`: `x.substr(...)` and `x.substring(...)` (non-computed
//! member) are reported on the call, unless the receiver is known not to be a string or a
//! `substring` reads one character (`s.substring(i, i + 1)`, `substring(2, 3)`, ...).
//!
//! The fix renames the method to `slice` and rewrites the arguments only where the result
//! is the same, otherwise it is withheld (more than two arguments, spreads, or arguments
//! whose meaning differs):
//!
//! - `substr(0, n)`: `x.length - k` on the same receiver becomes `-k`; a number literal or a
//!   `.length` end stays; a (negated) literal end is clamped at `0`; any other end without
//!   side effects becomes `Math.max(0, end === undefined ? Infinity : end)`.
//!   `substr(a, b)` with two literals becomes `slice(a, a + b)`.
//! - `substring(a)`: literals are clamped at `0`, `.length` stays, anything else is wrapped
//!   in `Math.max(0, ...)`. `substring(a, b)` with two literals clamps and orders them; with
//!   a `0` on either side the other becomes the clamped end.
//!
//! An argument is not replaced when its parentheses contain a comment.

use super::prelude::*;
use super::unicorn_static::{Static, static_value_no_side_effects};
use super::unicorn_text::{
    has_comment_in, has_possible_side_effect, is_parenthesized, is_same_reference, paren_span,
    paren_text, static_string_value,
};
use super::unicorn_types::is_known_non_string;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-string-slice",
        "Prefer `String#slice()` over `String#substr()` and `String#substring()`.",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<PreferStringSlice>,
};

#[derive(Debug, Default)]
struct PreferStringSlice;

impl Rule for PreferStringSlice {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Call { callee, args, .. } = *cx.node(node) else {
            return;
        };
        let Node::Member { obj, prop, .. } = *ast.node(skip_parens(ast, callee)) else {
            return;
        };
        let method: &'static str = match ident_name(ast, prop) {
            Some("substr") => "substr",
            Some("substring") => "substring",
            _ => return,
        };
        if is_known_non_string(cx, obj) {
            return;
        }
        let args = ast.list(args);
        if method == "substring" && is_single_character(cx, args) {
            return;
        }
        let fix = build_fix(cx, method, skip_parens(ast, obj), prop, args);
        let msg = format!("Prefer `String#slice()` over `String#{method}()`.");
        cx.report_node(node, msg).id(method).fix_opt(fix);
    }
}

fn number_lit(ast: &ts_r_ast::Ast, id: NodeId) -> Option<f64> {
    match *ast.node(skip_parens(ast, id)) {
        Node::NumberLit { value } => Some(value.get()),
        _ => None,
    }
}

/// A number literal, or a negation of one.
fn numeric_value(ast: &ts_r_ast::Ast, id: NodeId) -> Option<f64> {
    match *ast.node(skip_parens(ast, id)) {
        Node::NumberLit { value } => Some(value.get()),
        Node::Unary {
            op: UnaryOp::Minus,
            arg,
        } => numeric_value(ast, arg).map(|v| -v),
        _ => None,
    }
}

fn is_safe_integer(v: f64) -> bool {
    v.fract() == 0.0 && v.abs() <= 9_007_199_254_740_991.0
}

/// `x.length` (non-computed).
fn is_length_property(ast: &ts_r_ast::Ast, id: NodeId) -> bool {
    matches!(*ast.node(skip_parens(ast, id)), Node::Member { prop, .. }
        if ident_name(ast, prop) == Some("length"))
}

fn is_one(ast: &ts_r_ast::Ast, id: NodeId) -> bool {
    number_lit(ast, id) == Some(1.0)
}

/// `substring(i, i + 1)`, `substring(i + 1, i)`, `substring(i - 1, i)` or two adjacent
/// non-negative integer literals.
fn is_single_character(cx: &Ctx<'_, '_>, args: &[NodeId]) -> bool {
    let ast = cx.ast();
    let &[a, b] = args else {
        return false;
    };
    let (a, b) = (skip_parens(ast, a), skip_parens(ast, b));
    let non_negative = |v: Option<f64>| v.filter(|v| is_safe_integer(*v) && *v >= 0.0);
    if let (Some(x), Some(y)) = (
        non_negative(number_lit(ast, a)),
        non_negative(number_lit(ast, b)),
    ) && (x - y).abs() == 1.0
    {
        return true;
    }
    let plus_one = |sum: NodeId, index: NodeId| {
        matches!(*ast.node(sum), Node::Binary { left, op: BinaryOp::Add, right }
            if (is_same_reference(cx, left, index) && is_one(ast, right))
                || (is_one(ast, left) && is_same_reference(cx, right, index)))
    };
    if plus_one(b, a) || plus_one(a, b) {
        return true;
    }
    matches!(*ast.node(a), Node::Binary { left, op: BinaryOp::Sub, right }
        if is_one(ast, right) && is_same_reference(cx, left, b))
}

/// `end === undefined ? Infinity : end`, or `None` when `end` may have side effects.
fn coalesced_end_text(cx: &Ctx<'_, '_>, end: NodeId) -> Option<String> {
    let ast = cx.ast();
    let inner = skip_parens(ast, end);
    if has_possible_side_effect(cx, inner) {
        return None;
    }
    let mut text = paren_text(cx, inner).to_owned();
    let logical = matches!(
        ast.node(inner),
        Node::Binary {
            op: BinaryOp::LogicalOr | BinaryOp::LogicalAnd | BinaryOp::Coalesce,
            ..
        }
    );
    if !is_parenthesized(cx, inner) && (logical || precedence(ast, inner) < 12) {
        text = format!("({text})");
    }
    Some(format!("{text} === undefined ? Infinity : {text}"))
}

/// Builds the fix's edits; `None` aborts the whole fix.
struct Edits<'c, 'f, 'a> {
    cx: &'c Ctx<'f, 'a>,
    fix: Fix,
}

impl Edits<'_, '_, '_> {
    /// Replaces an argument with its parentheses (aborting on a comment inside them).
    fn replace_arg(&mut self, arg: NodeId, text: String) -> Option<()> {
        let span = paren_span(self.cx, arg);
        if has_comment_in(self.cx, span) {
            return None;
        }
        self.fix = std::mem::replace(&mut self.fix, Fix { edits: Vec::new() }).and(span, text);
        Some(())
    }
    fn replace_span(&mut self, span: Span, text: String) {
        self.fix = std::mem::replace(&mut self.fix, Fix { edits: Vec::new() }).and(span, text);
    }
}

fn build_fix(
    cx: &Ctx<'_, '_>,
    method: &str,
    receiver: NodeId,
    prop: NodeId,
    args: &[NodeId],
) -> Option<Fix> {
    let ast = cx.ast();
    let mut e = Edits {
        cx,
        fix: Fix::replace(cx.span(prop), "slice"),
    };
    if args.is_empty() {
        return Some(e.fix);
    }
    if args.len() > 2
        || args
            .iter()
            .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
    {
        return None;
    }
    if method == "substr" {
        substr_args(&mut e, receiver, args)?;
    } else {
        substring_args(&mut e, args)?;
    }
    Some(e.fix)
}

fn substr_args(e: &mut Edits<'_, '_, '_>, receiver: NodeId, args: &[NodeId]) -> Option<()> {
    let cx = e.cx;
    let ast = cx.ast();
    let (first, second) = match *args {
        [a, b] => (a, b),
        _ => return Some(()),
    };
    if matches!(static_value_no_side_effects(cx, first), Some(Static::Num(n)) if n == 0.0) {
        if let Some(neg) = negative_index(cx, second, receiver) {
            return e.replace_arg(second, neg);
        }
        if number_lit(ast, second).is_some() || is_length_property(ast, second) {
            return Some(());
        }
        if let Some(v) = numeric_value(ast, second) {
            return e.replace_arg(second, number_to_string(v.max(0.0) + 0.0));
        }
        let end = coalesced_end_text(cx, second)?;
        e.replace_span(paren_span(cx, second), format!("Math.max(0, {end})"));
        return Some(());
    }
    if let (Some(a), Some(b)) = (number_lit(ast, first), number_lit(ast, second)) {
        return e.replace_arg(second, number_to_string(a.trunc() + b.trunc()));
    }
    None
}

/// `x.length - k` on the receiver `x` (an identifier or a static string): `-k`.
fn negative_index(cx: &Ctx<'_, '_>, arg: NodeId, receiver: NodeId) -> Option<String> {
    let ast = cx.ast();
    let Node::Binary {
        left,
        op: BinaryOp::Sub,
        right,
    } = *ast.node(skip_parens(ast, arg))
    else {
        return None;
    };
    let right = skip_parens(ast, right);
    let k = number_lit(ast, right)?;
    if !(is_safe_integer(k) && k > 0.0 && is_length_property(ast, left)) {
        return None;
    }
    let Node::Member { obj, .. } = *ast.node(skip_parens(ast, left)) else {
        return None;
    };
    let safe_receiver = matches!(ast.node(receiver), Node::Ident { .. })
        || static_string_value(ast, receiver).is_some();
    if !safe_receiver || !is_same_reference(cx, obj, receiver) {
        return None;
    }
    let span = paren_span(cx, arg);
    let start = skip_trivia_backward(ast, cx.src(), span.start as usize) as u32;
    if has_comment_in(cx, Span::new(start, span.end)) {
        return None;
    }
    Some(format!("-{}", cx.text(right)))
}

fn substring_args(e: &mut Edits<'_, '_, '_>, args: &[NodeId]) -> Option<()> {
    let cx = e.cx;
    let ast = cx.ast();
    let first = args[0];
    let first_number = numeric_value(ast, first);
    let Some(&second) = args.get(1) else {
        if is_length_property(ast, first) {
            return Some(());
        }
        if let Some(v) = first_number {
            return e.replace_arg(first, number_to_string(v.max(0.0) + 0.0));
        }
        let span = paren_span(cx, first);
        e.replace_span(Span::at(span.start), "Math.max(0, ".to_owned());
        e.replace_span(Span::at(span.end), ")".to_owned());
        return Some(());
    };
    let second_number = numeric_value(ast, second);
    if let (Some(a), Some(b)) = (first_number, second_number) {
        let mut values = [a.max(0.0) + 0.0, b.max(0.0) + 0.0];
        if a > b {
            values.reverse();
        }
        if values[0] != a {
            e.replace_arg(first, number_to_string(values[0]))?;
        }
        if values[1] != b {
            e.replace_arg(second, number_to_string(values[1]))?;
        }
        return Some(());
    }
    if first_number == Some(0.0) || second_number == Some(0.0) {
        let end = if second_number == Some(0.0) {
            paren_text(cx, first).to_owned()
        } else {
            coalesced_end_text(cx, second)?
        };
        e.replace_arg(first, "0".to_owned())?;
        e.replace_arg(second, format!("Math.max(0, {end})"))?;
        return Some(());
    }
    None
}
