//! `unicorn/prefer-modern-math-apis`: `Math.log10(x)` / `Math.log2(x)` over
//! `Math.log(x) * Math.LOG10E`, `Math.LOG2E * Math.log(x)`, `Math.log(x) / Math.LN10` and
//! `Math.log(x) / Math.LN2`; `Math.hypot(a, b)` / `Math.abs(a)` over `Math.sqrt(a * a + b ** 2)`.
//!
//! `Math` is the identifier as written (not scope-checked); calls need exactly one
//! argument, no optional chaining; constants are non-computed members. A square is `x * x`
//! (same reference on both sides) or `x ** 2` (the literal `2`); a sum is any tree of `+`.
//!
//! The replacement is an autofix only when it can be shown not to change the result,
//! otherwise a suggestion:
//! - the value is unused (operand of `void`/`typeof`/`delete`, a `for` init or update, a
//!   non-last sequence element, reached through the non-test arm of `?:` or the right side
//!   of a logical, the last element of a sequence, and TS wrappers);
//! - logarithms: the argument's static number is `<= 0`, `1`, or not finite;
//! - squares: every squared operand has a static number, and either the value is unused,
//!   or (several terms) with an infinity none is `NaN`, else all are `0` or one is `NaN`,
//!   or (one term) `x ** 2` with `x` zero or not finite, `x * x` when `sqrt(x * x)` is
//!   exactly `abs(x)`.
//!
//! A static number is read through TS wrappers and one `const` binding: `Infinity`, `NaN`,
//! `undefined` (by name), `Number.<constant>`, `Math.<constant>` (optionally signed), or a
//! primitive literal (strings and booleans converted as JavaScript does; bigints have none).
//! No fix when a comment would be lost (log: outside the argument; squares: between a
//! square's base and its end).

use super::prelude::*;
use super::unicorn_no_array_reduce::const_initializer;
use super::unicorn_static::string_to_number;
use super::unicorn_text::{has_comment_in, is_same_reference, would_remove_comments};
use super::unicorn_util::skip_wrappers;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-modern-math-apis",
        "Prefer modern `Math` APIs over legacy patterns.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create_default::<PreferModernMathApis>,
};

#[derive(Debug, Default)]
struct PreferModernMathApis;

impl Rule for PreferModernMathApis {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call, NodeKind::Binary]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        match *cx.node(node) {
            Node::Binary { left, op, right } => check_log(cx, node, left, op, right),
            Node::Call { .. } => check_sqrt(cx, node),
            _ => {}
        }
    }
}

/// `Math.<method>(x)` with one non-spread argument and no optional chaining: the argument.
fn math_call(ast: &Ast, id: NodeId, method: &str) -> Option<NodeId> {
    let Node::Call {
        callee,
        optional: false,
        args,
        ..
    } = *ast.node(skip_parens(ast, id))
    else {
        return None;
    };
    let Node::Member {
        obj,
        optional: false,
        prop,
    } = *ast.node(skip_parens(ast, callee))
    else {
        return None;
    };
    if ident_name(ast, skip_parens(ast, obj)) != Some("Math")
        || ident_name(ast, prop) != Some(method)
    {
        return None;
    }
    match ast.list(args) {
        &[arg] if !matches!(ast.node(arg), Node::Spread { .. }) => Some(arg),
        _ => None,
    }
}

/// `<object>.<name>` (non-computed, not optional): the property name.
fn member_of<'a>(ast: &'a Ast, id: NodeId, object: &str) -> Option<&'a str> {
    let Node::Member {
        obj,
        optional: false,
        prop,
    } = *ast.node(skip_parens(ast, id))
    else {
        return None;
    };
    if ident_name(ast, skip_parens(ast, obj)) != Some(object) {
        return None;
    }
    ident_name(ast, prop)
}

fn report(
    cx: &mut Ctx<'_, '_>,
    span: Span,
    replacement: &str,
    description: &str,
    fix: Option<Fix>,
    auto: bool,
) {
    let message = format!("Prefer `{replacement}` over `{description}`.");
    let r = cx.report(span, message).id("prefer-modern-math-apis");
    match fix {
        Some(f) if auto => {
            r.fix(f);
        }
        Some(f) => {
            r.suggest("suggestion", format!("Replace with `{replacement}`."), f);
        }
        None => {}
    }
}

