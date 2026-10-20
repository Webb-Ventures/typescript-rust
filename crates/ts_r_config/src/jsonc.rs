//! A from-scratch JSONC parser with tsc's leniency.
//!
//! Accepts `//` and `/* */` comments, trailing commas, a UTF-8 BOM, single-quoted strings,
//! hex/octal/binary numbers and unquoted keys (the last with a TS1327 error, as tsc does).
//! Every value keeps its byte span so later stages can point diagnostics at it. Malformed
//! input never panics: it produces diagnostics with tsc's codes and a best-effort tree.

use crate::diagnostic::ConfigDiagnostic;

/// A parsed JSON value with its source span.
#[derive(Debug, Clone, PartialEq)]
pub struct Value {
    /// The value itself.
    pub kind: ValueKind,
    /// `[start, end)` byte range of the value's tokens (no leading trivia).
    pub span: (u32, u32),
    /// Start including leading trivia (where tsc anchors some type errors).
    pub full_start: u32,
}

/// The kinds of JSON value.
#[derive(Debug, Clone, PartialEq)]
pub enum ValueKind {
    /// `null`
    Null,
    /// `true` / `false`
    Bool(bool),
    /// Any numeric literal (decimal, hex, octal, binary, optionally negated).
    Number(f64),
    /// A string literal (escapes decoded).
    String(String),
    /// `[...]`
    Array(Vec<Value>),
    /// `{...}` in source order. Duplicate keys are kept; lookups take the last.
    Object(Vec<Member>),
    /// Something that is not a JSON value (an identifier, `+1`, a template...). An error has
    /// already been reported.
    Invalid,
}

/// One `key: value` pair of an object.
#[derive(Debug, Clone, PartialEq)]
pub struct Member {
    /// Decoded key text.
    pub key: String,
    /// Span of the key token.
    pub key_span: (u32, u32),
    /// The value.
    pub value: Value,
}

impl Value {
    /// The members when this is an object.
    pub fn as_object(&self) -> Option<&[Member]> {
        match &self.kind {
            ValueKind::Object(m) => Some(m),
            _ => None,
        }
    }

    /// The elements when this is an array.
    pub fn as_array(&self) -> Option<&[Value]> {
        match &self.kind {
            ValueKind::Array(v) => Some(v),
            _ => None,
        }
    }

    /// The string when this is a string.
    pub fn as_str(&self) -> Option<&str> {
        match &self.kind {
            ValueKind::String(s) => Some(s),
            _ => None,
        }
    }

    /// The boolean when this is `true`/`false`.
    pub fn as_bool(&self) -> Option<bool> {
        match self.kind {
            ValueKind::Bool(b) => Some(b),
            _ => None,
        }
    }

    /// True for `null`.
    pub fn is_null(&self) -> bool {
        matches!(self.kind, ValueKind::Null)
    }

    /// Last member named `key` when this is an object.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.as_object()?
            .iter()
            .rev()
            .find(|m| m.key == key)
            .map(|m| &m.value)
    }

    /// Last member named `key` (with its key span) when this is an object.
    pub fn member(&self, key: &str) -> Option<&Member> {
        self.as_object()?.iter().rev().find(|m| m.key == key)
    }

    /// A short name for the value's type, used in messages.
    pub fn type_name(&self) -> &'static str {
        match self.kind {
            ValueKind::Null => "null",
            ValueKind::Bool(_) => "boolean",
            ValueKind::Number(_) => "number",
            ValueKind::String(_) => "string",
            ValueKind::Array(_) => "Array",
            ValueKind::Object(_) => "object",
            ValueKind::Invalid => "invalid",
        }
    }

    /// Converts to plain JSON text (compact). Used for plugin entries and debugging.
    pub fn to_json(&self) -> String {
        let mut out = String::new();
        write_json(self, &mut out);
        out
    }
}

