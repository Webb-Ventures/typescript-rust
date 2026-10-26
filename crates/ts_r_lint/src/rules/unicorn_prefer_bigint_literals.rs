//! `unicorn/prefer-bigint-literals`: `BigInt(x)` with one static argument becomes a bigint
//! literal. A string argument is read as `BigInt` reads strings (trimmed, an optional sign
//! for decimal digits, or a `0b`/`0o`/`0x` prefix; empty is `0`); its text is kept when it is
//! already a canonical literal (decimal without leading zeros, or prefixed digits that print
//! back the same), else the decimal value is written. A number argument (under any `-`/`+`
//! signs) must be an integer; its raw text is kept when it is canonical, else the exact
//! decimal value is written as a suggestion. A `+` anywhere, or a comment inside the call,
//! makes it a suggestion instead of a fix. Negative results are parenthesized (with a `;`
//! when the statement would continue the previous one).

use super::prelude::*;
use super::unicorn_ast::is_js_space;
use super::unicorn_text::{has_comment_in, is_parenthesized, needs_semicolon, static_string_value};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-bigint-literals",
        "Prefer `BigInt` literals over the constructor.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create_default::<PreferBigintLiterals>,
};

#[derive(Debug, Default)]
struct PreferBigintLiterals;

/// An arbitrary-size unsigned integer, little-endian base-2^32 limbs.
#[derive(Clone, Debug, PartialEq)]
struct Big(Vec<u32>);

impl Big {
    fn zero() -> Big {
        Big(Vec::new())
    }
    fn is_zero(&self) -> bool {
        self.0.iter().all(|&l| l == 0)
    }
    fn mul_add(&mut self, m: u32, a: u32) {
        let mut carry = a as u64;
        for l in &mut self.0 {
            let v = *l as u64 * m as u64 + carry;
            *l = v as u32;
            carry = v >> 32;
        }
        if carry > 0 {
            self.0.push(carry as u32);
        }
    }
    /// Divides in place, returning the remainder.
    fn div_rem(&mut self, d: u32) -> u32 {
        let mut rem = 0u64;
        for l in self.0.iter_mut().rev() {
            let v = (rem << 32) | *l as u64;
            *l = (v / d as u64) as u32;
            rem = v % d as u64;
        }
        while self.0.last() == Some(&0) {
            self.0.pop();
        }
        rem as u32
    }
    fn parse(digits: &str, base: u32) -> Option<Big> {
        if digits.is_empty() {
            return None;
        }
        let mut b = Big::zero();
        for c in digits.chars() {
            b.mul_add(base, c.to_digit(base)?);
        }
        Some(b)
    }
    fn to_string_radix(&self, base: u32) -> String {
        if self.is_zero() {
            return "0".to_owned();
        }
        let mut n = self.clone();
        let mut out = Vec::new();
        while !n.is_zero() {
            let r = n.div_rem(base);
            out.push(std::char::from_digit(r, base).unwrap_or('0'));
        }
        out.iter().rev().collect()
    }
    /// The exact value of a non-negative integral double.
    fn from_f64(v: f64) -> Big {
        let bits = v.to_bits();
        let exp = ((bits >> 52) & 0x7ff) as i64;
        let frac = bits & ((1u64 << 52) - 1);
        if exp == 0 {
            return Big::zero();
        }
        let mant = frac | (1u64 << 52);
        let shift = exp - 1075;
        let mut b = Big(vec![mant as u32, (mant >> 32) as u32]);
        if shift < 0 {
            let s = (-shift) as u32;
            let m = if s >= 64 { 0 } else { mant >> s };
            return Big(vec![m as u32, (m >> 32) as u32]);
        }
        for _ in 0..shift {
            b.mul_add(2, 0);
        }
        b
    }
}

/// A signed big integer.
struct BigInt {
    neg: bool,
    mag: Big,
}

/// `BigInt(string)`, or `None` where it throws.
fn string_to_bigint(s: &str) -> Option<BigInt> {
    let t = s.trim_matches(is_js_space);
    if t.is_empty() {
        return Some(BigInt {
            neg: false,
            mag: Big::zero(),
        });
    }
    let lower = t.to_ascii_lowercase();
    for (p, base) in [("0b", 2), ("0o", 8), ("0x", 16)] {
        if let Some(d) = lower.strip_prefix(p) {
            return Some(BigInt {
                neg: false,
                mag: Big::parse(d, base)?,
            });
        }
    }
    let (neg, digits) = match t.as_bytes()[0] {
        b'-' => (true, &t[1..]),
        b'+' => (false, &t[1..]),
        _ => (false, t),
    };
    if !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mag = Big::parse(digits, 10)?;
    Some(BigInt {
        neg: neg && !mag.is_zero(),
        mag,
    })
}

