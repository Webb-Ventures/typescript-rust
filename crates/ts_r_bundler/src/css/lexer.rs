//! CSS tokens (CSS Syntax Level 3, section 4): byte ranges into the source, nothing decoded.
//! Comments are dropped. The tokenizer always consumes input: every iteration of its loop
//! advances by at least one byte.

/// A token's type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum T {
    Ident,
    /// `name(`: the range includes the `(`.
    Function,
    /// `@name`.
    AtKeyword,
    /// `#name` where the name would not start an identifier (`#123`).
    Hash,
    /// `#name` where the name starts an identifier (an ID selector).
    IdHash,
    Str,
    BadStr,
    /// An unquoted `url(...)`, the whole range.
    Url,
    BadUrl,
    Delim,
    Number,
    Percentage,
    Dimension,
    Ws,
    Cdo,
    Cdc,
    Colon,
    Semicolon,
    Comma,
    OpenSquare,
    CloseSquare,
    OpenParen,
    CloseParen,
    OpenCurly,
    CloseCurly,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Token {
    pub t: T,
    pub start: u32,
    pub end: u32,
}

impl Token {
    pub fn text<'a>(&self, src: &'a str) -> &'a str {
        src.get(self.start as usize..self.end as usize)
            .unwrap_or("")
    }
}

/// A problem found while tokenizing (an unterminated string or URL): position and text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LexWarning {
    pub pos: u32,
    pub text: String,
}

fn is_name_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b >= 0x80
}

fn is_name(b: u8) -> bool {
    is_name_start(b) || b.is_ascii_digit() || b == b'-'
}

/// `\` followed by anything but a newline (or the end).
fn valid_escape(s: &[u8], i: usize) -> bool {
    s.get(i) == Some(&b'\\')
        && s.get(i + 1)
            .is_some_and(|&c| c != b'\n' && c != b'\r' && c != 0x0c)
}

/// Whether the three bytes at `i` would start an identifier.
fn starts_ident(s: &[u8], i: usize) -> bool {
    match s.get(i) {
        Some(b'-') => match s.get(i + 1) {
            Some(&c) if is_name_start(c) || c == b'-' => true,
            _ => valid_escape(s, i + 1),
        },
        Some(&c) if is_name_start(c) => true,
        Some(b'\\') => valid_escape(s, i),
        _ => false,
    }
}

fn starts_number(s: &[u8], i: usize) -> bool {
    match s.get(i) {
        Some(b'+') | Some(b'-') => match s.get(i + 1) {
            Some(c) if c.is_ascii_digit() => true,
            Some(b'.') => s.get(i + 2).is_some_and(u8::is_ascii_digit),
            _ => false,
        },
        Some(b'.') => s.get(i + 1).is_some_and(u8::is_ascii_digit),
        Some(c) => c.is_ascii_digit(),
        None => false,
    }
}

/// Consumes an escape starting after the `\`; returns the new position.
fn escape_end(s: &[u8], mut i: usize) -> usize {
    if i >= s.len() {
        return i;
    }
    if s[i].is_ascii_hexdigit() {
        let start = i;
        while i < s.len() && i - start < 6 && s[i].is_ascii_hexdigit() {
            i += 1;
        }
        if i < s.len() && matches!(s[i], b' ' | b'\t' | b'\n') {
            i += 1;
        }
        return i;
    }
    // One (possibly multi-byte) character.
    i + utf8_len(s[i])
}

fn utf8_len(b: u8) -> usize {
    match b {
        0xf0..=0xff => 4,
        0xe0..=0xef => 3,
        0xc0..=0xdf => 2,
        _ => 1,
    }
}

fn name_end(s: &[u8], mut i: usize) -> usize {
    while i < s.len() {
        if is_name(s[i]) {
            i += 1;
        } else if valid_escape(s, i) {
            i = escape_end(s, i + 1);
        } else {
            break;
        }
    }
    i.min(s.len())
}

