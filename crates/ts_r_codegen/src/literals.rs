//! Printing synthesized literals: numbers (ECMAScript `Number::toString`), strings, and
//! identifier checks. Literals that come from the source are printed from their raw text.

use std::fmt::Write as _;

/// ECMAScript `Number.prototype.toString()` for finite, non-negative-or-negative values.
/// NaN and infinities are returned as `NaN` / `Infinity` / `-Infinity` (callers that need an
/// expression print `0 / 0` or `1 / 0` instead).
pub fn js_number(v: f64) -> String {
    if v.is_nan() {
        return "NaN".to_owned();
    }
    if v.is_infinite() {
        return if v > 0.0 { "Infinity" } else { "-Infinity" }.to_owned();
    }
    if v == 0.0 {
        return "0".to_owned();
    }
    let mut out = String::new();
    let v = if v < 0.0 {
        out.push('-');
        -v
    } else {
        v
    };
    // Shortest round-trip digits and exponent from Rust's `{:e}` (e.g. "1.2345e3").
    let e = format!("{v:e}");
    let (mantissa, exp) = e.split_once('e').unwrap_or((&e, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i32;
    // n is the position of the decimal point relative to the digits (value = 0.digits * 10^n).
    let n = exp + 1;
    if k <= n && n <= 21 {
        out.push_str(&digits);
        for _ in 0..(n - k) {
            out.push('0');
        }
    } else if 0 < n && n <= 21 {
        out.push_str(&digits[..n as usize]);
        out.push('.');
        out.push_str(&digits[n as usize..]);
    } else if -6 < n && n <= 0 {
        out.push_str("0.");
        for _ in 0..(-n) {
            out.push('0');
        }
        out.push_str(&digits);
    } else {
        out.push_str(&digits[..1]);
        if k > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('e');
        let e = n - 1;
        if e >= 0 {
            out.push('+');
        }
        let _ = write!(out, "{e}");
    }
    out
}

/// Quotes `s` as a JS string literal with `quote` (`"` or `'`), escaping what must be escaped
/// and non-printable characters. Non-ASCII printable characters are kept as is.
pub fn quote_string(s: &str, quote: char, out: &mut String) {
    out.push(quote);
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{b}' => out.push_str("\\v"),
            '\u{c}' => out.push_str("\\f"),
            '\0' => {
                // `\0` followed by a digit would read as a legacy octal escape.
                if chars.peek().is_some_and(|d| d.is_ascii_digit()) {
                    out.push_str("\\x00");
                } else {
                    out.push_str("\\0");
                }
            }
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            '\u{feff}' => out.push_str("\\uFEFF"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                let _ = write!(out, "\\x{:02X}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push(quote);
}

/// Quotes `s` with `"` as tsc prints a synthesized string: like [`quote_string`], and every
/// non-ASCII UTF-16 unit as `\uXXXX` (uppercase hex).
pub fn quote_string_ascii(s: &str, out: &mut String) {
    let mut q = String::with_capacity(s.len() + 2);
    quote_string(s, '"', &mut q);
    let mut buf = [0u16; 2];
    for c in q.chars() {
        if c.is_ascii() {
            out.push(c);
            continue;
        }
        for u in c.encode_utf16(&mut buf) {
            let _ = write!(out, "\\u{:04X}", *u);
        }
    }
}

/// Picks the quote that needs fewer escapes (double quotes on a tie, like tsc).
pub fn best_quote(s: &str) -> char {
    let d = s.bytes().filter(|&b| b == b'"').count();
    let q = s.bytes().filter(|&b| b == b'\'').count();
    if d > q { '\'' } else { '"' }
}

/// Escapes the content of a template literal segment (no delimiters).
pub fn template_raw(s: &str, out: &mut String) {
    let mut prev_dollar = false;
    for c in s.chars() {
        match c {
            '`' => out.push_str("\\`"),
            '\\' => out.push_str("\\\\"),
            '{' if prev_dollar => out.push_str("\\{"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
        prev_dollar = c == '$';
    }
}

/// True if `s` is an IdentifierName (may be a reserved word).
pub fn is_identifier_name(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c == '$' || c == '_' || ts_r_lexer::is_id_start(c) => {}
        _ => return false,
    }
    chars.all(|c| c == '$' || c == '\u{200c}' || c == '\u{200d}' || ts_r_lexer::is_id_part(c))
}

/// Reserved words that cannot be used as binding identifiers in strict code.
pub fn is_reserved_word(s: &str) -> bool {
    matches!(
        s,
        "break"
            | "case"
            | "catch"
            | "class"
            | "const"
            | "continue"
            | "debugger"
            | "default"
            | "delete"
            | "do"
            | "else"
            | "enum"
            | "export"
            | "extends"
            | "false"
            | "finally"
            | "for"
            | "function"
            | "if"
            | "import"
            | "in"
            | "instanceof"
            | "new"
            | "null"
            | "return"
            | "super"
            | "switch"
            | "this"
            | "throw"
            | "true"
            | "try"
            | "typeof"
            | "var"
            | "void"
            | "while"
            | "with"
            | "implements"
            | "interface"
            | "let"
            | "package"
            | "private"
            | "protected"
            | "public"
            | "static"
            | "yield"
            | "await"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_match_js() {
        let cases: &[(f64, &str)] = &[
            (0.0, "0"),
            (1.0, "1"),
            (-1.5, "-1.5"),
            (100.0, "100"),
            (0.1, "0.1"),
            (1e21, "1e+21"),
            (1e20, "100000000000000000000"),
            (123456789012345680000.0, "123456789012345680000"),
            (1e-7, "1e-7"),
            (0.000001, "0.000001"),
            (1.5e-10, "1.5e-10"),
            (2f64.powi(53), "9007199254740992"),
            (0.1 + 0.2, "0.30000000000000004"),
            (f64::MAX, "1.7976931348623157e+308"),
            (5e-324, "5e-324"),
            (255.0, "255"),
        ];
        for &(v, s) in cases {
            assert_eq!(js_number(v), s, "{v}");
        }
    }

    #[test]
    fn strings() {
        let mut s = String::new();
        quote_string("a\"b'c\n\0\u{1}", '"', &mut s);
        assert_eq!(s, r#""a\"b'c\n\0\x01""#);
        assert_eq!(best_quote("it's"), '"');
        assert_eq!(best_quote("say \"hi\""), '\'');
        let mut t = String::new();
        template_raw("a`${b}\\", &mut t);
        assert_eq!(t, "a\\`$\\{b}\\\\");
    }

    #[test]
    fn identifiers() {
        assert!(is_identifier_name("a$_1"));
        assert!(is_identifier_name("café"));
        assert!(!is_identifier_name("1a"));
        assert!(!is_identifier_name("a-b"));
        assert!(!is_identifier_name(""));
        assert!(is_reserved_word("class"));
    }
}