fn write_json(v: &Value, out: &mut String) {
    match &v.kind {
        ValueKind::Null | ValueKind::Invalid => out.push_str("null"),
        ValueKind::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        ValueKind::Number(n) => out.push_str(&format_number(*n)),
        ValueKind::String(s) => write_json_string(s, out),
        ValueKind::Array(items) => {
            out.push('[');
            for (i, it) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_json(it, out);
            }
            out.push(']');
        }
        ValueKind::Object(members) => {
            out.push('{');
            for (i, m) in members.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_json_string(&m.key, out);
                out.push(':');
                write_json(&m.value, out);
            }
            out.push('}');
        }
    }
}

/// Formats a number the way JavaScript's `JSON.stringify` does for common values.
pub fn format_number(n: f64) -> String {
    if n.is_finite() && n.fract() == 0.0 && n.abs() < 1e21 {
        format!("{}", n as i64)
    } else if n.is_finite() {
        format!("{n}")
    } else {
        "null".to_owned()
    }
}

/// Writes `s` as a JSON string literal (quotes and escapes).
pub fn write_json_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Result of [`parse`].
#[derive(Debug, Clone)]
pub struct ParsedJson {
    /// The root value, `None` for an empty (or comment-only) document.
    pub root: Option<Value>,
    /// Syntax diagnostics, anchored in `file`.
    pub diagnostics: Vec<ConfigDiagnostic>,
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Colon,
    Comma,
    Minus,
    Plus,
    Str { value: String, single: bool },
    Num(f64),
    Ident(String),
    Template,
    Other,
    Eof,
}

struct Parser<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
    file: &'a str,
    diags: Vec<ConfigDiagnostic>,
    // current token
    tok: Tok,
    tok_start: usize,
    tok_end: usize,
    /// End of the previous token (start of the current token's leading trivia).
    prev_end: usize,
    depth: u32,
}

/// Parses JSONC text. `file` is used only to label diagnostics.
pub fn parse(src: &str, file: &str) -> ParsedJson {
    let mut p = Parser {
        src,
        bytes: src.as_bytes(),
        pos: 0,
        file,
        diags: Vec::new(),
        tok: Tok::Eof,
        tok_start: 0,
        tok_end: 0,
        prev_end: 0,
        depth: 0,
    };
    if src.starts_with('\u{feff}') {
        p.pos = 3;
    }
    p.prev_end = p.pos;
    p.next();
    if p.tok == Tok::Eof {
        return ParsedJson {
            root: None,
            diagnostics: p.diags,
        };
    }
    let root = p.parse_value();
    if p.tok != Tok::Eof {
        p.error_at(p.tok_start, p.tok_end, 1012, "Unexpected token.");
    }
    ParsedJson {
        root: Some(root),
        diagnostics: p.diags,
    }
}

impl<'a> Parser<'a> {
    fn error_at(&mut self, start: usize, end: usize, code: u32, msg: &str) {
        // Like tsc, a parse error at the same position as the previous error is dropped.
        let is_scanner_error = matches!(code, 1002 | 1010);
        if !is_scanner_error
            && self
                .diags
                .last()
                .and_then(|d| d.span)
                .is_some_and(|(s, _)| s as usize == start)
        {
            return;
        }
        self.diags.push(ConfigDiagnostic::at(
            code,
            msg,
            self.file,
            (start as u32, end as u32),
        ));
    }

    fn skip_trivia(&mut self) {
        loop {
            let Some(&b) = self.bytes.get(self.pos) else {
                return;
            };
            match b {
                b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c => self.pos += 1,
                b'/' if self.bytes.get(self.pos + 1) == Some(&b'/') => {
                    while let Some(&c) = self.bytes.get(self.pos) {
                        if c == b'\n' || c == b'\r' {
                            break;
                        }
                        self.pos += 1;
                    }
                }
                b'/' if self.bytes.get(self.pos + 1) == Some(&b'*') => {
                    let start = self.pos;
                    self.pos += 2;
                    let mut closed = false;
                    while self.pos < self.bytes.len() {
                        if self.bytes[self.pos] == b'*'
                            && self.bytes.get(self.pos + 1) == Some(&b'/')
                        {
                            self.pos += 2;
                            closed = true;
                            break;
                        }
                        self.pos += 1;
                    }
                    if !closed {
                        let end = self.bytes.len();
                        let _ = start;
                        self.error_at(end, end, 1010, "'*/' expected.");
                    }
                }
                _ => {
                    // Unicode whitespace (NBSP, BOM, line/paragraph separators...)
                    let ch = self.src[self.pos..].chars().next().unwrap_or('\0');
                    if ch.is_whitespace() || ch == '\u{feff}' {
                        self.pos += ch.len_utf8();
                    } else {
                        return;
                    }
                }
            }
        }
    }

