//! Decoding token values: escapes in identifiers, strings and templates, and numbers.

use ts_r_diagnostics::{DiagnosticMessage, messages};

fn hex_val(b: u8) -> Option<u32> {
    (b as char).to_digit(16)
}

/// Reads the part of a `\u` escape after the `u`: `XXXX` or `{X...}`. Advances `pos` past
/// what was read. Returns `None` if malformed. Surrogate code points are returned as
/// U+FFFD (they cannot be represented in UTF-8); callers combine pairs separately.
pub(crate) fn read_unicode_escape_body(b: &[u8], pos: &mut usize) -> Option<char> {
    let cp = read_unicode_code_point(b, pos)?;
    Some(char::from_u32(cp).unwrap_or('\u{FFFD}'))
}

/// Like [`read_unicode_escape_body`] but returns the raw code point (surrogates included).
fn read_unicode_code_point(b: &[u8], pos: &mut usize) -> Option<u32> {
    if b.get(*pos) == Some(&b'{') {
        let mut i = *pos + 1;
        let mut v: u32 = 0;
        let mut n = 0;
        while let Some(d) = b.get(i).and_then(|&c| hex_val(c)) {
            v = v.saturating_mul(16).saturating_add(d);
            i += 1;
            n += 1;
        }
        if n == 0 || b.get(i) != Some(&b'}') || v > 0x10FFFF {
            *pos = i;
            return None;
        }
        *pos = i + 1;
        Some(v)
    } else {
        let mut v = 0;
        for k in 0..4 {
            match b.get(*pos + k).and_then(|&c| hex_val(c)) {
                Some(d) => v = v * 16 + d,
                None => {
                    *pos += k;
                    return None;
                }
            }
        }
        *pos += 4;
        Some(v)
    }
}

/// Decodes `\uXXXX` / `\u{X}` escapes in an identifier.
pub(crate) fn decode_identifier(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && b.get(i + 1) == Some(&b'u') {
            let mut p = i + 2;
            match read_unicode_escape_body(b, &mut p) {
                Some(c) => out.push(c),
                None => out.push('\u{FFFD}'),
            }
            i = p.max(i + 2);
        } else {
            let c = text[i..].chars().next().unwrap_or('\u{FFFD}');
            out.push(c);
            i += c.len_utf8();
        }
    }
    out
}

/// An invalid escape: the byte offset to report at and the message.
pub(crate) type EscapeError = (usize, DiagnosticMessage);