fn number_end(s: &[u8], mut i: usize) -> usize {
    if matches!(s.get(i), Some(b'+') | Some(b'-')) {
        i += 1;
    }
    while s.get(i).is_some_and(u8::is_ascii_digit) {
        i += 1;
    }
    if s.get(i) == Some(&b'.') && s.get(i + 1).is_some_and(u8::is_ascii_digit) {
        i += 1;
        while s.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
    }
    if matches!(s.get(i), Some(b'e') | Some(b'E')) {
        let j = if matches!(s.get(i + 1), Some(b'+') | Some(b'-')) {
            i + 2
        } else {
            i + 1
        };
        if s.get(j).is_some_and(u8::is_ascii_digit) {
            i = j;
            while s.get(i).is_some_and(u8::is_ascii_digit) {
                i += 1;
            }
        }
    }
    i
}

/// Tokenizes `src`. Comments are skipped.
pub fn tokenize(src: &str) -> (Vec<Token>, Vec<LexWarning>) {
    let s = src.as_bytes();
    let mut toks = Vec::with_capacity(s.len() / 4);
    let mut warnings = Vec::new();
    let mut i = 0usize;
    while i < s.len() {
        let start = i;
        let c = s[i];
        let t = match c {
            b'/' if s.get(i + 1) == Some(&b'*') => {
                i = match src[i + 2..].find("*/") {
                    Some(k) => i + 2 + k + 2,
                    None => s.len(),
                };
                continue;
            }
            b' ' | b'\t' | b'\n' | b'\r' | 0x0c => {
                while i < s.len() && matches!(s[i], b' ' | b'\t' | b'\n' | b'\r' | 0x0c) {
                    i += 1;
                }
                T::Ws
            }
            b'"' | b'\'' => {
                i += 1;
                let mut t = T::Str;
                loop {
                    match s.get(i) {
                        None => {
                            warnings.push(LexWarning {
                                pos: start as u32,
                                text: "Unterminated string token".into(),
                            });
                            break;
                        }
                        Some(&q) if q == c => {
                            i += 1;
                            break;
                        }
                        Some(b'\n') | Some(b'\r') | Some(0x0c) => {
                            warnings.push(LexWarning {
                                pos: start as u32,
                                text: "Unterminated string token".into(),
                            });
                            t = T::BadStr;
                            break;
                        }
                        Some(b'\\') => {
                            if i + 1 >= s.len() {
                                i += 1;
                            } else if matches!(s[i + 1], b'\n' | 0x0c) {
                                i += 2;
                            } else if s[i + 1] == b'\r' {
                                i += if s.get(i + 2) == Some(&b'\n') { 3 } else { 2 };
                            } else {
                                i = escape_end(s, i + 1);
                            }
                        }
                        Some(_) => i += 1,
                    }
                }
                t
            }
            b'#' if s.get(i + 1).is_some_and(|&b| is_name(b)) || valid_escape(s, i + 1) => {
                let id = starts_ident(s, i + 1);
                i = name_end(s, i + 1);
                if id { T::IdHash } else { T::Hash }
            }
            b'(' => {
                i += 1;
                T::OpenParen
            }
            b')' => {
                i += 1;
                T::CloseParen
            }
            b'[' => {
                i += 1;
                T::OpenSquare
            }
            b']' => {
                i += 1;
                T::CloseSquare
            }
            b'{' => {
                i += 1;
                T::OpenCurly
            }
            b'}' => {
                i += 1;
                T::CloseCurly
            }
            b',' => {
                i += 1;
                T::Comma
            }
            b':' => {
                i += 1;
                T::Colon
            }
            b';' => {
                i += 1;
                T::Semicolon
            }
            b'<' if s[i..].starts_with(b"<!--") => {
                i += 4;
                T::Cdo
            }
            b'-' if s[i..].starts_with(b"-->") => {
                i += 3;
                T::Cdc
            }
            b'@' if starts_ident(s, i + 1) => {
                i = name_end(s, i + 1);
                T::AtKeyword
            }
            _ if starts_number(s, i) => {
                i = number_end(s, i);
                if s.get(i) == Some(&b'%') {
                    i += 1;
                    T::Percentage
                } else if starts_ident(s, i) {
                    i = name_end(s, i);
                    T::Dimension
                } else {
                    T::Number
                }
            }
            _ if starts_ident(s, i) => {
                i = name_end(s, i);
                if s.get(i) == Some(&b'(') {
                    let name = &src[start..i];
                    i += 1;
                    if name.eq_ignore_ascii_case("url") {
                        // `url(` + optional whitespace + a quote is a function; else a URL token.
                        let mut j = i;
                        while j < s.len() && matches!(s[j], b' ' | b'\t' | b'\n' | b'\r' | 0x0c) {
                            j += 1;
                        }
                        if matches!(s.get(j), Some(b'"') | Some(b'\'')) {
                            T::Function
                        } else {
                            let (end, ok, eof) = url_end(s, j);
                            if !ok || eof {
                                warnings.push(LexWarning {
                                    pos: start as u32,
                                    text: "Expected \")\" to end URL token".into(),
                                });
                            }
                            i = end;
                            if ok { T::Url } else { T::BadUrl }
                        }
                    } else {
                        T::Function
                    }
                } else {
                    T::Ident
                }
            }
            _ => {
                i += utf8_len(c);
                T::Delim
            }
        };
        let i2 = i.min(s.len()).max(start + 1);
        i = i2;
        toks.push(Token {
            t,
            start: start as u32,
            end: i as u32,
        });
    }
    (toks, warnings)
}

