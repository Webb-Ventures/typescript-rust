//! A partial validator for regular expression patterns. tsc 7 reports these errors from
//! its checker; the parser keeps them only in a file without syntax errors.
//!
//! Only errors that valid code can never produce are reported, so a pattern this module
//! does not fully understand yields no error:
//! - escapes: `\x` and `\u` without enough hex digits (TS1125, in every mode), `\u{...}`
//!   (TS1538 without the `u` or `v` flag, TS1125, TS1198, TS1199, TS1126);
//! - with `u` or `v`: a lone `}` or `]` (TS1508);
//! - character class ranges whose ends are out of order (TS1517). Without `u` or `v`, an
//!   astral character is two UTF-16 units, as in JavaScript.

use ts_r_diagnostics::{DiagnosticMessage, messages};

use crate::utf8_len;

/// One error: byte offsets in the source, the message and its argument.
pub(crate) type RegexError = (usize, usize, DiagnosticMessage, &'static str);

/// Validates the pattern `src[start..end]` (between the slashes).
pub(crate) fn validate(
    src: &str,
    start: usize,
    end: usize,
    unicode: bool,
    sets: bool,
) -> Vec<RegexError> {
    let mut v = Validator {
        src,
        b: src.as_bytes(),
        end,
        unicode: unicode || sets,
        sets,
        errors: Vec::new(),
    };
    v.pattern(start);
    v.errors
}

struct Validator<'a> {
    src: &'a str,
    b: &'a [u8],
    end: usize,
    /// `u` or `v`.
    unicode: bool,
    /// `v`.
    sets: bool,
    errors: Vec<RegexError>,
}

/// A class atom's value: one code point, or (without `u`) the two UTF-16 units of an
/// astral character. `None` for class escapes (`\d`) and anything not understood.
type Atom = Option<(u32, Option<u32>)>;