    fn next(&mut self) {
        self.prev_end = self.tok_end.max(self.prev_end);
        self.skip_trivia();
        self.tok_start = self.pos;
        let Some(&b) = self.bytes.get(self.pos) else {
            self.tok = Tok::Eof;
            self.tok_end = self.pos;
            return;
        };
        self.tok = match b {
            b'{' => {
                self.pos += 1;
                Tok::LBrace
            }
            b'}' => {
                self.pos += 1;
                Tok::RBrace
            }
            b'[' => {
                self.pos += 1;
                Tok::LBracket
            }
            b']' => {
                self.pos += 1;
                Tok::RBracket
            }
            b':' => {
                self.pos += 1;
                Tok::Colon
            }
            b',' => {
                self.pos += 1;
                Tok::Comma
            }
            b'-' => {
                self.pos += 1;
                Tok::Minus
            }
            b'+' => {
                self.pos += 1;
                Tok::Plus
            }
            b'"' | b'\'' => self.scan_string(b),
            b'`' => {
                self.scan_template();
                Tok::Template
            }
            b'0'..=b'9' => self.scan_number(),
            b'.' if matches!(self.bytes.get(self.pos + 1), Some(b'0'..=b'9')) => self.scan_number(),
            _ => {
                let ch = self.src[self.pos..].chars().next().unwrap_or('\0');
                if ch == '_' || ch == '$' || ch.is_alphabetic() {
                    let start = self.pos;
                    while let Some(c) = self.src[self.pos..].chars().next() {
                        if c == '_' || c == '$' || c.is_alphanumeric() {
                            self.pos += c.len_utf8();
                        } else {
                            break;
                        }
                    }
                    Tok::Ident(self.src[start..self.pos].to_owned())
                } else {
                    self.pos += ch.len_utf8().max(1);
                    Tok::Other
                }
            }
        };
        self.tok_end = self.pos;
    }

    fn scan_template(&mut self) {
        self.pos += 1;
        while let Some(&c) = self.bytes.get(self.pos) {
            self.pos += 1;
            if c == b'\\' {
                self.pos += 1;
            } else if c == b'`' {
                return;
            }
        }
        self.pos = self.pos.min(self.bytes.len());
    }

    fn scan_string(&mut self, quote: u8) -> Tok {
        self.pos += 1;
        let mut value = String::new();
        loop {
            let Some(&c) = self.bytes.get(self.pos) else {
                let end = self.bytes.len();
                self.error_at(end, end, 1002, "Unterminated string literal.");
                break;
            };
            if c == quote {
                self.pos += 1;
                break;
            }
            if c == b'\n' || c == b'\r' {
                let p = self.pos;
                self.error_at(p, p, 1002, "Unterminated string literal.");
                break;
            }
            if c == b'\\' {
                self.pos += 1;
                self.scan_escape(&mut value);
                continue;
            }
            let ch = self.src[self.pos..].chars().next().unwrap_or('\u{fffd}');
            value.push(ch);
            self.pos += ch.len_utf8();
        }
        Tok::Str {
            value,
            single: quote == b'\'',
        }
    }