/// Cooks string or template text (without delimiters). On invalid escapes returns the best
/// effort value and every escape's error, in order (`"\u{r}\u{n}"` has two).
pub(crate) fn cook(raw: &str, template: bool) -> Result<String, (String, Vec<EscapeError>)> {
    let b = raw.as_bytes();
    let mut out = String::with_capacity(raw.len());
    let mut err: Vec<EscapeError> = Vec::new();
    let mut i = 0;
    // Pending high surrogate from a `\uD8xx` escape.
    let mut high: Option<u32> = None;
    while i < b.len() {
        let c = b[i];
        if c == b'\r' {
            flush_high(&mut out, &mut high);
            // CR and CRLF become LF (templates only; strings cannot contain raw CR).
            out.push('\n');
            i += if b.get(i + 1) == Some(&b'\n') { 2 } else { 1 };
            continue;
        }
        if c != b'\\' {
            flush_high(&mut out, &mut high);
            let ch = raw[i..].chars().next().unwrap_or('\u{FFFD}');
            out.push(ch);
            i += ch.len_utf8();
            continue;
        }
        let esc_start = i;
        i += 1;
        let Some(&e) = b.get(i) else { break };
        i += 1;
        match e {
            b'n' => push(&mut out, &mut high, '\n'),
            b't' => push(&mut out, &mut high, '\t'),
            b'r' => push(&mut out, &mut high, '\r'),
            b'b' => push(&mut out, &mut high, '\u{8}'),
            b'f' => push(&mut out, &mut high, '\u{C}'),
            b'v' => push(&mut out, &mut high, '\u{B}'),
            b'\r' => {
                flush_high(&mut out, &mut high);
                if b.get(i) == Some(&b'\n') {
                    i += 1;
                }
            }
            b'\n' => flush_high(&mut out, &mut high),
            b'0'..=b'7' => {
                let next_digit = b.get(i).is_some_and(|d| d.is_ascii_digit());
                if e == b'0' && !next_digit {
                    push(&mut out, &mut high, '\0');
                } else if template {
                    err.push((esc_start, messages::OCTAL_LITERALS_NOT_ALLOWED));
                } else {
                    // Legacy octal escape: up to three digits, value <= 0o377.
                    let mut v = (e - b'0') as u32;
                    let max_len = if e <= b'3' { 2 } else { 1 };
                    let mut n = 0;
                    while n < max_len {
                        match b.get(i) {
                            Some(&d @ b'0'..=b'7') => {
                                v = v * 8 + (d - b'0') as u32;
                                i += 1;
                                n += 1;
                            }
                            _ => break,
                        }
                    }
                    push(&mut out, &mut high, char::from_u32(v).unwrap_or('\u{FFFD}'));
                }
            }
            b'8' | b'9' => {
                if template {
                    err.push((esc_start, messages::OCTAL_LITERALS_NOT_ALLOWED));
                } else {
                    push(&mut out, &mut high, e as char);
                }
            }
            b'x' => {
                match (
                    b.get(i).and_then(|&c| hex_val(c)),
                    b.get(i + 1).and_then(|&c| hex_val(c)),
                ) {
                    (Some(h), Some(l)) => {
                        i += 2;
                        push(
                            &mut out,
                            &mut high,
                            char::from_u32(h * 16 + l).unwrap_or('\u{FFFD}'),
                        );
                    }
                    (first, _) => {
                        // Reported at the first character that is not a hex digit.
                        let at = if first.is_some() { i + 1 } else { i };
                        err.push((at, messages::HEXADECIMAL_DIGIT_EXPECTED));
                    }
                }
            }
            b'u' => {
                let mut p = i;
                match read_unicode_code_point(b, &mut p) {
                    Some(cp) => {
                        i = p;
                        if (0xD800..0xDC00).contains(&cp) {
                            flush_high(&mut out, &mut high);
                            high = Some(cp);
                        } else if (0xDC00..0xE000).contains(&cp) {
                            match high.take() {
                                Some(h) => {
                                    let full = 0x10000 + ((h - 0xD800) << 10) + (cp - 0xDC00);
                                    out.push(char::from_u32(full).unwrap_or('\u{FFFD}'));
                                }
                                None => out.push('\u{FFFD}'),
                            }
                        } else {
                            push(
                                &mut out,
                                &mut high,
                                char::from_u32(cp).unwrap_or('\u{FFFD}'),
                            );
                        }
                    }
                    None => {
                        // `\u{}` / `\u{r}`: TS1125 after the `{`; `\u{110000}`: TS1198 at
                        // the digits; `\u{67`: TS1199 after them (unicodeExtendedEscapes*).
                        let (at, msg) = if b.get(i) == Some(&b'{') {
                            let digits = i + 1;
                            let value = b[digits..p]
                                .iter()
                                .filter_map(|&c| hex_val(c))
                                .fold(0u32, |v, d| v.saturating_mul(16).saturating_add(d));
                            if p == digits {
                                (digits, messages::HEXADECIMAL_DIGIT_EXPECTED)
                            } else if value > 0x10FFFF {
                                (digits, messages::EXTENDED_UNICODE_ESCAPE_RANGE)
                            } else {
                                (p, messages::INVALID_UNICODE_ESCAPE)
                            }
                        } else {
                            (p, messages::HEXADECIMAL_DIGIT_EXPECTED)
                        };
                        err.push((at, msg));
                        i = p.max(i);
                        if b.get(i) == Some(&b'}')
                            && msg.code != messages::INVALID_UNICODE_ESCAPE.code
                        {
                            i += 1;
                        }
                    }
                }
            }
            _ => {
                // Any other character (including U+2028/2029 continuations) is itself.
                let ch = raw[i - 1..].chars().next().unwrap_or('\u{FFFD}');
                i += ch.len_utf8() - 1;
                if ch == '\u{2028}' || ch == '\u{2029}' {
                    flush_high(&mut out, &mut high);
                } else {
                    push(&mut out, &mut high, ch);
                }
            }
        }
    }
    flush_high(&mut out, &mut high);
    if err.is_empty() {
        Ok(out)
    } else {
        Err((out, err))
    }
}