fn check_log(cx: &mut Ctx<'_, '_>, node: NodeId, left: NodeId, op: BinaryOp, right: NodeId) {
    let ast = cx.ast();
    let (value, method, description) = match op {
        BinaryOp::Mul => {
            let pick = |log: NodeId, constant: NodeId| {
                let value = math_call(ast, log, "log")?;
                let c = member_of(ast, constant, "Math")?;
                let method = match c {
                    "LOG10E" => "log10",
                    "LOG2E" => "log2",
                    _ => return None,
                };
                Some((value, method, c))
            };
            if let Some((v, m, c)) = pick(left, right) {
                (v, m, format!("Math.log(…) * Math.{c}"))
            } else if let Some((v, m, c)) = pick(right, left) {
                (v, m, format!("Math.{c} * Math.log(…)"))
            } else {
                return;
            }
        }
        BinaryOp::Div => {
            let Some(value) = math_call(ast, left, "log") else {
                return;
            };
            let (method, c) = match member_of(ast, right, "Math") {
                Some("LN10") => ("log10", "LN10"),
                Some("LN2") => ("log2", "LN2"),
                _ => return,
            };
            (value, method, format!("Math.log(…) / Math.{c}"))
        }
        _ => return,
    };
    let span = cx.span(node);
    let value_span = cx.span(value);
    let fix = (!would_remove_comments(cx, span, &[value_span])).then(|| {
        Fix::replace(
            span,
            format!("Math.{method}({})", value_span.text(cx.src())),
        )
    });
    let auto = value_unused(cx, node)
        || static_number(cx, value).is_some_and(|v| v <= 0.0 || v == 1.0 || !v.is_finite());
    report(
        cx,
        span,
        &format!("Math.{method}(…)"),
        &description,
        fix,
        auto,
    );
}

/// The terms of a `+` tree (through parentheses) and the `+` nodes themselves.
fn flatten_plus(
    ast: &Ast,
    id: NodeId,
    terms: &mut Vec<NodeId>,
    pluses: &mut Vec<NodeId>,
    depth: u32,
) -> bool {
    if depth > 200 {
        return false;
    }
    let id = skip_parens(ast, id);
    match *ast.node(id) {
        Node::Binary {
            left,
            op: BinaryOp::Add,
            right,
        } => {
            pluses.push(id);
            flatten_plus(ast, left, terms, pluses, depth + 1)
                && flatten_plus(ast, right, terms, pluses, depth + 1)
        }
        _ => {
            terms.push(id);
            true
        }
    }
}

/// `x * x` or `x ** 2`: `x` and whether it is `**`.
fn square(cx: &Ctx<'_, '_>, id: NodeId) -> Option<(NodeId, bool)> {
    let ast = cx.ast();
    match *ast.node(id) {
        Node::Binary {
            left,
            op: BinaryOp::Mul,
            right,
        } if is_same_reference(cx, left, right) => Some((left, false)),
        Node::Binary {
            left,
            op: BinaryOp::Exp,
            right,
        } => match *ast.node(skip_parens(ast, right)) {
            Node::NumberLit { value } if value.get() == 2.0 => Some((left, true)),
            _ => None,
        },
        _ => None,
    }
}

fn check_sqrt(cx: &mut Ctx<'_, '_>, call: NodeId) {
    let ast = cx.ast();
    let Some(arg) = math_call(ast, call, "sqrt") else {
        return;
    };
    let (mut terms, mut pluses) = (Vec::new(), Vec::new());
    if !flatten_plus(ast, arg, &mut terms, &mut pluses, 0) {
        return;
    }
    let Some(squares) = terms
        .iter()
        .map(|&t| square(cx, t))
        .collect::<Option<Vec<_>>>()
    else {
        return;
    };
    let method = if squares.len() == 1 { "abs" } else { "hypot" };
    let auto = can_fix_squares(cx, call, &squares);
    let Node::Call { callee, .. } = *ast.node(call) else {
        return;
    };
    let Node::Member { prop, .. } = *ast.node(skip_parens(ast, callee)) else {
        return;
    };
    let fix = sqrt_fix(cx, prop, method, &terms, &squares, &pluses);
    let span = cx.span(prop);
    report(
        cx,
        span,
        &format!("Math.{method}(…)"),
        "Math.sqrt(…)",
        fix,
        auto,
    );
}

