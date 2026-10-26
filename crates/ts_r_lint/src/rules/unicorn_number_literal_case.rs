//! `unicorn/number-literal-case`: number and bigint literals are written in lowercase
//! (`0x`, `0b`, `0o`, `e`, `n`), with uppercase hexadecimal digits by default
//! (`hexadecimalValue: "lowercase"` for lowercase ones). The whole literal is reported and
//! replaced.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/number-literal-case",
        "Enforce proper case for numeric literals.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<NumberLiteralCase>,
};

#[derive(Debug, Default)]
struct NumberLiteralCase {
    lowercase_hex: bool,
}

impl FromOptions for NumberLiteralCase {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(NumberLiteralCase {
            lowercase_hex: str_field(obj_opt(opts, 0), "hexadecimalValue") == Some("lowercase"),
        })
    }
}

impl Rule for NumberLiteralCase {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::NumberLit, NodeKind::BigIntLit]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let span = cx.span(node);
        let raw = span.text(cx.src());
        let fixed = match *cx.node(node) {
            Node::NumberLit { .. } => self.fix(raw),
            _ => match raw.strip_suffix('n') {
                Some(digits) => format!("{}n", self.fix(digits)),
                None => return,
            },
        };
        if fixed != raw {
            cx.report(span, "Invalid number literal casing.")
                .id("number-literal-case")
                .fix(Fix::replace(span, fixed));
        }
    }
}

impl NumberLiteralCase {
    fn fix(&self, raw: &str) -> String {
        let mut fixed = raw.to_ascii_lowercase();
        if let Some(at) = fixed.find("0x") {
            let digits = fixed[at + 2..]
                .bytes()
                .take_while(|c| c.is_ascii_hexdigit() || *c == b'_')
                .count();
            if !self.lowercase_hex {
                let range = at + 2..at + 2 + digits;
                let upper = fixed[range.clone()].to_ascii_uppercase();
                fixed.replace_range(range, &upper);
            }
        }
        fixed
    }
}
