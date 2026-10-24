//! Lexer for TS, TSX and JS.
//!
//! The lexer is pull-based: the parser calls [`Lexer::next_token`] and reads [`Lexer::token`].
//! Context-dependent tokens are produced by explicit re-scans driven by the parser:
//!
//! - `>` is always scanned alone; [`Lexer::rescan_greater`] merges `>=`, `>>`, `>>>=`, ...
//!   in expression contexts (so `Array<Array<T>>` needs no splitting).
//! - `/` and `/=` become a regex with [`Lexer::rescan_slash_as_regex`].
//! - `}` continues a template with [`Lexer::rescan_template_continuation`].
//! - JSX children, attribute strings and dashed names use the `jsx_*` methods.
//!
//! Scanning is byte-oriented with `memchr` for comments, strings and templates. Tokens hold
//! only a kind, flags and a byte range; values (identifier names, cooked strings, numbers) are
//! decoded on request without allocating unless the token contains escapes.

mod chars;
mod cook;
mod regex;
mod regex_props;
mod token;

use std::borrow::Cow;

use memchr::{memchr, memchr2, memchr3};
use ts_r_ast::{Comment, CommentKind};
use ts_r_diagnostics::{Diagnostic, DiagnosticMessage, messages};
use ts_r_span::Span;

pub use chars::{is_id_part, is_id_start, is_line_terminator};
pub use cook::parse_number;
pub use token::{Token, TokenFlags, TokenKind, keyword_kind};

/// Saved lexer state for lookahead and speculative parsing.
#[derive(Clone, Copy, Debug)]
pub struct LexerCheckpoint {
    pos: u32,
    tok: Token,
    comments: u32,
    diags: u32,
}

/// The lexer. Holds only the current token; lookahead is done with checkpoints.
#[derive(Debug)]
pub struct Lexer<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
    tok: Token,
    /// Start of the token being scanned.
    scan_start: usize,
    /// Comments seen so far, in source order.
    pub comments: Vec<Comment>,
    /// Lexical errors, in source order.
    pub diagnostics: Vec<Diagnostic>,
}

const NL: u8 = TokenFlags::NEWLINE_BEFORE;

impl<'a> Lexer<'a> {
    /// Creates a lexer positioned before the first token. A leading `#!` line is recorded as a
    /// hashbang comment. Call [`Lexer::next_token`] to scan the first token.
    pub fn new(src: &'a str) -> Lexer<'a> {
        let mut lx = Lexer {
            src,
            bytes: src.as_bytes(),
            pos: 0,
            tok: Token::default(),
            scan_start: 0,
            comments: Vec::new(),
            diagnostics: Vec::new(),
        };
        if lx.bytes.starts_with(b"\xEF\xBB\xBF") {
            lx.pos = 3;
        }
        if lx.bytes[lx.pos..].starts_with(b"#!") {
            let start = lx.pos;
            lx.pos = lx.find_line_end(start);
            lx.comments.push(Comment {
                span: Span::new(start as u32, lx.pos as u32),
                kind: CommentKind::Hashbang,
                newline_before: false,
            });
        }
        lx
    }

    #[inline]
    pub fn source(&self) -> &'a str {
        self.src
    }

    /// The current token.
    #[inline]
    pub fn token(&self) -> Token {
        self.tok
    }

    #[inline]
    pub fn kind(&self) -> TokenKind {
        self.tok.kind
    }

    /// Byte position just after the current token.
    #[inline]
    pub fn pos(&self) -> u32 {
        self.pos as u32
    }