fn sqrt_fix(
    cx: &Ctx<'_, '_>,
    prop: NodeId,
    method: &str,
    terms: &[NodeId],
    squares: &[(NodeId, bool)],
    pluses: &[NodeId],
) -> Option<Fix> {
    let ast = cx.ast();
    let src = cx.src();
    let mut fix = Fix::replace(cx.span(prop), method);
    for &plus in pluses {
        let Node::Binary { left, .. } = *ast.node(plus) else {
            continue;
        };
        let mut pos = skip_parens_span_end(cx, left) as usize;
        loop {
            pos = skip_trivia_forward(src, pos);
            if src.as_bytes().get(pos) == Some(&b')') {
                pos += 1;
            } else {
                break;
            }
        }
        if src.as_bytes().get(pos) != Some(&b'+') {
            return None;
        }
        let before = &src[..pos];
        let ws = &before[before.trim_end().len()..];
        let lb = if ws.starts_with("\r\n") {
            "\r\n"
        } else if ws.starts_with(['\n', '\r']) {
            &ws[..1]
        } else {
            ""
        };
        fix = fix.and(
            Span::new((pos - ws.len()) as u32, pos as u32 + 1),
            format!("{lb},"),
        );
        let mut cur = plus;
        while let Some(p) = cx.parent(cur) {
            if !matches!(cx.node(p), Node::Paren { .. }) {
                break;
            }
            let s = cx.span(p);
            fix = fix
                .and(Span::new(s.start, s.start + 1), "")
                .and(Span::new(s.end - 1, s.end), "");
            cur = p;
        }
    }
    for (&term, &(base, _)) in terms.iter().zip(squares) {
        let start = cx.span(base).end;
        let start = paren_end(cx, base).max(start);
        let range = Span::new(start, cx.span(term).end);
        if has_comment_in(cx, range) {
            return None;
        }
        fix = fix.and(range, "");
    }
    Some(fix)
}

/// The end of `id` with all its parentheses.
fn paren_end(cx: &Ctx<'_, '_>, id: NodeId) -> u32 {
    super::unicorn_util::parenthesized_span(cx, id).end
}

/// The end of the expression inside `id`'s parentheses (ESTree's range of a child).
fn skip_parens_span_end(cx: &Ctx<'_, '_>, id: NodeId) -> u32 {
    cx.span(skip_parens(cx.ast(), id)).end
}

fn can_fix_squares(cx: &Ctx<'_, '_>, call: NodeId, squares: &[(NodeId, bool)]) -> bool {
    let Some(values) = squares
        .iter()
        .map(|&(b, _)| static_number(cx, b))
        .collect::<Option<Vec<f64>>>()
    else {
        return false;
    };
    if value_unused(cx, call) {
        return true;
    }
    if values.len() > 1 {
        if values.iter().any(|v| v.is_infinite()) {
            return values.iter().all(|v| !v.is_nan());
        }
        return values.iter().all(|&v| v == 0.0) || values.iter().any(|v| v.is_nan());
    }
    let v = values[0];
    if squares[0].1 {
        return v == 0.0 || !v.is_finite();
    }
    let a = (v * v).sqrt();
    let b = v.abs();
    (a.is_nan() && b.is_nan()) || (a == b && a.is_sign_negative() == b.is_sign_negative())
}