#[inline]
fn push(out: &mut String, high: &mut Option<u32>, c: char) {
    flush_high(out, high);
    out.push(c);
}

#[inline]
fn flush_high(out: &mut String, high: &mut Option<u32>) {
    if high.take().is_some() {
        out.push('\u{FFFD}');
    }
}

/// Parses the text of a numeric literal (without a `n` suffix check) into an `f64`.
/// Handles separators, `0x`/`0o`/`0b`, legacy octal and leading-zero decimals.
pub fn parse_number(text: &str) -> f64 {
    let text = text.strip_suffix('n').unwrap_or(text);
    let owned;
    let t = if text.contains('_') {
        owned = text.replace('_', "");
        owned.as_str()
    } else {
        text
    };
    let b = t.as_bytes();
    if b.len() > 1 && b[0] == b'0' {
        let radix = match b[1] {
            b'x' | b'X' => Some(16),
            b'o' | b'O' => Some(8),
            b'b' | b'B' => Some(2),
            b'0'..=b'7' if b[1..].iter().all(|d| (b'0'..=b'7').contains(d)) => {
                return radix_value(&t[1..], 8);
            }
            _ => None,
        };
        if let Some(r) = radix {
            return radix_value(&t[2..], r);
        }
    }
    t.parse::<f64>().unwrap_or(f64::NAN)
}

fn radix_value(digits: &str, radix: u32) -> f64 {
    // Exact for up to 128 bits; beyond that, accumulate in f64.
    let mut v: u128 = 0;
    let mut overflow = false;
    for c in digits.chars() {
        let Some(d) = c.to_digit(radix) else { break };
        match v
            .checked_mul(radix as u128)
            .and_then(|x| x.checked_add(d as u128))
        {
            Some(n) => v = n,
            None => {
                overflow = true;
                break;
            }
        }
    }
    if !overflow {
        return v as f64;
    }
    let mut f = 0f64;
    for c in digits.chars() {
        let Some(d) = c.to_digit(radix) else { break };
        f = f * radix as f64 + d as f64;
    }
    f
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers() {
        assert_eq!(parse_number("0"), 0.0);
        assert_eq!(parse_number("1_000"), 1000.0);
        assert_eq!(parse_number("0x1F"), 31.0);
        assert_eq!(parse_number("0o17"), 15.0);
        assert_eq!(parse_number("0b101"), 5.0);
        assert_eq!(parse_number("017"), 15.0);
        assert_eq!(parse_number("089"), 89.0);
        assert_eq!(parse_number(".5"), 0.5);
        assert_eq!(parse_number("5."), 5.0);
        assert_eq!(parse_number("1e3"), 1000.0);
        assert_eq!(parse_number("1.5E-2"), 0.015);
        assert_eq!(
            parse_number("0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF"),
            2f64.powi(136) - 1.0
        );
        assert_eq!(parse_number("123n"), 123.0);
    }

    #[test]
    fn cooking() {
        assert_eq!(cook(r"a\nb", false).unwrap(), "a\nb");
        assert_eq!(
            cook(concat!(r"\x41", "\\", "u0042", r"\u{43}"), false).unwrap(),
            "ABC"
        );
        assert_eq!(
            cook(concat!("\\", "uD83D", "\\", "uDE00"), false).unwrap(),
            "😀"
        );
        assert_eq!(cook(r"\uD83D", false).unwrap(), "\u{FFFD}");
        assert_eq!(cook("a\\\nb", false).unwrap(), "ab");
        assert_eq!(cook(r"\101", false).unwrap(), "A");
        assert_eq!(cook(r"\0", false).unwrap(), "\0");
        assert_eq!(cook(r"\q", false).unwrap(), "q");
        assert!(cook(r"\01", true).is_err());
        assert!(cook(r"\xZ", false).is_err());
        assert!(cook(r"\u{110000}", false).is_err());
        assert_eq!(cook("a\r\nb", true).unwrap(), "a\nb");
        assert_eq!(decode_identifier(concat!("\\", "u0061b", r"\u{63}")), "abc");
    }
}