/// The end of an unquoted URL token's body starting at `i` (after leading whitespace), and
/// whether it was well formed (closed by `)` with nothing invalid inside).
/// The end of a URL token starting at `i`, whether it is well formed, and whether it ran
/// into the end of the input (still a URL token, with a warning).
fn url_end(s: &[u8], mut i: usize) -> (usize, bool, bool) {
    let mut ok = true;
    loop {
        match s.get(i) {
            None => return (i, ok, true),
            Some(b')') => return (i + 1, ok, false),
            Some(b' ' | b'\t' | b'\n' | b'\r' | 0x0c) => {
                while i < s.len() && matches!(s[i], b' ' | b'\t' | b'\n' | b'\r' | 0x0c) {
                    i += 1;
                }
                match s.get(i) {
                    Some(b')') => return (i + 1, ok, false),
                    None => return (i, ok, true),
                    _ => ok = false,
                }
            }
            Some(b'"' | b'\'' | b'(') => {
                ok = false;
                i += 1;
            }
            Some(b'\\') => {
                if valid_escape(s, i) {
                    i = escape_end(s, i + 1);
                } else {
                    ok = false;
                    i += 1;
                }
            }
            Some(&c) => i += utf8_len(c),
        }
    }
}

/// The decoded value of an identifier-like text (escapes resolved).
pub fn unescape(text: &str) -> String {
    let s = text.as_bytes();
    if !text.contains('\\') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'\\' && i + 1 < s.len() {
            let j = i + 1;
            if s[j].is_ascii_hexdigit() {
                let mut k = j;
                while k < s.len() && k - j < 6 && s[k].is_ascii_hexdigit() {
                    k += 1;
                }
                let cp = u32::from_str_radix(&text[j..k], 16).unwrap_or(0xfffd);
                out.push(
                    char::from_u32(cp)
                        .filter(|&c| c != '\0')
                        .unwrap_or('\u{fffd}'),
                );
                if k < s.len() && matches!(s[k], b' ' | b'\t' | b'\n') {
                    k += 1;
                }
                i = k;
            } else if matches!(s[j], b'\n' | b'\r' | 0x0c) {
                i = j + 1;
            } else {
                let n = utf8_len(s[j]);
                out.push_str(text.get(j..j + n).unwrap_or(""));
                i = j + n;
            }
        } else {
            let n = utf8_len(s[i]);
            out.push_str(text.get(i..i + n).unwrap_or(""));
            i += n;
        }
    }
    out
}

