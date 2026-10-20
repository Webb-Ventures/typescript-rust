//! `no-loss-of-precision`: disallow number literals that lose precision.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-loss-of-precision",
        "Disallow literal numbers that lose precision",
        ESLINT_REC,
        Category::Correctness,
    ),
    create: create_default::<NoLossOfPrecision>,
};

#[derive(Debug, Default)]
struct NoLossOfPrecision;

/// True when the literal `raw` cannot be represented exactly enough as an f64.
pub(crate) fn loses_precision(raw: &str) -> bool {
    let t: String = raw.chars().filter(|&c| c != '_').collect();
    let lower = t.to_ascii_lowercase();
    let radix_digits = if let Some(r) = lower.strip_prefix("0x") {
        Some((16, r.to_owned()))
    } else if let Some(r) = lower.strip_prefix("0o") {
        Some((8, r.to_owned()))
    } else if let Some(r) = lower.strip_prefix("0b") {
        Some((2, r.to_owned()))
    } else if lower.len() > 1
        && lower.starts_with('0')
        && lower.bytes().all(|b| (b'0'..=b'7').contains(&b))
    {
        Some((8, lower[1..].to_owned()))
    } else {
        None
    };
    if let Some((radix, digits)) = radix_digits {
        // Significant bits from the highest to the lowest set bit must fit in 53.
        let bits_per = match radix {
            16 => 4,
            8 => 3,
            _ => 1,
        };
        let mut bits = String::new();
        for c in digits.chars() {
            let v = c.to_digit(radix).unwrap_or(0);
            for i in (0..bits_per).rev() {
                bits.push(if v >> i & 1 == 1 { '1' } else { '0' });
            }
        }
        let trimmed = bits.trim_start_matches('0').trim_end_matches('0');
        return trimmed.len() > 53;
    }
    let (mantissa, _exp) = match lower.split_once('e') {
        Some((m, e)) => (m, Some(e)),
        None => (lower.as_str(), None),
    };
    let (int, frac) = match mantissa.split_once('.') {
        Some((i, f)) => (i, f),
        None => (mantissa, ""),
    };
    let digits: String = if frac.is_empty() {
        int.trim_start_matches('0').trim_end_matches('0').to_owned()
    } else {
        format!("{int}{frac}").trim_start_matches('0').to_owned()
    };
    if digits.is_empty() {
        return false;
    }
    let Ok(v) = t.parse::<f64>() else {
        return false;
    };
    if v.is_infinite() || v == 0.0 {
        return true;
    }
    let n = digits.len();
    if n > 100 {
        // Beyond `toPrecision`'s range: never exact enough.
        return true;
    }
    let formatted = format!("{:.*e}", n - 1, v.abs());
    let (m, _) = formatted.split_once('e').unwrap_or((&formatted, ""));
    let got: String = m.chars().filter(|c| c.is_ascii_digit()).collect();
    got != digits
}

impl Rule for NoLossOfPrecision {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::NumberLit]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        if loses_precision(cx.text(node)) {
            cx.report_node(node, "This number literal will lose precision at runtime.")
                .id("noLossOfPrecision");
        }
    }
}
