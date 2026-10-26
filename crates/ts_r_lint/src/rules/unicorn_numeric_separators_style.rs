//! `unicorn/numeric-separators-style`: group the digits of number literals with `_`.
//!
//! Binary and octal digits go in groups of 4 and hexadecimal in groups of 2 (from the
//! right); decimal integers of at least 5 digits in groups of 3, the exponent likewise, and
//! the fraction (from the left) only when `fractionGroupLength` is set. Each kind
//! (`binary`, `octal`, `hexadecimal`, `number`) takes `minimumDigits`, `groupLength` and
//! `onlyIfContainsSeparator` (which also has a top-level default). Existing separators are
//! removed first; legacy octals (`0777`) are skipped. The literal is reported and replaced.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/numeric-separators-style",
        "Enforce the style of numeric separators by correctly grouping digits.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<NumericSeparatorsStyle>,
};

#[derive(Debug, Clone, Copy)]
struct Group {
    only_if_separated: bool,
    minimum_digits: usize,
    length: usize,
    fraction_length: usize,
}

#[derive(Debug)]
struct NumericSeparatorsStyle {
    binary: Group,
    octal: Group,
    hex: Group,
    number: Group,
}

impl FromOptions for NumericSeparatorsStyle {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        let only = bool_field(o, "onlyIfContainsSeparator", false);
        let group = |key: &str, length: usize, minimum_digits: usize| {
            let g = o.get(key).and_then(Value::as_object);
            let num = |k: &str, d: usize| {
                g.and_then(|g| g.get(k))
                    .and_then(Value::as_u64)
                    .map_or(d, |v| usize::try_from(v).unwrap_or(usize::MAX))
            };
            Group {
                only_if_separated: g
                    .and_then(|g| g.get("onlyIfContainsSeparator"))
                    .and_then(Value::as_bool)
                    .unwrap_or(only),
                minimum_digits: num("minimumDigits", minimum_digits),
                length: num("groupLength", length).max(1),
                fraction_length: num("fractionGroupLength", usize::MAX).max(1),
            }
        };
        Ok(NumericSeparatorsStyle {
            binary: group("binary", 4, 0),
            octal: group("octal", 4, 0),
            hex: group("hexadecimal", 2, 0),
            number: group("number", 3, 5),
        })
    }
}

impl Rule for NumericSeparatorsStyle {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::NumberLit, NodeKind::BigIntLit]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let span = cx.span(node);
        let raw = span.text(cx.src());
        let (digits, suffix) = match cx.node(node) {
            Node::BigIntLit { .. } => match raw.strip_suffix('n') {
                Some(d) => (d, "n"),
                None => return,
            },
            _ => {
                let b = raw.as_bytes();
                if b.len() > 1 && b[0] == b'0' && b.iter().all(u8::is_ascii_digit) {
                    return; // legacy octal
                }
                (raw, "")
            }
        };
        let stripped = digits.replace('_', "");
        let (prefix, data) = match stripped.get(..2) {
            Some(p)
                if p.eq_ignore_ascii_case("0b")
                    || p.eq_ignore_ascii_case("0o")
                    || p.eq_ignore_ascii_case("0x") =>
            {
                (p, &stripped[2..])
            }
            _ => ("", stripped.as_str()),
        };
        let group = match prefix.to_ascii_lowercase().as_str() {
            "0b" => self.binary,
            "0o" => self.octal,
            "0x" => self.hex,
            _ => self.number,
        };
        if group.only_if_separated && !raw.contains('_') {
            return;
        }
        let body = if prefix.is_empty() {
            let Some(formatted) = format_decimal(data, &self.number) else {
                return;
            };
            formatted
        } else {
            separate(data, &group, group.length, false)
        };
        let formatted = format!("{prefix}{body}{suffix}");
        if formatted != raw {
            cx.report(span, "Invalid group length in numeric value.")
                .id("numeric-separators-style")
                .fix(Fix::replace(span, formatted));
        }
    }
}

/// `int.frac` with an optional exponent, each part grouped.
fn format_decimal(text: &str, g: &Group) -> Option<String> {
    let (number, exponent) = match text.find(['e', 'E']) {
        Some(i) => (&text[..i], Some((&text[i..i + 1], &text[i + 1..]))),
        None => (text, None),
    };
    let (integer, fraction) = match number.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (number, None),
    };
    let mut out = separate(integer, g, g.length, false);
    if let Some(f) = fraction {
        out.push('.');
        out.push_str(&separate(f, g, g.fraction_length, true));
    }
    if let Some((mark, rest)) = exponent {
        let (sign, power) = match rest.strip_prefix(['+', '-']) {
            Some(p) => (&rest[..1], p),
            None => ("", rest),
        };
        if power.is_empty() {
            return None;
        }
        out.push_str(mark);
        out.push_str(sign);
        out.push_str(&separate(power, g, g.length, false));
    }
    Some(out)
}

/// Groups ASCII digits with `_`, counting from the right (or the left).
fn separate(digits: &str, g: &Group, length: usize, from_left: bool) -> String {
    let n = digits.len();
    if n < g.minimum_digits || n <= length {
        return digits.to_owned();
    }
    let mut out = String::with_capacity(n + n / length);
    let first = if from_left {
        length
    } else {
        (n - 1) % length + 1
    };
    out.push_str(&digits[..first]);
    let mut i = first;
    while i < n {
        out.push('_');
        let end = (i + length).min(n);
        out.push_str(&digits[i..end]);
        i = end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{Group, format_decimal, separate};

    const NUMBER: Group = Group {
        only_if_separated: false,
        minimum_digits: 5,
        length: 3,
        fraction_length: usize::MAX,
    };

    #[test]
    fn groups_digits() {
        assert_eq!(separate("1234567", &NUMBER, 3, false), "1_234_567");
        assert_eq!(separate("1234", &NUMBER, 3, false), "1234");
        assert_eq!(
            format_decimal("12345.67891e100000", &NUMBER).as_deref(),
            Some("12_345.67891e100_000")
        );
        assert_eq!(
            separate(
                "ABCDEF",
                &Group {
                    minimum_digits: 0,
                    ..NUMBER
                },
                2,
                false
            ),
            "AB_CD_EF"
        );
    }
}
