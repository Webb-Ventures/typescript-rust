//! `unicorn/no-xor-as-exponentiation`: `^` between two decimal integer literals (`2 ^ 8`)
//! was probably meant as `**`. The operator is reported, with a suggestion to replace it.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-xor-as-exponentiation",
        "Disallow the bitwise XOR operator where exponentiation was likely intended.",
        NO_PRESET,
        Category::Suspicious,
    )
    .suggestions(),
    create: create_default::<NoXorAsExponentiation>,
};

#[derive(Debug, Default)]
struct NoXorAsExponentiation;

impl Rule for NoXorAsExponentiation {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Binary]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Binary {
            left,
            op: BinaryOp::BitXor,
            right,
        } = *cx.node(node)
        else {
            return;
        };
        if !is_decimal_integer(cx, left) || !is_decimal_integer(cx, right) {
            return;
        }
        let Some(at) = find_char(cx.src(), ast.span(left).end, b'^') else {
            return;
        };
        let op = Span::new(at, at + 1);
        cx.report(
            op,
            "Unexpected bitwise XOR operator `^`. Did you mean the exponentiation operator `**`?",
        )
        .id("no-xor-as-exponentiation/error")
        .suggest(
            "no-xor-as-exponentiation/suggestion",
            "Replace `^` with `**`.",
            Fix::replace(op, "**"),
        );
    }
}

/// A number literal written as a decimal integer (`0`, `10`, `1_000`, or a legacy `09`).
fn is_decimal_integer(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let id = skip_parens(cx.ast(), id);
    if !matches!(cx.node(id), Node::NumberLit { .. }) {
        return false;
    }
    let raw = cx.span(id).text(cx.src());
    let b = raw.as_bytes();
    match b {
        [b'0'] => true,
        [b'0', rest @ ..] => {
            // A legacy literal with an 8 or 9 is decimal.
            rest.iter().all(u8::is_ascii_digit) && rest.iter().any(|&c| c == b'8' || c == b'9')
        }
        [b'1'..=b'9', ..] => {
            !raw.ends_with('_')
                && !raw.contains("__")
                && b.iter().all(|&c| c.is_ascii_digit() || c == b'_')
        }
        _ => false,
    }
}