    fn scan_escape(&mut self, out: &mut String) {
        let Some(&c) = self.bytes.get(self.pos) else {
            return;
        };
        self.pos += 1;
        match c {
            b'n' => out.push('\n'),
            b't' => out.push('\t'),
            b'r' => out.push('\r'),
            b'b' => out.push('\u{8}'),
            b'f' => out.push('\u{c}'),
            b'v' => out.push('\u{b}'),
            b'0' if !matches!(self.bytes.get(self.pos), Some(b'0'..=b'9')) => out.push('\0'),
            b'x' => {
                if let Some(v) = self.hex_digits(2) {
                    out.push(char::from_u32(v).unwrap_or('\u{fffd}'));
                } else {
                    out.push('x');
                }
            }
            b'u' => {
                if self.bytes.get(self.pos) == Some(&b'{') {
                    let start = self.pos + 1;
                    let mut end = start;
                    while matches!(self.bytes.get(end), Some(b) if b.is_ascii_hexdigit()) {
                        end += 1;
                    }
                    if self.bytes.get(end) == Some(&b'}') && end > start {
                        let v = u32::from_str_radix(&self.src[start..end], 16).unwrap_or(0xfffd);
                        out.push(char::from_u32(v).unwrap_or('\u{fffd}'));
                        self.pos = end + 1;
                    } else {
                        out.push('u');
                    }
                } else if let Some(v) = self.hex_digits(4) {
                    // Combine surrogate pairs.
                    if (0xd800..0xdc00).contains(&v)
                        && self.bytes.get(self.pos) == Some(&b'\\')
                        && self.bytes.get(self.pos + 1) == Some(&b'u')
                    {
                        let save = self.pos;
                        self.pos += 2;
                        if let Some(lo) = self.hex_digits(4)
                            && (0xdc00..0xe000).contains(&lo)
                        {
                            let cp = 0x10000 + ((v - 0xd800) << 10) + (lo - 0xdc00);
                            out.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
                            return;
                        }
                        self.pos = save;
                    }
                    out.push(char::from_u32(v).unwrap_or('\u{fffd}'));
                } else {
                    out.push('u');
                }
            }
            b'\r' => {
                if self.bytes.get(self.pos) == Some(&b'\n') {
                    self.pos += 1;
                }
            }
            b'\n' => {}
            _ => {
                // Any other escaped character stands for itself.
                self.pos -= 1;
                let ch = self.src[self.pos..].chars().next().unwrap_or('\u{fffd}');
                out.push(ch);
                self.pos += ch.len_utf8();
            }
        }
    }

    fn hex_digits(&mut self, n: usize) -> Option<u32> {
        let end = self.pos + n;
        if end > self.bytes.len() {
            return None;
        }
        let s = &self.src.get(self.pos..end)?;
        if !s.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        self.pos = end;
        u32::from_str_radix(s, 16).ok()
    }