    /// Text of the current token.
    #[inline]
    pub fn token_text(&self) -> &'a str {
        self.text(self.tok.span())
    }

    #[inline]
    pub fn text(&self, span: Span) -> &'a str {
        self.src
            .get(span.start as usize..span.end as usize)
            .unwrap_or("")
    }

    pub fn checkpoint(&self) -> LexerCheckpoint {
        LexerCheckpoint {
            pos: self.pos as u32,
            tok: self.tok,
            comments: self.comments.len() as u32,
            diags: self.diagnostics.len() as u32,
        }
    }

    /// Moves the lexer to byte `pos` (clamped to the source, moved back to a character
    /// boundary) before any token; call [`Lexer::next_token`] next. Used to parse pieces
    /// of a file out of order (JSDoc type expressions inside comments).
    pub fn seek(&mut self, pos: u32) {
        let mut p = (pos as usize).min(self.src.len());
        while !self.src.is_char_boundary(p) {
            p -= 1;
        }
        self.pos = p;
        self.tok = Token::default();
    }

    pub fn rewind(&mut self, cp: LexerCheckpoint) {
        self.pos = cp.pos as usize;
        self.tok = cp.tok;
        self.comments.truncate(cp.comments as usize);
        self.diagnostics.truncate(cp.diags as usize);
    }

    fn error(&mut self, msg: DiagnosticMessage, start: usize, end: usize, args: &[&str]) {
        let span = Span::new(start as u32, end as u32);
        if self
            .diagnostics
            .last()
            .is_some_and(|d| d.span.start == span.start)
        {
            return;
        }
        self.diagnostics.push(Diagnostic::new(msg, span, args));
    }

    #[inline]
    fn peek_byte(&self, off: usize) -> u8 {
        self.bytes.get(self.pos + off).copied().unwrap_or(0)
    }

    #[inline]
    fn char_at(&self, pos: usize) -> Option<char> {
        self.src.get(pos..).and_then(|s| s.chars().next())
    }

    /// Position of the next line terminator at or after `from`, or the end of input.
    fn find_line_end(&self, from: usize) -> usize {
        let mut i = from;
        loop {
            match memchr3(b'\n', b'\r', 0xE2, &self.bytes[i..]) {
                None => return self.bytes.len(),
                Some(off) => {
                    let j = i + off;
                    if self.bytes[j] != 0xE2 || self.is_ls_ps(j) {
                        return j;
                    }
                    i = j + 1;
                }
            }
        }
    }

    /// U+2028 or U+2029 starts at `i`.
    #[inline]
    fn is_ls_ps(&self, i: usize) -> bool {
        self.bytes.get(i + 1) == Some(&0x80)
            && matches!(self.bytes.get(i + 2), Some(&0xA8) | Some(&0xA9))
    }

    /// Scans the next token in the default (non-JSX) mode.
    pub fn next_token(&mut self) -> TokenKind {
        let mut flags = self.skip_trivia();
        let start = self.pos;
        self.scan_start = start;
        let mut kind = self.scan(&mut flags);
        if self.pos == start && kind != TokenKind::Eof {
            // Backstop: every non-EOF token consumes input, so no caller can stall on it.
            self.pos += self.char_at(start).map_or(1, char::len_utf8);
            kind = TokenKind::Unknown;
        }
        self.tok = Token {
            kind,
            flags: TokenFlags(flags),
            start: start as u32,
            end: self.pos as u32,
        };
        kind
    }

    /// Skips whitespace and comments. Returns `NEWLINE_BEFORE` if a line break was seen.
    fn skip_trivia(&mut self) -> u8 {
        let mut flags = 0u8;
        let len = self.bytes.len();
        while self.pos < len {
            let b = self.bytes[self.pos];
            match b {
                b' ' | b'\t' | 0x0B | 0x0C => self.pos += 1,
                b'\n' | b'\r' => {
                    flags |= NL;
                    self.pos += 1;
                }
                b'/' => match self.peek_byte(1) {
                    b'/' => {
                        let start = self.pos;
                        self.pos = self.find_line_end(start + 2);
                        self.comments.push(Comment {
                            span: Span::new(start as u32, self.pos as u32),
                            kind: CommentKind::Line,
                            newline_before: flags & NL != 0,
                        });
                    }
                    b'*' => {
                        let start = self.pos;
                        let body = &self.bytes[start + 2..];
                        let end = match memchr::memmem::find(body, b"*/") {
                            Some(off) => start + 2 + off + 2,
                            None => {
                                self.error(messages::ASTERISK_SLASH_EXPECTED, len, len, &[]);
                                len
                            }
                        };
                        let inner = &self.bytes[start + 2..end.min(len)];
                        let had_nl = flags & NL != 0;
                        if memchr2(b'\n', b'\r', inner).is_some()
                            || (memchr(0xE2, inner).is_some()
                                && (start + 2..end)
                                    .any(|i| self.bytes[i] == 0xE2 && self.is_ls_ps(i)))
                        {
                            flags |= NL;
                        }
                        self.comments.push(Comment {
                            span: Span::new(start as u32, end as u32),
                            kind: CommentKind::Block,
                            newline_before: had_nl,
                        });
                        self.pos = end;
                    }
                    _ => break,
                },
                b'<' | b'=' | b'>' | b'|' if self.is_conflict_marker(self.pos) => {
                    self.skip_conflict_marker();
                }
                0xC2 | 0xE1 | 0xE2 | 0xE3 | 0xEF => {
                    let Some(c) = self.char_at(self.pos) else {
                        break;
                    };
                    if c == '\u{2028}' || c == '\u{2029}' {
                        flags |= NL;
                        self.pos += 3;
                    } else if chars::is_unicode_whitespace(c) {
                        self.pos += c.len_utf8();
                    } else {
                        break;
                    }
                }
                _ => break,
            }
        }
        flags
    }

    /// A merge conflict marker starts at `i` (observed in tsc 7): seven `<`, `|`, `=` or `>`
    /// with only whitespace before them on their line, followed by a space (`=======` by any
    /// character).
    fn is_conflict_marker(&self, i: usize) -> bool {
        let b = self.bytes;
        let Some(&c) = b.get(i) else {
            return false;
        };
        if b.get(i + 1) != Some(&c) || b.get(i..i + 7).is_none_or(|m| m.iter().any(|&x| x != c)) {
            return false;
        }
        let next_ok = match b.get(i + 7) {
            None => false,
            Some(&n) => c == b'=' || n == b' ',
        };
        if !next_ok {
            return false;
        }
        let mut j = i;
        while j > 0 && matches!(b[j - 1], b' ' | b'\t' | 0x0B | 0x0C) {
            j -= 1;
        }
        j == 0 || matches!(b[j - 1], b'\n' | b'\r')
    }

    /// Skips a conflict marker at `self.pos` (TS1185 on its seven characters): the rest of
    /// a `<<<<<<<` or `>>>>>>>` line, and everything after `|||||||` up to the next
    /// `=======` or `>>>>>>>` marker, or after `=======` up to the next `>>>>>>>` marker.
    fn skip_conflict_marker(&mut self) {
        let start = self.pos;
        let c = self.bytes[start];
        self.error(messages::MERGE_CONFLICT_MARKER, start, start + 7, &[]);
        let len = self.bytes.len();
        if c == b'<' || c == b'>' {
            self.pos = self.find_line_end(start + 7);
            return;
        }
        let mut i = self.find_line_end(start + 7);
        while i < len {
            // `i` is at a line terminator; look at the next line's first non-blank.
            i += 1;
            while i < len && matches!(self.bytes[i], b' ' | b'\t' | 0x0B | 0x0C) {
                i += 1;
            }
            let m = self.bytes.get(i).copied().unwrap_or(0);
            if (m == b'>' || (c == b'|' && m == b'=')) && self.is_conflict_marker(i) {
                self.pos = i;
                return;
            }
            i = self.find_line_end(i);
        }
        self.pos = len;
    }

    /// Scans one token starting at `self.pos` (trivia already skipped).
    fn scan(&mut self, flags: &mut u8) -> TokenKind {
        use TokenKind as T;
        let Some(&b) = self.bytes.get(self.pos) else {
            return T::Eof;
        };
        let p1 = self.peek_byte(1);
        let p2 = self.peek_byte(2);
        macro_rules! op {
            ($n:expr, $k:expr) => {{
                self.pos += $n;
                $k
            }};
        }
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'_' | b'$' => self.scan_identifier(flags),
            b'0'..=b'9' => self.scan_number(flags),
            b'"' | b'\'' => self.scan_string(b, flags),
            b'`' => {
                self.pos += 1;
                self.scan_template_part(flags, true)
            }
            b'{' => op!(1, T::LBrace),
            b'}' => op!(1, T::RBrace),
            b'(' => op!(1, T::LParen),
            b')' => op!(1, T::RParen),
            b'[' => op!(1, T::LBracket),
            b']' => op!(1, T::RBracket),
            b';' => op!(1, T::Semi),
            b',' => op!(1, T::Comma),
            b':' => op!(1, T::Colon),
            b'~' => op!(1, T::Tilde),
            b'@' => op!(1, T::At),
            b'.' => {
                if p1.is_ascii_digit() {
                    self.scan_number(flags)
                } else if p1 == b'.' && p2 == b'.' {
                    op!(3, T::DotDotDot)
                } else {
                    op!(1, T::Dot)
                }
            }
            b'<' => match (p1, p2) {
                (b'<', b'=') => op!(3, T::LtLtEq),
                (b'<', _) => op!(2, T::LtLt),
                (b'=', _) => op!(2, T::LtEq),
                _ => op!(1, T::Lt),
            },
            b'>' => op!(1, T::Gt),
            b'=' => match (p1, p2) {
                (b'=', b'=') => op!(3, T::EqEqEq),
                (b'=', _) => op!(2, T::EqEq),
                (b'>', _) => op!(2, T::Arrow),
                _ => op!(1, T::Eq),
            },
            b'!' => match (p1, p2) {
                (b'=', b'=') => op!(3, T::NotEqEq),
                (b'=', _) => op!(2, T::NotEq),
                _ => op!(1, T::Bang),
            },
            b'+' => match p1 {
                b'+' => op!(2, T::PlusPlus),
                b'=' => op!(2, T::PlusEq),
                _ => op!(1, T::Plus),
            },
            b'-' => match p1 {
                b'-' => op!(2, T::MinusMinus),
                b'=' => op!(2, T::MinusEq),
                _ => op!(1, T::Minus),
            },
            b'*' => match (p1, p2) {
                (b'*', b'=') => op!(3, T::StarStarEq),
                (b'*', _) => op!(2, T::StarStar),
                (b'=', _) => op!(2, T::StarEq),
                _ => op!(1, T::Star),
            },
            b'/' => match p1 {
                b'=' => op!(2, T::SlashEq),
                _ => op!(1, T::Slash),
            },
            b'%' => match p1 {
                b'=' => op!(2, T::PercentEq),
                _ => op!(1, T::Percent),
            },
            b'&' => match (p1, p2) {
                (b'&', b'=') => op!(3, T::AmpAmpEq),
                (b'&', _) => op!(2, T::AmpAmp),
                (b'=', _) => op!(2, T::AmpEq),
                _ => op!(1, T::Amp),
            },
            b'|' => match (p1, p2) {
                (b'|', b'=') => op!(3, T::PipePipeEq),
                (b'|', _) => op!(2, T::PipePipe),
                (b'=', _) => op!(2, T::PipeEq),
                _ => op!(1, T::Pipe),
            },
            b'^' => match p1 {
                b'=' => op!(2, T::CaretEq),
                _ => op!(1, T::Caret),
            },
            b'?' => match (p1, p2) {
                (b'?', b'=') => op!(3, T::QuestionQuestionEq),
                (b'?', _) => op!(2, T::QuestionQuestion),
                (b'.', d) if !d.is_ascii_digit() => op!(2, T::QuestionDot),
                _ => op!(1, T::Question),
            },
            b'#' => {
                if p1 == b'!' {
                    // Only the `#` is skipped: the rest of the line is scanned as usual
                    // (shebangError: `!/usr/bin/env node` is an expression).
                    self.error(messages::INVALID_HASHBANG, self.pos, self.pos + 2, &[]);
                    self.pos += 1;
                    return T::Unknown;
                }
                self.pos += 1;
                if self.at_identifier_start() {
                    self.scan_identifier_rest(flags, true);
                    T::PrivateName
                } else {
                    // A lone `#` is TS1127 and still a (nameless) private name, as in tsc 7:
                    // `class C { # m() {} }` is a property `#`, then `';' expected`.
                    self.error(messages::INVALID_CHARACTER, self.pos - 1, self.pos, &[]);
                    T::PrivateName
                }
            }
            b'\\' => {
                if self.at_identifier_start() {
                    self.scan_identifier_rest(flags, true);
                    self.finish_escaped_identifier(flags)
                } else {
                    self.invalid_char()
                }
            }
            0x80.. => {
                let c = self.char_at(self.pos).unwrap_or('\u{FFFD}');
                if chars::is_id_start(c) {
                    self.scan_identifier_rest(flags, true);
                    if *flags & TokenFlags::ESCAPED != 0 {
                        self.finish_escaped_identifier(flags)
                    } else {
                        T::Ident
                    }
                } else if c == '\u{FFFD}' {
                    self.binary_rest()
                } else {
                    self.invalid_char()
                }
            }
            _ => self.invalid_char(),
        }
    }

    /// U+FFFD outside a string, comment or template: as in tsc, the file is binary (TS1490
    /// at its start) and the rest of it is one unknown token, with no TS1127.
    fn binary_rest(&mut self) -> TokenKind {
        if !self.diagnostics.iter().any(|d| d.code == 1490) {
            self.diagnostics.push(Diagnostic::new(
                messages::FILE_APPEARS_TO_BE_BINARY,
                Span::at(0),
                &[],
            ));
        }
        self.pos = self.bytes.len();
        TokenKind::Unknown
    }

    fn invalid_char(&mut self) -> TokenKind {
        let start = self.pos;
        let len = self.char_at(start).map_or(1, |c| c.len_utf8());
        self.pos += len;
        self.error(messages::INVALID_CHARACTER, start, self.pos, &[]);
        TokenKind::Unknown
    }

    /// True if an identifier starts at `self.pos` (including a `\u` escape).
    fn at_identifier_start(&self) -> bool {
        match self.bytes.get(self.pos) {
            None => false,
            Some(&b) if b < 0x80 => {
                chars::is_ascii_ident_start(b)
                    || (b == b'\\'
                        && self.peek_byte(1) == b'u'
                        && {
                            let mut tmp = self.pos + 2;
                            matches!(cook::read_unicode_escape_body(self.bytes, &mut tmp), Some(c) if chars::is_id_start(c))
                        })
            }
            Some(_) => self.char_at(self.pos).is_some_and(chars::is_id_start),
        }
    }

    #[inline]
    fn scan_identifier(&mut self, flags: &mut u8) -> TokenKind {
        let start = self.pos;
        let mut i = self.pos + 1;
        let bytes = self.bytes;
        while i < bytes.len() && chars::is_ascii_ident_part(bytes[i]) {
            i += 1;
        }
        self.pos = i;
        if i < bytes.len() && (bytes[i] == b'\\' || bytes[i] >= 0x80) {
            self.scan_identifier_rest(flags, false);
            if *flags & TokenFlags::ESCAPED != 0 {
                return self.finish_escaped_identifier(flags);
            }
            // A keyword followed by non-ASCII whitespace (`return` + U+0085,
            // fileWithNextLine3) is still a keyword.
            return keyword_kind(&bytes[start..self.pos]);
        }
        keyword_kind(&bytes[start..i])
    }

    /// Continues an identifier at `self.pos`, handling escapes and non-ASCII characters.
    /// `first`: nothing of the identifier is consumed yet, so the next character must be an
    /// identifier start (false after `scan_identifier`'s ASCII prefix).
    fn scan_identifier_rest(&mut self, flags: &mut u8, mut first: bool) {
        loop {
            let Some(&b) = self.bytes.get(self.pos) else {
                break;
            };
            if b < 0x80 {
                if if first {
                    chars::is_ascii_ident_start(b)
                } else {
                    chars::is_ascii_ident_part(b)
                } {
                    self.pos += 1;
                } else if b == b'\\' {
                    let start = self.pos;
                    if self.peek_byte(1) != b'u' {
                        break;
                    }
                    let mut p = self.pos + 2;
                    match cook::read_unicode_escape_body(self.bytes, &mut p) {
                        Some(c)
                            if (if first {
                                chars::is_id_start(c)
                            } else {
                                chars::is_id_part(c)
                            }) =>
                        {
                            *flags |= TokenFlags::ESCAPED;
                            self.pos = p;
                        }
                        Some(_) => {
                            self.error(messages::INVALID_CHARACTER, start, p, &[]);
                            *flags |= TokenFlags::ESCAPED;
                            self.pos = p;
                        }
                        // Not an escape: the identifier ends before the `\`, which is then an
                        // invalid character (`arg\u003`, invalidUnicodeEscapeSequance*).
                        None => break,
                    }
                } else {
                    break;
                }
            } else {
                let Some(c) = self.char_at(self.pos) else {
                    break;
                };
                if if first {
                    chars::is_id_start(c)
                } else {
                    chars::is_id_part(c)
                } {
                    self.pos += c.len_utf8();
                } else {
                    break;
                }
            }
            first = false;
        }
    }

    fn finish_escaped_identifier(&mut self, flags: &mut u8) -> TokenKind {
        let text = &self.src[self.scan_start..self.pos];
        let decoded = cook::decode_identifier(text);
        // An escaped keyword (`v\u0061r`) is still the keyword, flagged: tsc reads it as the
        // keyword and reports TS1260 where it is used as one (not as a name).
        let kind = keyword_kind(decoded.as_bytes());
        if kind != TokenKind::Ident {
            *flags |= TokenFlags::ESCAPED_KEYWORD;
        }
        kind
    }

    fn scan_number(&mut self, flags: &mut u8) -> TokenKind {
        let start = self.pos;
        let b = self.bytes;
        let mut kind = TokenKind::Number;
        if b[start] == b'0' && matches!(self.peek_byte(1), b'x' | b'X' | b'o' | b'O' | b'b' | b'B')
        {
            let radix = match self.peek_byte(1) {
                b'x' | b'X' => 16,
                b'o' | b'O' => 8,
                _ => 2,
            };
            *flags |= TokenFlags::NON_DECIMAL;
            self.pos += 2;
            let n = self.scan_digits(radix, flags);
            if n == 0 {
                let msg = match radix {
                    16 => messages::HEXADECIMAL_DIGIT_EXPECTED,
                    8 => messages::OCTAL_DIGIT_EXPECTED,
                    _ => messages::BINARY_DIGIT_EXPECTED,
                };
                self.error(msg, self.pos, self.pos, &[]);
            }
            if self.peek_byte(0) == b'n' {
                self.pos += 1;
                kind = TokenKind::BigInt;
            }
        } else if b[start] == b'0' && self.peek_byte(1).is_ascii_digit() {
            // Legacy octal (`017`) or leading-zero decimal (`089`).
            self.pos += 1;
            let digits_start = self.pos;
            while self.peek_byte(0).is_ascii_digit() {
                self.pos += 1;
            }
            *flags |= TokenFlags::LEGACY_OCTAL;
            let is_decimal = b[digits_start..self.pos].iter().any(|&d| d >= b'8');
            if is_decimal {
                self.scan_fraction_and_exponent(flags);
            }
        } else {
            if b[start] == b'0' && self.peek_byte(1) == b'_' {
                // `0_1`: no separator after a leading zero (once; the rest scans as a
                // decimal literal, as in tsc 7).
                self.error(
                    messages::NUMERIC_SEPARATORS_NOT_ALLOWED_HERE,
                    start + 1,
                    start + 2,
                    &[],
                );
            }
            if b[start] != b'.' {
                self.scan_digits(10, flags);
            }
            let fractional = self.scan_fraction_and_exponent(flags);
            if self.peek_byte(0) == b'n' {
                self.pos += 1;
                if fractional {
                    // `1e2n`, `4.1n`: the `n` belongs to the (still numeric) literal, which
                    // is TS1352 or TS1353 on its whole span, as in tsc 7.
                    let msg = if b[start..self.pos].iter().any(|&c| matches!(c, b'e' | b'E')) {
                        messages::BIGINT_EXPONENTIAL
                    } else {
                        messages::BIGINT_NOT_INTEGER
                    };
                    self.error(msg, start, self.pos, &[]);
                } else {
                    kind = TokenKind::BigInt;
                }
            }
        }
        // An identifier may not directly follow a decimal literal (TS1351 on it, as in
        // tsc 7). It stays a token of its own: `3in x` is `3 in x`. Hex, octal, binary and
        // leading-zero literals are not checked (`0x1g` is `0x1`, then `g`).
        // A name starting with an escape is not checked: tsc 7 reports `';' expected` at it.
        if *flags & (TokenFlags::NON_DECIMAL | TokenFlags::LEGACY_OCTAL) == 0
            && self.peek_byte(0) != b'\\'
            && self.at_identifier_start()
        {
            let s = self.pos;
            let mut f = 0;
            self.scan_identifier_rest(&mut f, true);
            let end = self.pos;
            self.pos = s;
            self.error(messages::IDENTIFIER_AFTER_NUMERIC_LITERAL, s, end, &[]);
        }
        kind
    }

    /// Returns true if a `.` fraction or exponent was scanned.
    fn scan_fraction_and_exponent(&mut self, flags: &mut u8) -> bool {
        let mut fractional = false;
        if self.peek_byte(0) == b'.' {
            fractional = true;
            self.pos += 1;
            if self.peek_byte(0) == b'_' {
                self.error(
                    messages::NUMERIC_SEPARATORS_NOT_ALLOWED_HERE,
                    self.pos,
                    self.pos + 1,
                    &[],
                );
            }
            self.scan_digits(10, flags);
        }
        if matches!(self.peek_byte(0), b'e' | b'E') {
            let save = self.pos;
            self.pos += 1;
            if matches!(self.peek_byte(0), b'+' | b'-') {
                self.pos += 1;
            }
            if self.scan_digits(10, flags) == 0 {
                if self.at_identifier_start() || !self.peek_byte(0).is_ascii_digit() {
                    self.error(messages::DIGIT_EXPECTED, self.pos, self.pos, &[]);
                } else {
                    self.pos = save;
                }
            }
            fractional = true;
        }
        fractional
    }

    /// Scans digits of `radix` with `_` separators; returns how many digits were read.
    fn scan_digits(&mut self, radix: u32, flags: &mut u8) -> usize {
        let mut count = 0;
        let mut prev_sep = false;
        loop {
            let c = self.peek_byte(0);
            if c == b'_' {
                *flags |= TokenFlags::SEPARATOR;
                // Before the first digit every `_` is TS6188 (`0x___1`, tsc 7).
                if count == 0 {
                    self.error(
                        messages::NUMERIC_SEPARATORS_NOT_ALLOWED_HERE,
                        self.pos,
                        self.pos + 1,
                        &[],
                    );
                } else if prev_sep {
                    self.error(
                        messages::MULTIPLE_CONSECUTIVE_NUMERIC_SEPARATORS,
                        self.pos,
                        self.pos + 1,
                        &[],
                    );
                }
                prev_sep = true;
                self.pos += 1;
                continue;
            }
            if (c as char).is_digit(radix) {
                count += 1;
                prev_sep = false;
                self.pos += 1;
            } else {
                break;
            }
        }
        if prev_sep {
            self.error(
                messages::NUMERIC_SEPARATORS_NOT_ALLOWED_HERE,
                self.pos - 1,
                self.pos,
                &[],
            );
        }
        count
    }

    fn scan_string(&mut self, quote: u8, flags: &mut u8) -> TokenKind {
        self.pos += 1;
        let b = self.bytes;
        loop {
            let rest = &b[self.pos..];
            let Some(off) = memchr3(quote, b'\\', b'\n', rest) else {
                if let Some(r) = memchr(b'\r', rest) {
                    self.pos += r;
                } else {
                    self.pos = b.len();
                }
                *flags |= TokenFlags::UNTERMINATED;
                self.error(
                    messages::UNTERMINATED_STRING_LITERAL,
                    self.pos,
                    self.pos,
                    &[],
                );
                break;
            };
            if let Some(r) = memchr(b'\r', &rest[..off]) {
                self.pos += r;
                *flags |= TokenFlags::UNTERMINATED;
                self.error(
                    messages::UNTERMINATED_STRING_LITERAL,
                    self.pos,
                    self.pos,
                    &[],
                );
                break;
            }
            self.pos += off;
            match b[self.pos] {
                b'\\' => {
                    *flags |= TokenFlags::ESCAPED;
                    self.pos += 1;
                    match b.get(self.pos) {
                        None => {
                            // `"\` at the end of the file: TS1126, not TS1002 (tsc 7).
                            *flags |= TokenFlags::UNTERMINATED;
                            self.error(messages::UNEXPECTED_END_OF_TEXT, self.pos, self.pos, &[]);
                            break;
                        }
                        Some(b'\r') if b.get(self.pos + 1) == Some(&b'\n') => self.pos += 2,
                        Some(&c) => self.pos += utf8_len(c),
                    }
                }
                b'\n' => {
                    *flags |= TokenFlags::UNTERMINATED;
                    self.error(
                        messages::UNTERMINATED_STRING_LITERAL,
                        self.pos,
                        self.pos,
                        &[],
                    );
                    break;
                }
                _ => {
                    self.pos += 1;
                    break;
                }
            }
        }
        TokenKind::String
    }

    /// Scans template characters after `` ` `` or `}` up to `` ` `` or `${`.
    fn scan_template_part(&mut self, flags: &mut u8, head: bool) -> TokenKind {
        let b = self.bytes;
        loop {
            let Some(off) = memchr3(b'`', b'$', b'\\', &b[self.pos..]) else {
                self.pos = b.len();
                *flags |= TokenFlags::UNTERMINATED;
                self.error(
                    messages::UNTERMINATED_TEMPLATE_LITERAL,
                    b.len(),
                    b.len(),
                    &[],
                );
                return if head {
                    TokenKind::NoSubstTemplate
                } else {
                    TokenKind::TemplateTail
                };
            };
            self.pos += off;
            match b[self.pos] {
                b'`' => {
                    self.pos += 1;
                    return if head {
                        TokenKind::NoSubstTemplate
                    } else {
                        TokenKind::TemplateTail
                    };
                }
                b'$' => {
                    self.pos += 1;
                    if self.peek_byte(0) == b'{' {
                        self.pos += 1;
                        return if head {
                            TokenKind::TemplateHead
                        } else {
                            TokenKind::TemplateMiddle
                        };
                    }
                }
                _ => {
                    *flags |= TokenFlags::ESCAPED;
                    self.pos += 1;
                    let Some(&c) = b.get(self.pos) else {
                        // `\`\` at the end of the file: TS1126, not TS1160 (tsc 7).
                        *flags |= TokenFlags::UNTERMINATED;
                        self.error(messages::UNEXPECTED_END_OF_TEXT, self.pos, self.pos, &[]);
                        return if head {
                            TokenKind::NoSubstTemplate
                        } else {
                            TokenKind::TemplateTail
                        };
                    };
                    self.pos += utf8_len(c);
                }
            }
        }
    }

    // ---- Parser-driven re-scans ----

    /// If the current token is `>`, merges it with directly following `>` and `=` characters
    /// into `>=`, `>>`, `>>=`, `>>>` or `>>>=`.
    pub fn rescan_greater(&mut self) -> TokenKind {
        if self.tok.kind != TokenKind::Gt {
            return self.tok.kind;
        }
        let s = self.tok.start as usize;
        let at = |i: usize| self.bytes.get(s + i).copied().unwrap_or(0);
        let (kind, len) = match (at(1), at(2), at(3)) {
            (b'>', b'>', b'=') => (TokenKind::GtGtGtEq, 4),
            (b'>', b'>', _) => (TokenKind::GtGtGt, 3),
            (b'>', b'=', _) => (TokenKind::GtGtEq, 3),
            (b'>', _, _) => (TokenKind::GtGt, 2),
            (b'=', _, _) => (TokenKind::GtEq, 2),
            _ => (TokenKind::Gt, 1),
        };
        self.tok.kind = kind;
        self.tok.end = (s + len) as u32;
        self.pos = s + len;
        kind
    }

    /// Splits a `<<` / `<<=` / `<=` token so the current token is a single `<` (for generic
    /// argument lists such as `f<<T>(x: T) => T>()`).
    pub fn split_less_than(&mut self) {
        if matches!(
            self.tok.kind,
            TokenKind::LtLt | TokenKind::LtLtEq | TokenKind::LtEq
        ) {
            self.tok.kind = TokenKind::Lt;
            self.tok.end = self.tok.start + 1;
            self.pos = self.tok.end as usize;
        }
    }

    /// Splits a `*=` token so the current token is `*` (the JSDoc type `*=`: any, optional).
    pub fn split_star_eq(&mut self) {
        if self.tok.kind == TokenKind::StarEq {
            self.tok.kind = TokenKind::Star;
            self.tok.end = self.tok.start + 1;
            self.pos = self.tok.end as usize;
        }
    }

    /// Re-scans the current `/` or `/=` token as a regular expression literal.
    pub fn rescan_slash_as_regex(&mut self) -> TokenKind {
        if !matches!(self.tok.kind, TokenKind::Slash | TokenKind::SlashEq) {
            return self.tok.kind;
        }
        let start = self.tok.start as usize;
        let b = self.bytes;
        let mut i = start + 1;
        let mut in_class = false;
        let mut terminated = false;
        while i < b.len() {
            let c = b[i];
            match c {
                b'\\' => {
                    i += 1;
                    match b.get(i) {
                        None | Some(b'\n') | Some(b'\r') => break,
                        Some(&c) => i += utf8_len(c),
                    }
                    continue;
                }
                b'\n' | b'\r' => break,
                0xE2 if self.is_ls_ps(i) => break,
                b'[' => in_class = true,
                b']' => in_class = false,
                b'/' if !in_class => {
                    i += 1;
                    terminated = true;
                    break;
                }
                _ => {}
            }
            i += 1;
        }
        let mut flags = self.tok.flags.0;
        if !terminated {
            flags |= TokenFlags::UNTERMINATED;
            self.error(messages::UNTERMINATED_REGEX_LITERAL, start, i, &[]);
            i = unterminated_regex_end(b, start + 1, i);
        } else {
            let pattern_end = i - 1;
            let mut accepted = 0u8;
            while let Some(c) = self.src.get(i..).and_then(|rest| rest.chars().next()) {
                if !chars::is_id_part(c) {
                    break;
                }
                self.check_regex_flag(c, i, &mut accepted);
                i += c.len_utf8();
            }
            for e in regex::validate(self.src, start + 1, pattern_end, accepted) {
                let args: &[&str] = if e.arg.is_empty() { &[] } else { &[&e.arg] };
                let span = Span::new(e.start as u32, e.end as u32);
                self.diagnostics.push(Diagnostic::new(e.msg, span, args));
            }
        }
        self.pos = i;
        self.tok = Token {
            kind: TokenKind::Regex,
            flags: TokenFlags(flags),
            start: start as u32,
            end: i as u32,
        };
        TokenKind::Regex
    }

    /// Re-scans the current `}` as the continuation of a template literal, producing
    /// `TemplateMiddle` or `TemplateTail`.
    pub fn rescan_template_continuation(&mut self) -> TokenKind {
        if self.tok.kind != TokenKind::RBrace {
            return self.tok.kind;
        }
        let start = self.tok.start;
        self.pos = start as usize + 1;
        let mut flags = self.tok.flags.0 & NL;
        let kind = self.scan_template_part(&mut flags, false);
        self.tok = Token {
            kind,
            flags: TokenFlags(flags),
            start,
            end: self.pos as u32,
        };
        kind
    }

    // ---- JSX ----

    /// Scans JSX child content starting right after the previous token: `<`, `{`, end of file,
    /// or a `JsxText` run up to the next `<` or `{`.
    pub fn next_jsx_child(&mut self) -> TokenKind {
        let start = self.pos;
        let b = self.bytes;
        let kind = match b.get(start) {
            None => TokenKind::Eof,
            // A `<<<<<<<` conflict marker in JSX children: TS1185, and the marker line is
            // one `Unknown` token (the parser reports the missing closing tag on it).
            Some(b'<') if self.is_conflict_marker(start) => {
                self.skip_conflict_marker();
                TokenKind::Unknown
            }
            Some(b'<') => {
                self.pos += 1;
                TokenKind::Lt
            }
            Some(b'{') => {
                self.pos += 1;
                TokenKind::LBrace
            }
            Some(_) => {
                let end = memchr2(b'<', b'{', &b[start..]).map_or(b.len(), |o| start + o);
                for (i, &c) in b[start..end].iter().enumerate() {
                    if c == b'>' {
                        self.error(
                            messages::UNEXPECTED_TOKEN_GT_IN_JSX,
                            start + i,
                            start + i + 1,
                            &[],
                        );
                    } else if c == b'}' {
                        self.error(
                            messages::UNEXPECTED_TOKEN_RBRACE_IN_JSX,
                            start + i,
                            start + i + 1,
                            &[],
                        );
                    }
                }
                self.pos = end;
                TokenKind::JsxText
            }
        };
        self.tok = Token {
            kind,
            flags: TokenFlags(0),
            start: start as u32,
            end: self.pos as u32,
        };
        kind
    }

    /// Extends the current identifier or keyword token with JSX name characters (`-`), e.g.
    /// `data-id` or `aria-label`. The result is an `Ident`.
    pub fn rescan_jsx_identifier(&mut self) -> TokenKind {
        if !self.tok.kind.is_identifier_or_keyword() {
            return self.tok.kind;
        }
        let b = self.bytes;
        let mut i = self.tok.end as usize;
        let mut extended = false;
        loop {
            match b.get(i) {
                Some(b'-') => {
                    extended = true;
                    i += 1;
                }
                Some(&c) if chars::is_ascii_ident_part(c) => i += 1,
                Some(&c) if c >= 0x80 => match self.char_at(i) {
                    Some(ch) if chars::is_id_part(ch) => i += ch.len_utf8(),
                    _ => break,
                },
                _ => break,
            }
        }
        if extended || i != self.tok.end as usize {
            self.tok.end = i as u32;
            self.pos = i;
            self.tok.kind = TokenKind::Ident;
        } else if self.tok.kind != TokenKind::Ident {
            self.tok.kind = TokenKind::Ident;
        }
        self.tok.kind
    }

    /// Scans the next token in a JSX attribute value position: quoted strings have no escapes
    /// and may span lines.
    pub fn next_jsx_attribute_value(&mut self) -> TokenKind {
        let flags = self.skip_trivia();
        let start = self.pos;
        match self.bytes.get(start) {
            Some(&q @ (b'"' | b'\'')) => {
                let mut f = flags;
                let end = match memchr(q, &self.bytes[start + 1..]) {
                    Some(o) => start + 1 + o + 1,
                    None => {
                        f |= TokenFlags::UNTERMINATED;
                        self.error(
                            messages::UNTERMINATED_STRING_LITERAL,
                            self.bytes.len(),
                            self.bytes.len(),
                            &[],
                        );
                        self.bytes.len()
                    }
                };
                self.pos = end;
                self.tok = Token {
                    kind: TokenKind::String,
                    flags: TokenFlags(f),
                    start: start as u32,
                    end: end as u32,
                };
                TokenKind::String
            }
            _ => {
                let mut f = flags;
                self.scan_start = start;
                let kind = self.scan(&mut f);
                self.tok = Token {
                    kind,
                    flags: TokenFlags(f),
                    start: start as u32,
                    end: self.pos as u32,
                };
                kind
            }
        }
    }

    // ---- Values ----

    /// The name of an identifier, keyword or private name token (escapes decoded; the `#` of a
    /// private name is included).
    pub fn ident_value(&self, tok: Token) -> Cow<'a, str> {
        let text = self.text(tok.span());
        if tok.flags.has(TokenFlags::ESCAPED) {
            Cow::Owned(cook::decode_identifier(text))
        } else {
            Cow::Borrowed(text)
        }
    }

    /// The cooked value of a string token. Invalid escapes are reported once per call.
    pub fn string_value(&mut self, tok: Token) -> Cow<'a, str> {
        let s = tok.start as usize + 1;
        let mut e = tok.end as usize;
        if !tok.flags.has(TokenFlags::UNTERMINATED) && e > s {
            e -= 1;
        }
        let e = e.max(s);
        let inner = self.src.get(s..e).unwrap_or("");
        if !tok.flags.has(TokenFlags::ESCAPED) {
            return Cow::Borrowed(inner);
        }
        match cook::cook(inner) {
            Ok(v) => Cow::Owned(v),
            Err((v, errors)) => {
                for err in errors {
                    let at = (s + err.at).min(self.bytes.len());
                    let end = (s + err.end).clamp(at, self.bytes.len());
                    // tsc scans the escape first: at the end of an unterminated string the
                    // escape's error wins (unicodeExtendedEscapesInStrings24).
                    if self
                        .diagnostics
                        .last()
                        .is_some_and(|d| d.span.start as usize == at && d.code == 1002)
                    {
                        self.diagnostics.pop();
                    }
                    match &err.arg {
                        Some(arg) => self.error(err.msg, at, end, &[arg]),
                        None => self.error(err.msg, at, end, &[]),
                    }
                }
                Cow::Owned(v)
            }
        }
    }

    /// Reports unknown (TS1499), duplicate (TS1500) and `u`+`v` (TS1502) regex flags, and
    /// accepts the others (with TS1501, which the checker keeps only for older targets).
    fn check_regex_flag(&mut self, c: char, at: usize, accepted: &mut u8) {
        let end = at + c.len_utf8();
        let Some(bit) = regex::flag_bit(c) else {
            self.error(messages::UNKNOWN_REGEX_FLAG, at, end, &[]);
            return;
        };
        let unicode = regex::FLAG_U | regex::FLAG_V;
        if *accepted & bit != 0 {
            self.error(messages::DUPLICATE_REGEX_FLAG, at, end, &[]);
        } else if (*accepted | bit) & unicode == unicode {
            self.error(messages::REGEX_UNICODE_AND_SETS_FLAGS, at, end, &[]);
        } else {
            *accepted |= bit;
            if let Some(target) = regex::flag_target(bit) {
                self.error(messages::REGEX_FLAG_NEEDS_TARGET, at, end, &[target]);
            }
        }
    }

    /// Byte span of a template part's raw text (without `` ` ``, `${` and `}`).
    pub fn template_raw_span(&self, tok: Token) -> Span {
        let start = tok.start + 1;
        let mut end = tok.end;
        if !tok.flags.has(TokenFlags::UNTERMINATED) {
            end -= match tok.kind {
                TokenKind::TemplateHead | TokenKind::TemplateMiddle => 2,
                _ => 1,
            };
        }
        Span::new(start, end.max(start))
    }

    /// The cooked value of a template part. An invalid escape yields the error and the span
    /// of the offending character; the caller decides whether to report it (tagged
    /// templates allow invalid escapes).
    pub fn template_cooked(&self, tok: Token) -> Result<Cow<'a, str>, (Span, DiagnosticMessage)> {
        let span = self.template_raw_span(tok);
        let raw = self.text(span);
        if !tok.flags.has(TokenFlags::ESCAPED) && !raw.contains('\r') {
            return Ok(Cow::Borrowed(raw));
        }
        match cook::cook(raw) {
            Ok(v) => Ok(Cow::Owned(v)),
            Err((_, errors)) => {
                let d = Self::escape_diagnostic(span, &errors[0]);
                Err((d.span, errors[0].msg))
            }
        }
    }

    /// Every invalid escape of a template part (TS1125, TS1198, TS1487, ...): an untagged
    /// template reports them all (unicodeExtendedEscapesInTemplates17).
    pub fn template_escape_errors(&self, tok: Token) -> Vec<Diagnostic> {
        let span = self.template_raw_span(tok);
        let raw = self.text(span);
        match cook::cook(raw) {
            Ok(_) => Vec::new(),
            Err((_, errors)) => errors
                .iter()
                .map(|e| Self::escape_diagnostic(span, e))
                .collect(),
        }
    }

    /// The diagnostic for an invalid escape in the template text at `span`.
    fn escape_diagnostic(span: Span, e: &cook::EscapeError) -> Diagnostic {
        let at = (span.start + e.at as u32).min(span.end);
        let end = (span.start + e.end as u32).min(span.end.max(at)).max(at);
        let span = Span::new(at, end);
        match &e.arg {
            Some(arg) => Diagnostic::new(e.msg, span, &[arg]),
            None => Diagnostic::new(e.msg, span, &[]),
        }
    }

    /// The numeric value of a `Number` token.
    pub fn number_value(&self, tok: Token) -> f64 {
        parse_number(self.text(tok.span()))
    }

    /// Pattern and flags spans of a `Regex` token.
    pub fn regex_parts(&self, tok: Token) -> (Span, Span) {
        let text = &self.bytes[tok.start as usize..tok.end as usize];
        let close = text
            .iter()
            .rposition(|&c| c == b'/')
            .filter(|&i| i > 0)
            .unwrap_or(text.len());
        let pattern = Span::new(tok.start + 1, tok.start + close as u32);
        let flags = Span::new((tok.start + close as u32 + 1).min(tok.end), tok.end);
        (pattern, flags)
    }

    /// Reports an error found by the parser through the lexer's list (used for errors tied to
    /// token values).
    pub fn report(&mut self, msg: DiagnosticMessage, span: Span, args: &[&str]) {
        self.error(msg, span.start as usize, span.end as usize, args);
    }
}

