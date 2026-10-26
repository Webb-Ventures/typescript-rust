//! `--minify-syntax` numbers in declaration values, as esbuild 0.28.2 prints them
//! (observed):
//!
//! - A number with a fraction and no exponent loses its leading `0` and trailing zeros:
//!   `0.50px` is `.5px`, `1.0` is `1`, `-0.0px` is `-0px`. `010px` and `1e3px` stay.
//! - `calc()` (any case, nested `calc()` and parentheses included) is folded: terms of one
//!   unit (compared without case; the first one's spelling is kept) are added, and a term
//!   is multiplied or divided by a plain number. `calc(1px + 2PX)` is `3px`,
//!   `calc(2em - 3px + 1px)` is `calc(2em - 2px)`, `calc((1px + 2px) * 2)` is `6px`.
//!   The whole `calc()` stays as written when any part can't be folded: another function,
//!   a product of two dimensions or of a multi-term sum, division by zero, `+`/`-` without
//!   whitespace on both sides, or a result that isn't exact to 5 decimal places
//!   (`calc(.1px + .2px)`, `calc(10px / 3)`; `calc(10px / 3 * 3)` is `10px`). In the first
//!   two cases a minified `calc()` has no spaces around `*` and `/` ([`Unfolded::Tree`]).

use super::lexer::T;
use super::{Cv, Sheet};

/// Nesting limit for parentheses and nested `calc()`.
const MAX_DEPTH: usize = 32;

/// `0.50` as `.5` (`None`: printed as written).
pub fn short_number(text: &str) -> Option<String> {
    let n = number_len(text);
    let (num, unit) = text.split_at(n);
    if !num.contains('.') || num.contains(['e', 'E']) {
        return None;
    }
    let (sign, digits) = match num.as_bytes().first() {
        Some(b'+' | b'-') => num.split_at(1),
        _ => ("", num),
    };
    let (int, frac) = digits.split_once('.')?;
    let frac = frac.trim_end_matches('0');
    let int = if int == "0" && !frac.is_empty() {
        ""
    } else {
        int
    };
    let mut out = String::with_capacity(text.len());
    out.push_str(sign);
    if int.is_empty() && frac.is_empty() {
        out.push('0');
    }
    out.push_str(int);
    if !frac.is_empty() {
        out.push('.');
        out.push_str(frac);
    }
    out.push_str(unit);
    (out != text).then_some(out)
}

/// Length of the number at the start of a numeric token (`-1.5e3` of `-1.5e3px`).
fn number_len(text: &str) -> usize {
    let b = text.as_bytes();
    let mut i = 0;
    if matches!(b.first(), Some(b'+' | b'-')) {
        i += 1;
    }
    while b.get(i).is_some_and(u8::is_ascii_digit) {
        i += 1;
    }
    if b.get(i) == Some(&b'.') && b.get(i + 1).is_some_and(u8::is_ascii_digit) {
        i += 1;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
    }
    if matches!(b.get(i), Some(b'e' | b'E')) {
        let mut j = i + 1;
        if matches!(b.get(j), Some(b'+' | b'-')) {
            j += 1;
        }
        if b.get(j).is_some_and(u8::is_ascii_digit) {
            while b.get(j).is_some_and(u8::is_ascii_digit) {
                j += 1;
            }
            i = j;
        }
    }
    i
}

/// One term of a folded sum: a value and its unit (`""` for a number, `%`).
#[derive(Debug, Clone)]
struct Term<'a> {
    value: f64,
    unit: &'a str,
}

impl Term<'_> {
    fn same_unit(&self, other: &Term<'_>) -> bool {
        self.unit.eq_ignore_ascii_case(other.unit)
    }
}

/// Why a `calc()` didn't fold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unfolded {
    /// An inexact or infinite result, division by zero, or `+`/`-` without whitespace:
    /// printed as written.
    AsWritten,
    /// Something that isn't arithmetic on numbers (another function, `1px * 2px`, a
    /// multi-term sum times a number): esbuild prints its expression tree, so `*` and `/`
    /// have no spaces around them when minifying whitespace.
    Tree,
}

/// The folded text of `calc(<items>)`.
pub fn fold(sheet: &Sheet, items: &[Cv]) -> Result<String, Unfolded> {
    let folder = Folder {
        sheet,
        as_written: std::cell::Cell::new(false),
    };
    let Some(terms) = folder.sum(items, 0) else {
        return Err(if folder.as_written.get() {
            Unfolded::AsWritten
        } else {
            Unfolded::Tree
        });
    };
    let mut out = String::new();
    for (i, t) in terms.iter().enumerate() {
        let scaled = (t.value * 1e5).round() / 1e5;
        if !t.value.is_finite() || (t.value.fract() != 0.0 && scaled != t.value) {
            return Err(Unfolded::AsWritten);
        }
        let v = if t.value == 0.0 { 0.0 } else { t.value };
        if i > 0 {
            out.push_str(if v < 0.0 { " - " } else { " + " });
        }
        let shown = if i > 0 { v.abs() } else { v };
        let text = format!("{shown}");
        out.push_str(&short_number(&text).unwrap_or(text));
        out.push_str(t.unit);
    }
    Ok(if terms.len() == 1 {
        out
    } else {
        format!("calc({out})")
    })
}