/// The value of `node` is thrown away (see the module docs).
fn value_unused(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let mut expr = node;
    for _ in 0..10_000 {
        while let Some(p) = cx.parent(expr) {
            if matches!(
                cx.node(p),
                Node::Paren { .. }
                    | Node::As { .. }
                    | Node::Satisfies { .. }
                    | Node::NonNull { .. }
                    | Node::TypeAssertion { .. }
            ) {
                expr = p;
            } else {
                break;
            }
        }
        let Some(parent) = cx.parent(expr) else {
            return false;
        };
        match *cx.node(parent) {
            Node::Unary { op, .. } => {
                return matches!(op, UnaryOp::Void | UnaryOp::TypeOf | UnaryOp::Delete);
            }
            Node::For { init, update, .. } => {
                return init == Some(expr) || update == Some(expr);
            }
            Node::Seq { exprs } => {
                if cx.ast().list(exprs).last() != Some(&expr) {
                    return true;
                }
            }
            Node::Conditional { test, .. } => {
                if test == expr {
                    return false;
                }
            }
            Node::Binary {
                left,
                op: BinaryOp::LogicalAnd | BinaryOp::LogicalOr | BinaryOp::Coalesce,
                ..
            } => {
                if left == expr {
                    return false;
                }
            }
            _ => return false,
        }
        expr = parent;
    }
    false
}

fn number_constant(ast: &Ast, id: NodeId) -> Option<f64> {
    match *ast.node(id) {
        Node::Ident { .. } => match ident_name(ast, id)? {
            "Infinity" => Some(f64::INFINITY),
            "NaN" | "undefined" => Some(f64::NAN),
            _ => None,
        },
        Node::Member { obj, .. } if ident_name(ast, skip_parens(ast, obj)) == Some("Number") => {
            Some(match member_of(ast, id, "Number")? {
                "EPSILON" => f64::EPSILON,
                "MAX_SAFE_INTEGER" => 9_007_199_254_740_991.0,
                "MAX_VALUE" => f64::MAX,
                "MIN_SAFE_INTEGER" => -9_007_199_254_740_991.0,
                "MIN_VALUE" => f64::from_bits(1),
                "NEGATIVE_INFINITY" => f64::NEG_INFINITY,
                "NaN" => f64::NAN,
                "POSITIVE_INFINITY" => f64::INFINITY,
                _ => return None,
            })
        }
        Node::Member { .. } => {
            use std::f64::consts;
            Some(match member_of(ast, id, "Math")? {
                "E" => consts::E,
                "LN10" => consts::LN_10,
                "LN2" => consts::LN_2,
                "LOG10E" => consts::LOG10_E,
                "LOG2E" => consts::LOG2_E,
                "PI" => consts::PI,
                "SQRT1_2" => consts::FRAC_1_SQRT_2,
                "SQRT2" => consts::SQRT_2,
                _ => return None,
            })
        }
        _ => None,
    }
}

/// The number a static operand stands for (see the module docs).
fn static_number(cx: &Ctx<'_, '_>, id: NodeId) -> Option<f64> {
    let ast = cx.ast();
    let mut node = skip_wrappers(ast, id);
    if let Some(init) = const_initializer(cx, node) {
        node = skip_wrappers(ast, init);
    }
    let (arg, negate) = match *ast.node(node) {
        Node::Unary {
            op: UnaryOp::Plus,
            arg,
        } => (skip_wrappers(ast, arg), false),
        Node::Unary {
            op: UnaryOp::Minus,
            arg,
        } => (skip_wrappers(ast, arg), true),
        _ => (node, false),
    };
    if let Some(v) = number_constant(ast, arg) {
        return Some(if negate { -v } else { v });
    }
    // A primitive literal (signed numbers included).
    match *ast.node(node) {
        Node::NumberLit { value } => Some(value.get()),
        Node::StringLit { value } => Some(string_to_number(ast.atom(value))),
        Node::BoolLit { value } => Some(f64::from(u8::from(value))),
        Node::NullLit {} => Some(0.0),
        Node::TemplateLit { parts } => match ast.list(parts) {
            &[q] => match *ast.node(q) {
                Node::TemplateElement {
                    cooked,
                    invalid_escape: false,
                } => Some(string_to_number(ast.atom(cooked))),
                _ => None,
            },
            _ => None,
        },
        Node::Unary {
            op: op @ (UnaryOp::Plus | UnaryOp::Minus),
            ..
        } => match *ast.node(arg) {
            Node::NumberLit { value } => {
                let v = value.get();
                Some(if op == UnaryOp::Minus { -v } else { v })
            }
            _ => None,
        },
        _ => None,
    }
}