#[inline]
fn utf8_len(first: u8) -> usize {
    match first {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xFF => 4,
        _ => 1,
    }
}

/// Where an unterminated regular expression literal starting before `from` ends, as in
/// tsc: at the first `)`, `]` or `}` that closes nothing the pattern opened (outside a
/// character class), so `foo(/a);` keeps its `)` (parserRegularExpressionDivideAmbiguity4).
/// Otherwise at the end of the line.
fn unterminated_regex_end(b: &[u8], from: usize, line_end: usize) -> usize {
    let mut i = from;
    let (mut parens, mut braces) = (0u32, 0u32);
    let mut in_class = false;
    while i < line_end {
        match b[i] {
            b'\\' => i += 1,
            b']' if in_class => in_class = false,
            _ if in_class => {}
            b'[' => in_class = true,
            b'(' => parens += 1,
            b'{' => braces += 1,
            b')' if braces == 0 => {
                if parens == 0 {
                    return i;
                }
                parens -= 1;
            }
            b'}' => {
                if braces == 0 {
                    return i;
                }
                braces -= 1;
            }
            b']' => return i,
            _ => {}
        }
        i += 1;
    }
    // A trailing run of `;` (and blanks) is left out: `foo(/a b;` is `')' expected` at
    // the `;`.
    let mut end = line_end;
    while end > from && matches!(b[end - 1], b';' | b' ' | b'\t') {
        end -= 1;
    }
    if end < line_end && b[end..line_end].contains(&b';') {
        end
    } else {
        line_end
    }
}

#[cfg(test)]
mod tests;
