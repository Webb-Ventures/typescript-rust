//! `unicorn/prefer-math-constants`: a plain decimal literal (`digits.digits` or
//! `.digits`, separators ignored) that approximates a numeric `Math` constant: the
//! constant's shortest text starts with it, or (when shorter) the constant rounded to the
//! literal's fraction digits equals it. `PI` needs 2 fraction digits, `E` 3, the others 4,
//! tried in the order PI, E, LN2, LN10, LOG2E, LOG10E, SQRT2, SQRT1_2. Not for a
//! non-computed key or a literal type (`type T = 3.14`, `x as -3.14`). The literal is
//! reported with a suggestion to write the constant.

use super::prelude::*;
use super::unicorn_util::is_type_slot;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-math-constants",
        "Prefer `Math` constants over their approximate numeric values.",
        NO_PRESET,
        Category::Style,
    )
    .suggestions(),
    create: create_default::<PreferMathConstants>,
};

#[derive(Debug, Default)]
struct PreferMathConstants;

/// `(name, value, its shortest text, minimum fraction digits)`.
const CONSTANTS: [(&str, f64, &str, usize); 8] = [
    ("PI", std::f64::consts::PI, "3.141592653589793", 2),
    ("E", std::f64::consts::E, "2.718281828459045", 3),
    ("LN2", std::f64::consts::LN_2, "0.6931471805599453", 4),
    ("LN10", std::f64::consts::LN_10, "2.302585092994046", 4),
    ("LOG2E", std::f64::consts::LOG2_E, "1.4426950408889634", 4),
    ("LOG10E", std::f64::consts::LOG10_E, "0.4342944819032518", 4),
    ("SQRT2", std::f64::consts::SQRT_2, "1.4142135623730951", 4),
    (
        "SQRT1_2",
        std::f64::consts::FRAC_1_SQRT_2,
        "0.7071067811865476",
        4,
    ),
];

fn is_key(ast: &ts_r_ast::Ast, parent: NodeId, node: NodeId) -> bool {
    match *ast.node(parent) {
        Node::Property { key, .. }
        | Node::PropertyDef { key, .. }
        | Node::MethodDef { key, .. }
        | Node::PropertySignature { key, .. }
        | Node::MethodSignature { key, .. } => key == node,
        _ => false,
    }
}

impl Rule for PreferMathConstants {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::NumberLit]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        if let Some(parent) = cx.parent(node) {
            if is_key(ast, parent, node) || is_type_slot(ast, parent, node) {
                return;
            }
            if let Node::Unary {
                op: UnaryOp::Minus, ..
            } = ast.node(parent)
                && let Some(grand) = cx.parent(parent)
                && is_type_slot(ast, grand, parent)
            {
                return;
            }
        }
        let raw = cx.text(node);
        let text: String = raw.chars().filter(|&c| c != '_').collect();
        let Some((int, frac)) = text.split_once('.') else {
            return;
        };
        if !int.bytes().all(|b| b.is_ascii_digit())
            || frac.is_empty()
            || !frac.bytes().all(|b| b.is_ascii_digit())
        {
            return;
        }
        let normalized = format!("{}.{frac}", if int.is_empty() { "0" } else { int });
        for (name, value, exact, min_digits) in CONSTANTS {
            if frac.len() < min_digits {
                continue;
            }
            let rounded = normalized.len() < exact.len()
                && frac.len() <= 100
                && format!("{value:.prec$}", prec = frac.len()) == normalized;
            if !exact.starts_with(&normalized) && !rounded {
                continue;
            }
            let constant = format!("Math.{name}");
            let span = cx.span(node);
            cx.report(
                span,
                format!("Prefer `{constant}` over the approximate value `{raw}`."),
            )
            .id("prefer-math-constants/error")
            .suggest(
                "prefer-math-constants/suggestion",
                format!("Replace `{raw}` with `{constant}`."),
                Fix::replace(span, constant),
            );
            return;
        }
    }
}