/// `0b1`, `0o7`, `0xf` whose digits print back exactly.
fn prefixed_canonical(raw: &str, v: &Big) -> bool {
    for (p, base) in [("0b", 2), ("0o", 8), ("0x", 16)] {
        if let Some(d) = raw.strip_prefix(p) {
            return d == v.to_string_radix(base);
        }
    }
    false
}

fn is_literal_text(t: &str) -> bool {
    t == "0"
        || (t.starts_with(|c: char| ('1'..='9').contains(&c))
            && t.bytes().all(|b| b.is_ascii_digit()))
}

/// (needs a suggestion, literal text, negative).
fn replacement(cx: &Ctx<'_, '_>, mut arg: NodeId) -> Option<(bool, String, bool)> {
    let ast = cx.ast();
    arg = skip_parens(ast, arg);
    if let Some(s) = static_string_value(ast, arg) {
        let v = string_to_bigint(&s)?;
        let mut text = s.trim_matches(is_js_space);
        if let Some(r) = text.strip_prefix('+') {
            text = r.trim_matches(is_js_space);
        }
        let text = if is_literal_text(text) || prefixed_canonical(&text.to_lowercase(), &v.mag) {
            text.to_owned()
        } else {
            format!(
                "{}{}",
                if v.neg { "-" } else { "" },
                v.mag.to_string_radix(10)
            )
        };
        return Some((s.contains('+'), format!("{text}n"), v.neg));
    }
    let mut suggest = false;
    let mut neg = false;
    loop {
        match *ast.node(arg) {
            Node::Unary {
                op: UnaryOp::Plus,
                arg: a,
            } => {
                suggest = true;
                arg = skip_parens(ast, a);
            }
            Node::Unary {
                op: UnaryOp::Minus,
                arg: a,
            } => {
                neg = !neg;
                arg = skip_parens(ast, a);
            }
            _ => break,
        }
    }
    let Node::NumberLit { value } = *ast.node(arg) else {
        return None;
    };
    let value = value.get();
    if !value.is_finite() || value.fract() != 0.0 {
        return None;
    }
    let mag = Big::from_f64(value.abs());
    let raw = cx.text(arg).replace('_', "").to_lowercase();
    let canonical = !raw.contains('.')
        && (prefixed_canonical(&raw, &mag)
            || (!raw.contains('e') && raw == number_to_string(value)));
    let mut text = if canonical {
        format!("{}n", cx.text(arg))
    } else {
        suggest = true;
        format!("{}n", mag.to_string_radix(10))
    };
    let negative = neg && !mag.is_zero();
    if negative {
        text = format!("-{text}");
    }
    Some((suggest, text, negative))
}

impl Rule for PreferBigintLiterals {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Call {
            callee,
            optional: false,
            args,
            ..
        } = *cx.node(node)
        else {
            return;
        };
        let args = ast.list(args);
        if ident_name(ast, callee) != Some("BigInt")
            || args.len() != 1
            || matches!(ast.node(args[0]), Node::Spread { .. })
        {
            return;
        }
        let Some((suggest, text, negative)) = replacement(cx, args[0]) else {
            return;
        };
        let span = cx.span(node);
        let mut code = text.clone();
        if negative && !is_parenthesized(cx, node) {
            code = format!("({code})");
            if needs_semicolon(cx, span.start, &code) {
                code = format!(";{code}");
            }
        }
        let fix = Fix::replace(span, code);
        let suggest = suggest || has_comment_in(cx, span);
        let r = cx
            .report(span, "Prefer using bigint literal over `BigInt(…)`.")
            .id("prefer-bigint-literals/error");
        if suggest {
            let shown = if text.encode_utf16().count() < 20 {
                format!("`{text}`")
            } else {
                "a bigint literal".to_owned()
            };
            r.suggest(
                "prefer-bigint-literals/suggestion",
                format!("Replace with {shown}."),
                fix,
            );
        } else {
            r.fix(fix);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn big() {
        assert_eq!(Big::parse("ff", 16).unwrap().to_string_radix(10), "255");
        assert_eq!(
            Big::from_f64(1e21).to_string_radix(10),
            "1000000000000000000000"
        );
        assert_eq!(Big::from_f64(5.0).to_string_radix(2), "101");
        assert!(string_to_bigint("1.5").is_none());
        assert!(string_to_bigint(" -0 ").is_some_and(|b| !b.neg));
    }
}