    fn scan_number(&mut self) -> Tok {
        let start = self.pos;
        let b = self.bytes;
        if b[start] == b'0'
            && matches!(
                b.get(start + 1),
                Some(b'x' | b'X' | b'o' | b'O' | b'b' | b'B')
            )
        {
            let radix = match b[start + 1] {
                b'x' | b'X' => 16,
                b'o' | b'O' => 8,
                _ => 2,
            };
            self.pos += 2;
            let ds = self.pos;
            while matches!(b.get(self.pos), Some(c) if c.is_ascii_alphanumeric() || *c == b'_') {
                self.pos += 1;
            }
            let digits: String = self.src[ds..self.pos]
                .chars()
                .filter(|c| *c != '_')
                .collect();
            let v = u64::from_str_radix(&digits, radix)
                .map(|v| v as f64)
                .unwrap_or(f64::NAN);
            return Tok::Num(v);
        }
        while matches!(b.get(self.pos), Some(c) if c.is_ascii_digit() || *c == b'_') {
            self.pos += 1;
        }
        if b.get(self.pos) == Some(&b'.') {
            self.pos += 1;
            while matches!(b.get(self.pos), Some(c) if c.is_ascii_digit() || *c == b'_') {
                self.pos += 1;
            }
        }
        if matches!(b.get(self.pos), Some(b'e' | b'E')) {
            let save = self.pos;
            self.pos += 1;
            if matches!(b.get(self.pos), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if matches!(b.get(self.pos), Some(c) if c.is_ascii_digit()) {
                while matches!(b.get(self.pos), Some(c) if c.is_ascii_digit()) {
                    self.pos += 1;
                }
            } else {
                self.pos = save;
            }
        }
        // BigInt suffix
        if b.get(self.pos) == Some(&b'n') {
            self.pos += 1;
        }
        let text: String = self.src[start..self.pos]
            .chars()
            .filter(|c| *c != '_' && *c != 'n')
            .collect();
        Tok::Num(text.parse::<f64>().unwrap_or(f64::NAN))
    }

    fn value_error(&mut self) -> Value {
        let full_start = self.prev_end as u32;
        let (s, e) = (self.tok_start, self.tok_end);
        self.error_at(
            s,
            e,
            1328,
            "Property value can only be string literal, numeric literal, 'true', 'false', 'null', object literal or array literal.",
        );
        Value {
            kind: ValueKind::Invalid,
            span: (s as u32, e as u32),
            full_start,
        }
    }

    fn parse_value(&mut self) -> Value {
        let full_start = self.prev_end as u32;
        let start = self.tok_start as u32;
        if self.depth > 256 {
            let v = self.value_error();
            self.next();
            return v;
        }
        match self.tok.clone() {
            Tok::LBrace => {
                self.depth += 1;
                let v = self.parse_object(full_start);
                self.depth -= 1;
                v
            }
            Tok::LBracket => {
                self.depth += 1;
                let v = self.parse_array(full_start);
                self.depth -= 1;
                v
            }
            Tok::Str { value, .. } => {
                let end = self.tok_end as u32;
                self.next();
                Value {
                    kind: ValueKind::String(value),
                    span: (start, end),
                    full_start,
                }
            }
            Tok::Num(n) => {
                let end = self.tok_end as u32;
                self.next();
                Value {
                    kind: ValueKind::Number(n),
                    span: (start, end),
                    full_start,
                }
            }
            Tok::Minus => {
                self.next();
                if let Tok::Num(n) = self.tok {
                    let end = self.tok_end as u32;
                    self.next();
                    Value {
                        kind: ValueKind::Number(-n),
                        span: (start, end),
                        full_start,
                    }
                } else {
                    let mut v = self.value_error();
                    v.full_start = full_start;
                    if !matches!(
                        self.tok,
                        Tok::Comma | Tok::RBrace | Tok::RBracket | Tok::Eof
                    ) {
                        self.next();
                    }
                    v
                }
            }
            Tok::Ident(id) => {
                let end = self.tok_end as u32;
                let kind = match id.as_str() {
                    "true" => ValueKind::Bool(true),
                    "false" => ValueKind::Bool(false),
                    "null" => ValueKind::Null,
                    _ => {
                        let v = self.value_error();
                        self.next();
                        return v;
                    }
                };
                self.next();
                Value {
                    kind,
                    span: (start, end),
                    full_start,
                }
            }
            Tok::Plus | Tok::Template | Tok::Other => {
                let v = self.value_error();
                self.next();
                // `+1`: swallow the operand too, like a prefix expression.
                if matches!(self.tok, Tok::Num(_) | Tok::Ident(_))
                    && self.tok_start == v.span.1 as usize
                {
                    self.next();
                }
                v
            }
            Tok::Colon | Tok::Comma | Tok::RBrace | Tok::RBracket | Tok::Eof => self.value_error(),
        }
    }

    fn parse_object(&mut self, full_start: u32) -> Value {
        let start = self.tok_start as u32;
        self.next(); // {
        let mut members = Vec::new();
        loop {
            match &self.tok {
                Tok::RBrace => {
                    let end = self.tok_end as u32;
                    self.next();
                    return Value {
                        kind: ValueKind::Object(members),
                        span: (start, end),
                        full_start,
                    };
                }
                Tok::Eof => {
                    let p = self.tok_start;
                    self.error_at(p, p, 1005, "'}' expected.");
                    return Value {
                        kind: ValueKind::Object(members),
                        span: (start, p as u32),
                        full_start,
                    };
                }
                Tok::Comma => {
                    let (s, e) = (self.tok_start, self.tok_end);
                    self.error_at(s, e, 1136, "Property assignment expected.");
                    self.next();
                    continue;
                }
                _ => {}
            }
            // key
            let key_start = self.tok_start as u32;
            let key_end = self.tok_end as u32;
            let key = match self.tok.clone() {
                Tok::Str { value, single } => {
                    if single {
                        self.error_at(
                            key_start as usize,
                            key_end as usize,
                            1327,
                            "String literal with double quotes expected.",
                        );
                    }
                    self.next();
                    value
                }
                Tok::Ident(id) => {
                    self.error_at(
                        key_start as usize,
                        key_end as usize,
                        1327,
                        "String literal with double quotes expected.",
                    );
                    self.next();
                    id
                }
                Tok::Num(n) => {
                    self.error_at(
                        key_start as usize,
                        key_end as usize,
                        1327,
                        "String literal with double quotes expected.",
                    );
                    self.next();
                    format_number(n)
                }
                _ => {
                    let (s, e) = (self.tok_start, self.tok_end);
                    self.error_at(s, e, 1136, "Property assignment expected.");
                    // Skip the offending token unless it closes an outer construct.
                    if matches!(self.tok, Tok::RBracket)
                        || !matches!(self.tok, Tok::LBrace | Tok::LBracket)
                    {
                        self.next();
                    } else {
                        // `{` or `[` where a key should be: parse and discard the value.
                        let _ = self.parse_value();
                    }
                    continue;
                }
            };
            // colon
            if self.tok == Tok::Colon {
                self.next();
            } else {
                let p = self.tok_start;
                self.error_at(p, p.max(self.tok_end), 1005, "':' expected.");
            }
            let value = self.parse_value();
            members.push(Member {
                key,
                key_span: (key_start, key_end),
                value,
            });
            // separator
            match &self.tok {
                Tok::Comma => self.next(),
                Tok::RBrace => {}
                Tok::Eof => {}
                Tok::Str { .. } | Tok::Ident(_) | Tok::Num(_) => {
                    let (s, e) = (self.tok_start, self.tok_end);
                    self.error_at(s, e, 1005, "',' expected.");
                }
                _ => {
                    let (s, e) = (self.tok_start, self.tok_end);
                    self.error_at(s, e, 1005, "',' expected.");
                    self.next();
                }
            }
        }
    }

    fn parse_array(&mut self, full_start: u32) -> Value {
        let start = self.tok_start as u32;
        self.next(); // [
        let mut items = Vec::new();
        loop {
            match &self.tok {
                Tok::RBracket => {
                    let end = self.tok_end as u32;
                    self.next();
                    return Value {
                        kind: ValueKind::Array(items),
                        span: (start, end),
                        full_start,
                    };
                }
                Tok::Eof => {
                    let p = self.tok_start;
                    self.error_at(p, p, 1005, "']' expected.");
                    return Value {
                        kind: ValueKind::Array(items),
                        span: (start, p as u32),
                        full_start,
                    };
                }
                Tok::Comma => {
                    // Omitted element: `[a,,b]`.
                    let v = self.value_error();
                    items.push(v);
                    self.next();
                    continue;
                }
                Tok::RBrace => {
                    let (s, e) = (self.tok_start, self.tok_end);
                    self.error_at(s, e, 1005, "']' expected.");
                    return Value {
                        kind: ValueKind::Array(items),
                        span: (start, s as u32),
                        full_start,
                    };
                }
                // The one token that cannot start a value: skip it, or the loop never ends.
                Tok::Colon => {
                    let (s, e) = (self.tok_start, self.tok_end);
                    self.error_at(s, e, 1137, "Expression or comma expected.");
                    self.next();
                    continue;
                }
                _ => {}
            }
            let v = self.parse_value();
            items.push(v);
            match &self.tok {
                Tok::Comma => self.next(),
                Tok::RBracket | Tok::Eof | Tok::RBrace => {}
                Tok::Colon => {
                    let (s, e) = (self.tok_start, self.tok_end);
                    self.error_at(s, e, 1005, "',' expected.");
                    self.next();
                }
                _ => {
                    let (s, e) = (self.tok_start, self.tok_end);
                    self.error_at(s, e, 1005, "',' expected.");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codes(src: &str) -> Vec<u32> {
        parse(src, "t.json")
            .diagnostics
            .iter()
            .map(|d| d.code)
            .collect()
    }

    #[test]
    fn lenient_input() {
        let p = parse(
            "\u{feff}{ // c\n \"a\": [1, 0x10, -2.5e1,], /* b */ \"b\": {\"c\": null,}, }",
            "t.json",
        );
        assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
        let root = p.root.unwrap();
        let a = root.get("a").unwrap().as_array().unwrap();
        assert_eq!(a.len(), 3);
        assert_eq!(a[1].kind, ValueKind::Number(16.0));
        assert_eq!(a[2].kind, ValueKind::Number(-25.0));
        assert!(root.get("b").unwrap().get("c").unwrap().is_null());
    }

    #[test]
    fn strings_and_escapes() {
        let p = parse(r#"{"s": "a\u0041\n\"\\\/", "t": 'single'}"#, "t.json");
        let root = p.root.unwrap();
        assert_eq!(root.get("s").unwrap().as_str(), Some("aA\n\"\\/"));
        assert_eq!(root.get("t").unwrap().as_str(), Some("single"));
    }

    #[test]
    fn tsc_error_codes() {
        assert_eq!(codes("{'a': 1}"), [1327]);
        assert_eq!(codes("{a: 1}"), [1327]);
        assert_eq!(codes(r#"{"a": 1 "b": 2}"#), [1005]);
        assert_eq!(codes(r#"{"a": 1"#), [1005]);
        assert_eq!(codes(r#"{"a": 1,, "b": 2}"#), [1136]);
        assert_eq!(codes(r#"{"a" 1}"#), [1005]);
        assert_eq!(codes(r#"{"a": x}"#), [1328]);
        assert_eq!(codes(r#"{"a": +1}"#), [1328]);
        assert_eq!(codes(r#"{"a": 1}}"#), [1012]);
        assert_eq!(codes(r#"{"a": 1} /* x"#), [1010]);
        assert_eq!(codes(r#"{"a": "x}"#), [1002]);
        assert_eq!(codes(r#"{"a": 1}]"#), [1012]);
        assert_eq!(codes(r#"{"a": {"b": 1}]"#), [1005, 1005]);
        // Colons in arrays (these used to loop forever).
        assert_eq!(codes(r#"{"files":[:]}"#), [1137]);
        assert_eq!(codes(r#"{"files":["a.ts":]}"#), [1005]);
        assert_eq!(codes(r#"{"files":[:, "a.ts"]}"#), [1137, 1328]);
        let p = parse(r#"{"files":["a.ts" : "b.ts"]}"#, "t.json");
        assert_eq!(p.diagnostics.len(), 1);
        assert_eq!(p.diagnostics[0].span, Some((17, 18)));
        let files = p.root.unwrap();
        assert_eq!(files.get("files").unwrap().as_array().unwrap().len(), 2);
        assert!(parse("", "t").root.is_none());
        assert!(parse("  // only\n", "t").root.is_none());
    }

    #[test]
    fn spans() {
        let src = r#"{"a": "xy"}"#;
        let p = parse(src, "t.json");
        let root = p.root.unwrap();
        let m = root.member("a").unwrap();
        assert_eq!(m.key_span, (1, 4));
        assert_eq!(m.value.span, (6, 10));
        assert_eq!(m.value.full_start, 5);
    }

    #[test]
    fn duplicate_keys_last_wins() {
        let p = parse(r#"{"a": 1, "a": 2}"#, "t");
        assert_eq!(
            p.root.unwrap().get("a").unwrap().kind,
            ValueKind::Number(2.0)
        );
    }

    #[test]
    fn never_panics_on_garbage() {
        for src in [
            "{",
            "[",
            "{\"a\":",
            "\"",
            "/*",
            "-",
            "{[}]",
            "{\"a\": [1, }",
            "\\u",
            "{\"a\":\"\\u{110000}\"}",
            "0x",
            "1e",
            "{{{{",
            "]]]]",
            "[:]",
            "[::::",
            "[1:2:]",
        ] {
            let _ = parse(src, "t");
        }
        let deep = "[".repeat(10_000);
        let _ = parse(&deep, "t");
    }
}