/// The value of a string token (quotes removed, escapes resolved).
pub fn string_value(text: &str) -> String {
    let inner = if text.len() >= 2 {
        &text[1..text.len() - if text.ends_with(&text[..1]) { 1 } else { 0 }]
    } else {
        ""
    };
    unescape(inner)
}

/// The value inside an unquoted `url(...)` token.
pub fn url_value(text: &str) -> String {
    let inner = text
        .get(4..)
        .unwrap_or("")
        .strip_suffix(')')
        .unwrap_or_else(|| text.get(4..).unwrap_or(""));
    unescape(inner.trim_matches(|c: char| c.is_ascii_whitespace()))
}

/// `text` quoted as a CSS string, with the quote that needs fewer escapes (double on a tie).
pub fn quote(text: &str) -> String {
    let dq = text.matches('"').count();
    let sq = text.matches('\'').count();
    let q = if dq > sq { '\'' } else { '"' };
    let mut out = String::with_capacity(text.len() + 2);
    out.push(q);
    let mut pending_space = false;
    for ch in text.chars() {
        if pending_space && (ch.is_ascii_hexdigit() || ch == ' ') {
            out.push(' ');
        }
        match ch {
            c if c == q => {
                out.push('\\');
                out.push(c);
            }
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\a "),
            '\r' => out.push_str("\\d "),
            // C1 control characters are escaped (observed in esbuild's output).
            c if ('\u{80}'..='\u{9f}').contains(&c) => {
                out.push_str(&format!("\\{:x}", c as u32));
                pending_space = true;
                continue;
            }
            c => out.push(c),
        }
        pending_space = false;
    }
    out.push(q);
    out
}

/// `text` as a CSS identifier (escaping what can't appear in one).
pub fn escape_ident(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for (i, ch) in text.chars().enumerate() {
        let ok = ch.is_ascii_alphabetic()
            || ch == '_'
            || ch == '-'
            || (ch as u32) >= 0x80
            || (i > 0 && ch.is_ascii_digit());
        if ok {
            out.push(ch);
        } else if ch.is_ascii_digit() {
            out.push_str(&format!("\\{:x} ", ch as u32));
        } else {
            out.push('\\');
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(s: &str) -> Vec<T> {
        tokenize(s).0.into_iter().map(|t| t.t).collect()
    }

    #[test]
    fn basic_tokens() {
        assert_eq!(
            kinds("a{color:red}"),
            vec![
                T::Ident,
                T::OpenCurly,
                T::Ident,
                T::Colon,
                T::Ident,
                T::CloseCurly
            ]
        );
        assert_eq!(kinds("url(a.png)"), vec![T::Url]);
        // A URL cut off by the end of the input is still a URL (with a warning).
        assert_eq!(kinds("url(a.png"), vec![T::Url]);
        assert_eq!(kinds("url(a.png  "), vec![T::Url]);
        assert_eq!(kinds("url(a b)"), vec![T::BadUrl]);
        assert_eq!(
            kinds("url( \"a\" )"),
            vec![T::Function, T::Ws, T::Str, T::Ws, T::CloseParen]
        );
        assert_eq!(kinds("#a #1"), vec![T::IdHash, T::Ws, T::Hash]);
        assert_eq!(
            kinds("10px 5% .5 -x"),
            vec![
                T::Dimension,
                T::Ws,
                T::Percentage,
                T::Ws,
                T::Number,
                T::Ws,
                T::Ident
            ]
        );
        assert_eq!(kinds("@media/*c*/x"), vec![T::AtKeyword, T::Ident]);
    }

    #[test]
    fn unterminated_input_ends() {
        for s in ["\"abc", "url(abc", "/* x", "\\", "#", "@", "'\\", "url(a b"] {
            let (toks, _) = tokenize(s);
            assert!(toks.iter().all(|t| t.end as usize <= s.len()));
        }
    }

    #[test]
    fn values() {
        assert_eq!(url_value("url( a\\29 b.png )"), "a)b.png");
        assert_eq!(string_value("'a\\'b'"), "a'b");
        assert_eq!(quote("a\"b"), "'a\"b'");
        assert_eq!(unescape("\\31 23"), "123");
    }
}