struct Folder<'s> {
    sheet: &'s Sheet,
    /// A failure that leaves the `calc()` as written ([`Unfolded::AsWritten`]).
    as_written: std::cell::Cell<bool>,
}

impl<'s> Folder<'s> {
    /// `a + b - c`: `+` and `-` need whitespace on both sides.
    fn sum(&self, items: &'s [Cv], depth: usize) -> Option<Vec<Term<'s>>> {
        if depth > MAX_DEPTH {
            return None;
        }
        // Non-whitespace items, each with whether whitespace comes before it.
        let mut parts: Vec<(&'s Cv, bool)> = Vec::new();
        let mut ws = false;
        for cv in items {
            match cv {
                Cv::Tok(t) if t.t == T::Ws => ws = true,
                _ => {
                    parts.push((cv, ws));
                    ws = false;
                }
            }
        }
        let mut out: Vec<Term<'s>> = Vec::new();
        let mut i = 0;
        let mut sign = 1.0;
        loop {
            // One product: operands joined by `*` or `/`.
            let mut acc = self.operand(parts.get(i)?.0, depth)?;
            i += 1;
            while let Some((Cv::Tok(t), _)) = parts.get(i)
                && t.t == T::Delim
                && matches!(self.sheet.text(t), "*" | "/")
            {
                let div = self.sheet.text(t) == "/";
                let rhs = self.operand(parts.get(i + 1)?.0, depth)?;
                if div && matches!(rhs.as_slice(), [t] if t.unit.is_empty() && t.value == 0.0) {
                    self.as_written.set(true);
                    return None;
                }
                acc = product(acc, rhs, div)?;
                i += 2;
            }
            for mut t in acc {
                t.value *= sign;
                match out.iter_mut().find(|o| o.same_unit(&t)) {
                    Some(o) => o.value += t.value,
                    None => out.push(t),
                }
            }
            let Some(&(cv, before)) = parts.get(i) else {
                return Some(out);
            };
            let Cv::Tok(t) = cv else {
                return None;
            };
            sign = match self.sheet.text(t) {
                "+" if t.t == T::Delim => 1.0,
                "-" if t.t == T::Delim => -1.0,
                _ => return None,
            };
            if !before || !parts.get(i + 1).is_some_and(|p| p.1) {
                self.as_written.set(true);
                return None;
            }
            i += 1;
        }
    }

    fn operand(&self, cv: &'s Cv, depth: usize) -> Option<Vec<Term<'s>>> {
        match cv {
            Cv::Tok(t) if matches!(t.t, T::Number | T::Percentage | T::Dimension) => {
                let text = self.sheet.text(t);
                let n = number_len(text);
                let value: f64 = text[..n].parse().ok()?;
                Some(vec![Term {
                    value,
                    unit: &text[n..],
                }])
            }
            Cv::Block { open: b'(', items } => self.sum(items, depth + 1),
            Cv::Func { name, items } if self.sheet.text(name).eq_ignore_ascii_case("calc(") => {
                self.sum(items, depth + 1)
            }
            _ => None,
        }
    }
}

/// `a * b` or `a / b` of single terms, one side (the right one for `/`) a plain number.
fn product<'a>(a: Vec<Term<'a>>, b: Vec<Term<'a>>, div: bool) -> Option<Vec<Term<'a>>> {
    let ([a], [b]) = (a.as_slice(), b.as_slice()) else {
        return None;
    };
    if div {
        if !b.unit.is_empty() || b.value == 0.0 {
            return None;
        }
        return Some(vec![Term {
            value: a.value / b.value,
            unit: a.unit,
        }]);
    }
    let unit = match (a.unit.is_empty(), b.unit.is_empty()) {
        (true, _) => b.unit,
        (false, true) => a.unit,
        (false, false) => return None,
    };
    Some(vec![Term {
        value: a.value * b.value,
        unit,
    }])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_numbers() {
        let f = |s: &str| short_number(s).unwrap_or_else(|| s.to_owned());
        assert_eq!(f("0.50px"), ".5px");
        assert_eq!(f("1.0"), "1");
        assert_eq!(f("-0.0px"), "-0px");
        assert_eq!(f("0.0"), "0");
        assert_eq!(f("-0.5em"), "-.5em");
        assert_eq!(f("50.0%"), "50%");
        assert_eq!(f("010px"), "010px");
        assert_eq!(f("1.50E2px"), "1.50E2px");
        assert_eq!(f("+1px"), "+1px");
    }
}