impl Validator<'_> {
    fn error(&mut self, at: usize, len: usize, msg: DiagnosticMessage, arg: &'static str) {
        self.errors.push((at, (at + len).min(self.end), msg, arg));
    }

    fn byte(&self, i: usize) -> Option<u8> {
        if i < self.end { Some(self.b[i]) } else { None }
    }

    fn pattern(&mut self, mut i: usize) {
        // A `{` that starts no quantifier has errors of its own in tsc (TS1505, TS1507, ...)
        // which are not reported here, and no `}` after it is reported either.
        let mut lone_brace = false;
        let mut groups = 0u32;
        while i < self.end {
            match self.b[i] {
                b'\\' => i = self.escape(i, false).1,
                b'[' => i = self.class(i),
                b'(' => {
                    groups += 1;
                    i += 1;
                }
                b')' => {
                    if groups == 0 {
                        self.error(i, 1, messages::REGEX_UNEXPECTED_CHAR, ")");
                    } else {
                        groups -= 1;
                    }
                    i += 1;
                }
                b'{' => {
                    if let Some(next) = self.quantifier_end(i) {
                        i = next;
                    } else {
                        lone_brace = true;
                        i += 1;
                    }
                }
                b'}' if self.unicode && !lone_brace => {
                    self.error(i, 1, messages::REGEX_UNEXPECTED_CHAR, "}");
                    i += 1;
                }
                b']' if self.unicode => {
                    self.error(i, 1, messages::REGEX_UNEXPECTED_CHAR, "]");
                    i += 1;
                }
                c => i += utf8_len(c),
            }
        }
        if groups > 0 {
            // `/fo(o/`: one error at the end, however many groups are open (parser579071).
            self.error(self.end, 0, messages::EXPECTED, ")");
        }
    }

    /// At `{`: the end of a `{n}`, `{n,}` or `{n,m}` quantifier.
    fn quantifier_end(&self, i: usize) -> Option<usize> {
        let mut j = i + 1;
        let digits = |j: &mut usize| {
            let s = *j;
            while self.byte(*j).is_some_and(|c| c.is_ascii_digit()) {
                *j += 1;
            }
            *j > s
        };
        if !digits(&mut j) {
            return None;
        }
        if self.byte(j) == Some(b',') {
            j += 1;
            digits(&mut j);
        }
        (self.byte(j) == Some(b'}')).then_some(j + 1)
    }

    /// A character class at `[`; returns the offset after its `]` (or the end).
    fn class(&mut self, open: usize) -> usize {
        let mut i = open + 1;
        if self.byte(i) == Some(b'^') {
            i += 1;
        }
        // The last atom (its value and offset), and the start of a range waiting for its end.
        let mut last: Option<(u32, usize)> = None;
        let mut range_start: Option<(u32, usize)> = None;
        while let Some(c) = self.byte(i) {
            if c == b']' {
                return i + 1;
            }
            if self.sets {
                if c == b'[' {
                    i = self.class(i);
                    (last, range_start) = (None, None);
                    continue;
                }
                if matches!(c, b'-' | b'&') && self.byte(i + 1) == Some(c) {
                    // `--` and `&&` set operations: no ranges around them.
                    i += 2;
                    (last, range_start) = (None, None);
                    continue;
                }
            }
            if c == b'-' && range_start.is_none() && last.is_some() {
                if !matches!(self.byte(i + 1), None | Some(b']')) {
                    range_start = last.take();
                }
                i += 1;
                continue;
            }
            let at = i;
            let (atom, next) = if c == b'\\' {
                self.escape(i, true)
            } else {
                let ch = self.src[i..].chars().next().unwrap_or('\u{FFFD}') as u32;
                let atom = if ch > 0xFFFF && !self.unicode {
                    let u = ch - 0x10000;
                    Some((0xD800 + (u >> 10), Some(0xDC00 + (u & 0x3FF))))
                } else {
                    Some((ch, None))
                };
                (atom, i + utf8_len(c))
            };
            i = next;
            match (range_start.take(), atom) {
                (Some((low, low_at)), Some((high, rest))) => {
                    if low > high {
                        self.error(
                            low_at,
                            next - low_at,
                            messages::REGEX_RANGE_OUT_OF_ORDER,
                            "",
                        );
                    }
                    // The second unit of an astral range end is an atom of its own.
                    last = rest.map(|r| (r, at));
                }
                (_, Some((first, rest))) => last = Some((rest.unwrap_or(first), at)),
                (_, None) => last = None,
            }
        }
        // `/[[]/v`: the outer class is never closed (regExpWithOpenBracketInCharClass).
        // Without `v` the lexer only ends a regex outside a class.
        let reported = self
            .errors
            .last()
            .is_some_and(|e| e.0 == self.end && e.2.code == 1005);
        if self.sets && !reported {
            self.error(self.end, 0, messages::EXPECTED, "]");
        }
        i
    }

    /// An escape at `\`: its value as a class atom and the offset after it.
    fn escape(&mut self, at: usize, in_class: bool) -> (Atom, usize) {
        let Some(c) = self.byte(at + 1) else {
            return (None, at + 1);
        };
        let i = at + 2;
        let simple = |v: u32| (Some((v, None)), i);
        match c {
            b'x' => match self.hex_digits(i, 2) {
                Ok(v) => (Some((v, None)), i + 2),
                Err(bad) => {
                    self.error(bad, 1, messages::HEXADECIMAL_DIGIT_EXPECTED, "");
                    (None, bad)
                }
            },
            b'u' if self.byte(i) == Some(b'{') => self.braced_unicode(at),
            b'u' => match self.hex_digits(i, 4) {
                Ok(v) => {
                    // `𝘈` is one code point with `u`.
                    if self.unicode
                        && (0xD800..0xDC00).contains(&v)
                        && self.byte(i + 4) == Some(b'\\')
                        && self.byte(i + 5) == Some(b'u')
                        && let Ok(lo) = self.hex_digits(i + 6, 4)
                        && (0xDC00..0xE000).contains(&lo)
                    {
                        let cp = 0x10000 + ((v - 0xD800) << 10) + (lo - 0xDC00);
                        return (Some((cp, None)), i + 10);
                    }
                    (Some((v, None)), i + 4)
                }
                Err(bad) => {
                    self.error(bad, 1, messages::HEXADECIMAL_DIGIT_EXPECTED, "");
                    (None, bad)
                }
            },
            b'p' | b'P' | b'q' if self.unicode && self.byte(i) == Some(b'{') => {
                // `\p{L}`, `\q{abc}`: skip the braces. A malformed one ends before a `]`
                // (`[\P{]`), or for `\p` at anything but a name or `=`.
                let mut j = i + 1;
                while let Some(d) = self.byte(j) {
                    let name = d.is_ascii_alphanumeric() || matches!(d, b'_' | b'=');
                    if d == b'}' || d == b']' || (c != b'q' && !name) {
                        break;
                    }
                    j += 1;
                }
                if self.byte(j) == Some(b'}') {
                    j += 1;
                }
                (None, j)
            }
            b'b' if in_class => simple(8),
            b't' => simple(9),
            b'n' => simple(10),
            b'v' => simple(11),
            b'f' => simple(12),
            b'r' => simple(13),
            b'0' if !self.byte(i).is_some_and(|d| d.is_ascii_digit()) => simple(0),
            b'c' => match self.byte(i) {
                Some(l) if l.is_ascii_alphabetic() => (Some(((l % 32) as u32, None)), i + 1),
                _ => (None, i),
            },
            b'd' | b'D' | b'w' | b'W' | b's' | b'S' | b'p' | b'P' | b'k' | b'B' | b'b' => (None, i),
            b'1'..=b'9' | b'0' => (None, i),
            _ => {
                let ch = self.src[at + 1..].chars().next().unwrap_or('\u{FFFD}');
                let next = at + 1 + ch.len_utf8();
                if (ch as u32) > 0xFFFF && !self.unicode {
                    return (None, next);
                }
                (Some((ch as u32, None)), next)
            }
        }
    }

    /// `n` hex digits at `i`: their value, or the offset of the first non-digit.
    fn hex_digits(&self, i: usize, n: usize) -> Result<u32, usize> {
        let mut v = 0u32;
        for k in 0..n {
            match self.byte(i + k).and_then(hex_val) {
                Some(d) => v = v * 16 + d,
                None => return Err(i + k),
            }
        }
        Ok(v)
    }

    /// `\u{...}` at `\`.
    fn braced_unicode(&mut self, at: usize) -> (Atom, usize) {
        if !self.unicode {
            self.error(at, 1, messages::REGEX_UNICODE_ESCAPE_NEEDS_FLAG, "");
        }
        let digits = at + 3;
        let mut j = digits;
        let mut value = 0u32;
        while let Some(d) = self.byte(j).and_then(hex_val) {
            value = value.saturating_mul(16).saturating_add(d);
            j += 1;
        }
        if j == digits {
            // `\u{}` (the `}` is part of the escape), `\u{r}` (the `r` is not).
            if j >= self.end {
                self.error(j, 0, messages::UNEXPECTED_END_OF_TEXT, "");
                return (None, j);
            }
            self.error(j, 1, messages::HEXADECIMAL_DIGIT_EXPECTED, "");
            let next = if self.b[j] == b'}' { j + 1 } else { j };
            return (None, next);
        }
        match self.byte(j) {
            None => {
                self.error(j, 0, messages::UNEXPECTED_END_OF_TEXT, "");
                (None, j)
            }
            Some(b'}') if value > 0x10FFFF => {
                self.error(
                    digits,
                    j - digits,
                    messages::EXTENDED_UNICODE_ESCAPE_RANGE,
                    "",
                );
                (None, j + 1)
            }
            Some(b'}') => (Some((value, None)), j + 1),
            Some(_) => {
                self.error(j, 1, messages::INVALID_UNICODE_ESCAPE, "");
                (None, j)
            }
        }
    }
}

fn hex_val(c: u8) -> Option<u32> {
    (c as char).to_digit(16)
}
