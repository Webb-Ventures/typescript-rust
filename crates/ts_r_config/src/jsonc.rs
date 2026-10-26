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
    /// Decoded key text (empty for [`KeyForm::Computed`] and [`KeyForm::Shorthand`]).
    pub key: String,
    /// Span of the key token (of the whole element for a shorthand).
    pub key_span: (u32, u32),
    /// The value ([`ValueKind::Invalid`] spanning the element for a shorthand).
    pub value: Value,
    /// How the key was written.
    pub key_form: KeyForm,
}

/// How an object member's key was written. Only [`KeyForm::Quoted`] is JSON; the others
/// have already been reported (TS1327, or TS1136 for a shorthand).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyForm {
    /// `"key": value`
    Quoted,
    /// `key: value`, `'key': value` or `1: value` (TS1327; the key still names an option).
    Unquoted,
    /// `[expr]: value` (TS1327; no key text).
    Computed,
    /// `name`, `name = value` or `...expr`: not a property assignment (TS1136).
    Shorthand,
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

/// How deep values, operators and parentheses may nest before the parser stops descending
/// (each extra level is one TS1328 and one skipped token, never a stack overflow).
const MAX_DEPTH: u32 = 256;

const VALUE_MESSAGE: &str = "Property value can only be string literal, numeric literal, 'true', 'false', 'null', object literal or array literal.";

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    LParen,
    RParen,
    Colon,
    Comma,
    /// `.` not starting a number.
    Dot,
    /// `?` alone (`??` and `?.` are operators).
    Question,
    /// Any other punctuator: `=`, `+`, `===`, `...`, `;`, `@`, ...
    Op(&'static str),
    Str {
        value: String,
        single: bool,
    },
    Num {
        value: f64,
        bigint: bool,
    },
    /// An identifier, keyword or `#name`. `reserved` words (and `#name`) cannot be a
    /// shorthand property.
    Ident {
        name: String,
        reserved: bool,
    },
    Template,
    /// A character that starts no token (already reported as TS1127).
    Other,
    Eof,
}

/// Punctuators longest first, so the first prefix that matches is the token.
const PUNCTUATORS: &[&str] = &[
    ">>>=", "...", "===", "!==", "**=", "<<=", ">>=", ">>>", "&&=", "||=", "??=", "==", "!=", "<=",
    ">=", "&&", "||", "??", "**", "<<", ">>", "++", "--", "+=", "-=", "*=", "/=", "%=", "&=", "|=",
    "^=", "=>", "=", "<", ">", "+", "-", "*", "/", "%", "&", "|", "^", "!", "~", ";", "@",
];

/// Punctuators that join two operands (assignment included): after a value they make it an
/// expression, which is not JSON.
const BINARY: &[&str] = &[
    ">>>=", "===", "!==", "**=", "<<=", ">>=", ">>>", "&&=", "||=", "??=", "==", "!=", "<=", ">=",
    "&&", "||", "??", "**", "<<", ">>", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "=>", "=",
    "<", ">", "+", "-", "*", "/", "%", "&", "|", "^",
];

/// Words that are not identifiers, so `{ true }` is a property missing its `:`, not a
/// shorthand property.
const RESERVED: &[&str] = &[
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "function",
    "if",
    "import",
    "in",
    "instanceof",
    "new",
    "null",
    "return",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "var",
    "void",
    "while",
    "with",
];

/// The list being parsed, for error recovery: a token that ends or continues an enclosing
/// list stops the inner one instead of being skipped.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Ctx {
    Object,
    Array,
}

struct Parser<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
    file: &'a str,
    /// Parse errors: one per position (a second error where the last one starts is dropped).
    diags: Vec<ConfigDiagnostic>,
    /// Conversion errors (TS1327, TS1328, TS1136 for a non-JSON tree): not deduplicated
    /// against parse errors. Dropped again when the value they are in turns out to be part of
    /// a larger expression, which is reported once as a whole.
    conv: Vec<ConfigDiagnostic>,
    // current token
    tok: Tok,
    tok_start: usize,
    tok_end: usize,
    /// End of the previous token (start of the current token's leading trivia).
    prev_end: usize,
    /// A line break in the current token's leading trivia.
    nl_before: bool,
    depth: u32,
    template_depth: u32,
    ctx: Vec<Ctx>,
}

/// Parses JSONC text. `file` is used only to label diagnostics.
///
/// The error model follows tsc 7, which parses a JSON file as one expression (observed with
/// `tsc -p` on malformed configs; the cases are the oracle fixtures `tests/fixtures/malformed`):
///
/// - Parse errors (TS1005, TS1109, TS1136, TS1137, TS1012 and the scanner's TS1002, TS1010,
///   TS1125, TS1127, ...) keep one error per position: a parse error that starts where the
///   previous one started is dropped.
/// - Conversion errors are separate: TS1327 for a key or value that is not a double-quoted
///   string, TS1328 for a value that is not JSON (an identifier, a missing value, an
///   expression such as `a + b` or `f()`, reported once over its whole extent), TS1136 for a
///   shorthand property (`{ name }`, `{ ...x }`).
/// - A missing value (`"a": }`) is TS1109 where its token starts, at the end of the previous
///   token when that token is the end of the file.
/// - A list that meets a token it cannot use reports it (TS1136 in objects, TS1137 in arrays),
///   then stops when the token ends or continues an enclosing list, else skips it. After an
///   element, anything but `,` or the closer is "',' expected." (`[ "a" }` too).
/// - The root may lack its `{`: anything but `[`, a literal or a negative number parses as
///   an object literal after "'{' expected.". Tokens after the root are TS1012 once, then are
///   parsed the same way and checked, and the first object literal is the config.
pub fn parse(src: &str, file: &str) -> ParsedJson {
    let mut p = Parser {
        src,
        bytes: src.as_bytes(),
        pos: 0,
        file,
        diags: Vec::new(),
        conv: Vec::new(),
        tok: Tok::Eof,
        tok_start: 0,
        tok_end: 0,
        prev_end: 0,
        nl_before: false,
        depth: 0,
        template_depth: 0,
        ctx: Vec::new(),
    };
    if src.starts_with('\u{feff}') {
        p.pos = 3;
    }
    p.prev_end = p.pos;
    p.next();
    if p.tok == Tok::Eof {
        return p.finish(None);
    }
    let mut root: Option<Value> = None;
    let mut reported_extra = false;
    loop {
        let before = p.tok_start;
        let v = p.parse_root_expression();
        match &root {
            None => root = Some(v),
            Some(r) if r.as_object().is_none() && v.as_object().is_some() => root = Some(v),
            Some(_) => {}
        }
        if p.tok == Tok::Eof {
            break;
        }
        if p.tok_start == before {
            // Nothing was consumed (cannot happen; keeps the loop finite regardless).
            p.next();
            continue;
        }
        if !reported_extra {
            reported_extra = true;
            p.error_here(1012, "Unexpected token.");
        }
    }
    p.finish(root)
}

fn is_line_break(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

fn is_ident_start(c: char) -> bool {
    c == '_' || c == '$' || c.is_alphabetic()
}

fn is_ident_part(c: char) -> bool {
    c == '_' || c == '$' || c.is_alphanumeric() || c == '\u{200c}' || c == '\u{200d}'
}

impl<'a> Parser<'a> {
    fn finish(self, root: Option<Value>) -> ParsedJson {
        let mut diagnostics = self.diags;
        diagnostics.extend(self.conv);
        // Stable: parse errors before conversion errors at the same position.
        diagnostics.sort_by_key(|d| d.span.map(|s| s.0));
        diagnostics.dedup();
        ParsedJson { root, diagnostics }
    }

    fn error_at(&mut self, start: usize, end: usize, code: u32, msg: &str) {
        // Like tsc, a parse error at the same position as the previous error is dropped.
        if self
            .diags
            .last()
            .and_then(|d| d.span)
            .is_some_and(|(s, _)| s as usize == start)
        {
            return;
        }
        self.push_scanner_error(start, end, code, msg.to_owned());
    }

    /// Scanner errors are always recorded (but still count as the last error for the rule
    /// above); an identical one in a row is dropped.
    fn push_scanner_error(&mut self, start: usize, end: usize, code: u32, msg: String) {
        let d = ConfigDiagnostic::at(code, msg, self.file, (start as u32, end as u32));
        if self.diags.last() != Some(&d) {
            self.diags.push(d);
        }
    }

    fn scan_error(&mut self, start: usize, end: usize, code: u32, msg: &str) {
        self.push_scanner_error(start, end, code, msg.to_owned());
    }

    fn error_here(&mut self, code: u32, msg: &str) {
        let (s, e) = (self.tok_start, self.tok_end);
        self.error_at(s, e, code, msg);
    }

    fn conv_at(&mut self, start: u32, end: u32, code: u32, msg: &str) {
        self.conv
            .push(ConfigDiagnostic::at(code, msg, self.file, (start, end)));
    }

    fn char_at(&self, pos: usize) -> Option<char> {
        self.src.get(pos..)?.chars().next()
    }

    fn skip_trivia(&mut self) {
        loop {
            let Some(&b) = self.bytes.get(self.pos) else {
                return;
            };
            match b {
                b' ' | b'\t' | 0x0b | 0x0c => self.pos += 1,
                b'\n' | b'\r' => {
                    self.nl_before = true;
                    self.pos += 1;
                }
                b'/' if self.bytes.get(self.pos + 1) == Some(&b'/') => {
                    while let Some(&c) = self.bytes.get(self.pos) {
                        if c == b'\n' || c == b'\r' {
                            break;
                        }
                        self.pos += 1;
                    }
                }
                b'/' if self.bytes.get(self.pos + 1) == Some(&b'*') => {
                    self.pos += 2;
                    let mut closed = false;
                    while self.pos < self.bytes.len() {
                        let c = self.bytes[self.pos];
                        if c == b'*' && self.bytes.get(self.pos + 1) == Some(&b'/') {
                            self.pos += 2;
                            closed = true;
                            break;
                        }
                        if c == b'\n' || c == b'\r' {
                            self.nl_before = true;
                        }
                        self.pos += 1;
                    }
                    if !closed {
                        let end = self.bytes.len();
                        self.scan_error(end, end, 1010, "'*/' expected.");
                    }
                }
                _ => {
                    // Unicode whitespace (NBSP, BOM, line/paragraph separators...)
                    let ch = self.char_at(self.pos).unwrap_or('\0');
                    if ch.is_whitespace() || ch == '\u{feff}' {
                        if is_line_break(ch) {
                            self.nl_before = true;
                        }
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
        self.nl_before = false;
        self.skip_trivia();
        self.tok_start = self.pos;
        self.tok = self.scan_token();
        self.tok_end = self.pos;
    }

    fn scan_token(&mut self) -> Tok {
        let Some(&b) = self.bytes.get(self.pos) else {
            return Tok::Eof;
        };
        let single = |p: &mut Self, t: Tok| {
            p.pos += 1;
            t
        };
        match b {
            b'{' => single(self, Tok::LBrace),
            b'}' => single(self, Tok::RBrace),
            b'[' => single(self, Tok::LBracket),
            b']' => single(self, Tok::RBracket),
            b'(' => single(self, Tok::LParen),
            b')' => single(self, Tok::RParen),
            b':' => single(self, Tok::Colon),
            b',' => single(self, Tok::Comma),
            b'"' | b'\'' => self.scan_string(b),
            b'`' => {
                self.scan_template();
                Tok::Template
            }
            b'0'..=b'9' => self.scan_number(),
            b'.' => match self.bytes.get(self.pos + 1) {
                Some(b'0'..=b'9') => self.scan_number(),
                Some(b'.') if self.bytes.get(self.pos + 2) == Some(&b'.') => {
                    self.pos += 3;
                    Tok::Op("...")
                }
                _ => single(self, Tok::Dot),
            },
            b'?' => {
                let rest = &self.bytes[self.pos..];
                if rest.starts_with(b"??=") {
                    self.pos += 3;
                    Tok::Op("??=")
                } else if rest.starts_with(b"??") {
                    self.pos += 2;
                    Tok::Op("??")
                } else if rest.starts_with(b"?.") && !matches!(rest.get(2), Some(b'0'..=b'9')) {
                    self.pos += 2;
                    Tok::Op("?.")
                } else {
                    single(self, Tok::Question)
                }
            }
            b'#' => {
                let start = self.pos;
                self.pos += 1;
                let name_start = self.pos;
                let name = self.scan_ident_rest(String::from("#"));
                if self.pos == name_start {
                    self.scan_error(start, start + 1, 1127, "Invalid character.");
                }
                Tok::Ident {
                    name,
                    reserved: true,
                }
            }
            _ => {
                let rest = self.src.get(self.pos..).unwrap_or("");
                if let Some(op) = PUNCTUATORS.iter().find(|op| rest.starts_with(**op)) {
                    self.pos += op.len();
                    return Tok::Op(op);
                }
                let ch = rest.chars().next().unwrap_or('\0');
                if is_ident_start(ch) || (ch == '\\' && self.unicode_escape_len(self.pos).is_some())
                {
                    let name = self.scan_ident_rest(String::new());
                    let reserved = RESERVED.contains(&name.as_str());
                    Tok::Ident { name, reserved }
                } else {
                    let start = self.pos;
                    self.pos += ch.len_utf8().max(1);
                    self.scan_error(start, self.pos, 1127, "Invalid character.");
                    Tok::Other
                }
            }
        }
    }

    /// The byte length of a `\uXXXX` or `\u{X...}` escape at `pos`, if one is there.
    fn unicode_escape_len(&self, pos: usize) -> Option<(usize, char)> {
        let b = self.bytes;
        if b.get(pos) != Some(&b'\\') || b.get(pos + 1) != Some(&b'u') {
            return None;
        }
        if b.get(pos + 2) == Some(&b'{') {
            let start = pos + 3;
            let mut end = start;
            while end < b.len() && b[end].is_ascii_hexdigit() && end - start < 8 {
                end += 1;
            }
            if end == start || b.get(end) != Some(&b'}') {
                return None;
            }
            let v = u32::from_str_radix(self.src.get(start..end)?, 16).ok()?;
            return Some((end + 1 - pos, char::from_u32(v)?));
        }
        let digits = self.src.get(pos + 2..pos + 6)?;
        if !digits.bytes().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let v = u32::from_str_radix(digits, 16).ok()?;
        Some((6, char::from_u32(v).unwrap_or('\u{fffd}')))
    }

    /// Identifier characters (and unicode escapes) from `pos`, appended to `name`.
    fn scan_ident_rest(&mut self, mut name: String) -> String {
        while let Some(c) = self.char_at(self.pos) {
            if is_ident_part(c) {
                name.push(c);
                self.pos += c.len_utf8();
            } else if let Some((len, ch)) = self.unicode_escape_len(self.pos) {
                name.push(ch);
                self.pos += len;
            } else {
                break;
            }
        }
        name
    }

    /// A template literal, `${...}` substitutions included (their contents are skipped,
    /// nested templates and strings too). Unterminated: TS1160 at the end of the file.
    fn scan_template(&mut self) {
        self.pos += 1;
        self.template_depth += 1;
        loop {
            let Some(&c) = self.bytes.get(self.pos) else {
                let end = self.bytes.len();
                self.scan_error(end, end, 1160, "Unterminated template literal.");
                break;
            };
            match c {
                b'\\' => self.pos = (self.pos + 2).min(self.bytes.len()),
                b'`' => {
                    self.pos += 1;
                    break;
                }
                b'$' if self.bytes.get(self.pos + 1) == Some(&b'{') => {
                    self.pos += 2;
                    let mut braces = 1u32;
                    while let Some(&c) = self.bytes.get(self.pos) {
                        match c {
                            b'{' => braces += 1,
                            b'}' => {
                                braces -= 1;
                                if braces == 0 {
                                    self.pos += 1;
                                    break;
                                }
                            }
                            b'`' if self.template_depth < 32 => {
                                self.scan_template();
                                continue;
                            }
                            b'"' | b'\'' => {
                                let _ = self.scan_string(c);
                                continue;
                            }
                            _ => {}
                        }
                        self.pos += 1;
                    }
                }
                _ => self.pos += 1,
            }
        }
        self.template_depth -= 1;
        // Never stop inside a UTF-8 sequence (a `\` before a multi-byte character).
        while self.pos < self.bytes.len() && !self.src.is_char_boundary(self.pos) {
            self.pos += 1;
        }
    }

    /// Rescans the `/` or `/=` token at `tok_start` as a regular expression. An unterminated
    /// one (TS1161 at its start) ends at the first closing bracket its body did not open, or
    /// at the end of the line.
    fn rescan_regex(&mut self) {
        let start = self.tok_start;
        let mut p = start + 1;
        let (mut in_class, mut escape) = (false, false);
        let mut terminated = false;
        while let Some(c) = self.char_at(p) {
            if is_line_break(c) {
                break;
            }
            if escape {
                escape = false;
            } else if c == '/' && !in_class {
                p += 1;
                terminated = true;
                break;
            } else if c == '[' {
                in_class = true;
            } else if c == '\\' {
                escape = true;
            } else if c == ']' {
                in_class = false;
            }
            p += c.len_utf8();
        }
        if terminated {
            while let Some(c) = self.char_at(p) {
                if !is_ident_part(c) {
                    break;
                }
                p += c.len_utf8();
            }
        } else {
            self.scan_error(start, p, 1161, "Unterminated regular expression literal.");
            let mut depth = 0u32;
            let mut q = start + 1;
            while q < p {
                match self.bytes[q] {
                    b'(' | b'[' | b'{' => depth += 1,
                    b')' | b']' | b'}' => {
                        if depth == 0 {
                            p = q;
                            break;
                        }
                        depth -= 1;
                    }
                    _ => {}
                }
                q += 1;
            }
        }
        self.pos = p;
        self.tok_end = p;
    }

    fn scan_string(&mut self, quote: u8) -> Tok {
        self.pos += 1;
        let mut value = String::new();
        loop {
            let Some(&c) = self.bytes.get(self.pos) else {
                let end = self.bytes.len();
                self.scan_error(end, end, 1002, "Unterminated string literal.");
                break;
            };
            if c == quote {
                self.pos += 1;
                break;
            }
            if c == b'\n' || c == b'\r' {
                let p = self.pos;
                self.scan_error(p, p, 1002, "Unterminated string literal.");
                break;
            }
            if c == b'\\' {
                self.pos += 1;
                self.scan_escape(&mut value);
                continue;
            }
            let ch = self.char_at(self.pos).unwrap_or('\u{fffd}');
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
        // A malformed escape stands for its own text, backslash included (as in tsc).
        let backslash = self.pos - 1;
        let raw =
            |p: &Self, out: &mut String| out.push_str(p.src.get(backslash..p.pos).unwrap_or(""));
        self.pos += 1;
        match c {
            b'n' => out.push('\n'),
            b't' => out.push('\t'),
            b'r' => out.push('\r'),
            b'b' => out.push('\u{8}'),
            b'f' => out.push('\u{c}'),
            b'v' => out.push('\u{b}'),
            b'0' if !matches!(self.bytes.get(self.pos), Some(b'0'..=b'9')) => out.push('\0'),
            b'x' => match self.hex_escape(2) {
                Some(v) => out.push(char::from_u32(v).unwrap_or('\u{fffd}')),
                None => raw(self, out),
            },
            b'u' => {
                if self.bytes.get(self.pos) == Some(&b'{') {
                    self.pos += 1;
                    match self.extended_escape() {
                        Some(ch) => out.push(ch),
                        None => raw(self, out),
                    }
                } else if let Some(v) = self.hex_escape(4) {
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
                    raw(self, out);
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
                let ch = self.char_at(self.pos).unwrap_or('\u{fffd}');
                out.push(ch);
                self.pos += ch.len_utf8();
            }
        }
    }

    /// `n` hex digits of an escape; fewer is TS1125 where the digits stop.
    fn hex_escape(&mut self, n: usize) -> Option<u32> {
        if let Some(v) = self.hex_digits(n) {
            return Some(v);
        }
        while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_hexdigit() {
            self.pos += 1;
        }
        let p = self.pos;
        self.scan_error(p, p, 1125, "Hexadecimal digit expected.");
        None
    }

    /// The rest of a `\u{...}` escape (after the `{`).
    fn extended_escape(&mut self) -> Option<char> {
        let start = self.pos;
        while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_hexdigit() {
            self.pos += 1;
        }
        let end = self.pos;
        if end == start {
            self.scan_error(start, start, 1125, "Hexadecimal digit expected.");
            return None;
        }
        let digits = self
            .src
            .get(start..end)
            .unwrap_or("")
            .trim_start_matches('0');
        let value = if digits.len() > 6 {
            None
        } else {
            u32::from_str_radix(if digits.is_empty() { "0" } else { digits }, 16)
                .ok()
                .filter(|v| *v <= 0x10ffff)
        };
        if value.is_none() {
            self.scan_error(
                start,
                end,
                1198,
                "An extended Unicode escape value must be between 0x0 and 0x10FFFF inclusive.",
            );
        }
        if self.bytes.get(self.pos) == Some(&b'}') {
            self.pos += 1;
        } else {
            let p = self.pos;
            self.scan_error(p, p, 1199, "Unterminated Unicode escape sequence.");
            return None;
        }
        value.map(|v| char::from_u32(v).unwrap_or('\u{fffd}'))
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

    /// Digits of `radix` with numeric separators, from `pos`. Returns the digits without
    /// separators. A separator not between two digits is TS6188, two in a row TS6189.
    fn scan_digits(&mut self, radix: u32) -> String {
        let mut digits = String::new();
        // A `_` right before the current position.
        let mut sep: Option<usize> = None;
        while let Some(&c) = self.bytes.get(self.pos) {
            if c == b'_' {
                let p = self.pos;
                if sep.is_some() {
                    self.scan_error(
                        p,
                        p + 1,
                        6189,
                        "Multiple consecutive numeric separators are not permitted.",
                    );
                } else if digits.is_empty() {
                    self.scan_error(p, p + 1, 6188, "Numeric separators are not allowed here.");
                }
                sep = Some(p);
                self.pos += 1;
            } else if char::from(c).is_digit(radix) {
                digits.push(char::from(c));
                sep = None;
                self.pos += 1;
            } else {
                break;
            }
        }
        if let Some(p) = sep {
            self.scan_error(p, p + 1, 6188, "Numeric separators are not allowed here.");
        }
        digits
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
            let (radix, code, msg) = match b[start + 1] {
                b'x' | b'X' => (16, 1125, "Hexadecimal digit expected."),
                b'o' | b'O' => (8, 1178, "Octal digit expected."),
                _ => (2, 1177, "Binary digit expected."),
            };
            self.pos += 2;
            let digits = self.scan_digits(radix);
            if digits.is_empty() {
                let p = self.pos;
                self.scan_error(p, p, code, msg);
            }
            let value = u64::from_str_radix(&digits, radix)
                .map(|v| v as f64)
                .unwrap_or(if digits.is_empty() {
                    0.0
                } else {
                    f64::INFINITY
                });
            let bigint = self.bytes.get(self.pos) == Some(&b'n');
            if bigint {
                self.pos += 1;
            }
            return Tok::Num { value, bigint };
        }
        let mut text = self.scan_digits(10);
        let leading_zero = text.len() > 1 && text.starts_with('0');
        if leading_zero && text.bytes().all(|c| c < b'8') {
            // Legacy octal (`01`): its value, and TS1121 pointing at `0o`.
            let value = u64::from_str_radix(&text, 8)
                .map(|v| v as f64)
                .unwrap_or(0.0);
            self.scan_error(
                start,
                self.pos,
                1121,
                &format!(
                    "Octal literals are not allowed. Use the syntax '0o{value:o}'.",
                    value = value as u64
                ),
            );
            self.after_number();
            return Tok::Num {
                value,
                bigint: false,
            };
        }
        if leading_zero {
            self.scan_error(
                start,
                self.pos,
                1489,
                "Decimals with leading zeros are not allowed.",
            );
        }
        let mut plain = true;
        if self.bytes.get(self.pos) == Some(&b'.') {
            self.pos += 1;
            plain = false;
            text.push('.');
            text.push_str(&self.scan_digits(10));
        }
        if matches!(self.bytes.get(self.pos), Some(b'e' | b'E')) {
            plain = false;
            self.pos += 1;
            text.push('e');
            if let Some(&s @ (b'+' | b'-')) = self.bytes.get(self.pos) {
                self.pos += 1;
                text.push(s as char);
            }
            let exp = self.scan_digits(10);
            if exp.is_empty() {
                let p = self.pos;
                self.scan_error(p, p, 1124, "Digit expected.");
                text.push('0');
            }
            text.push_str(&exp);
        }
        let bigint = plain && self.bytes.get(self.pos) == Some(&b'n');
        if bigint {
            self.pos += 1;
        } else {
            self.after_number();
        }
        Tok::Num {
            value: text.parse::<f64>().unwrap_or(f64::NAN),
            bigint,
        }
    }

    /// An identifier right after a decimal literal (`1x`) is TS1351 and scanned on its own.
    fn after_number(&mut self) {
        let Some(c) = self.char_at(self.pos) else {
            return;
        };
        if is_ident_start(c) {
            let save = self.pos;
            let _ = self.scan_ident_rest(String::new());
            let end = self.pos;
            self.pos = save;
            self.scan_error(
                save,
                end,
                1351,
                "An identifier or keyword cannot immediately follow a numeric literal.",
            );
        }
    }

    /// Whether the token after the current one is `:` (or, with `number`, a number).
    fn peek(&mut self, number: bool) -> bool {
        let saved = (
            self.pos,
            self.tok.clone(),
            self.tok_start,
            self.tok_end,
            self.prev_end,
            self.nl_before,
        );
        let n = self.diags.len();
        self.next();
        let hit = if number {
            matches!(self.tok, Tok::Num { .. })
        } else {
            self.tok == Tok::Colon
        };
        self.diags.truncate(n);
        (
            self.pos,
            self.tok,
            self.tok_start,
            self.tok_end,
            self.prev_end,
            self.nl_before,
        ) = saved;
        hit
    }

    fn is_property_start(&self) -> bool {
        matches!(
            self.tok,
            Tok::Str { .. } | Tok::Num { .. } | Tok::Ident { .. } | Tok::LBracket | Tok::Op("...")
        )
    }

    fn is_expression_start(&self) -> bool {
        matches!(
            self.tok,
            Tok::LBrace
                | Tok::LBracket
                | Tok::LParen
                | Tok::Str { .. }
                | Tok::Num { .. }
                | Tok::Ident { .. }
                | Tok::Template
                | Tok::Op("-" | "+" | "!" | "~" | "++" | "--" | "/" | "/=" | "@")
        )
    }

    /// Whether the current token ends or continues one of the lists being parsed (the end of
    /// the file ends them all).
    fn in_some_context(&self) -> bool {
        self.tok == Tok::Eof
            || self.ctx.iter().any(|c| match c {
                Ctx::Object => self.tok == Tok::RBrace || self.is_property_start(),
                Ctx::Array => {
                    matches!(self.tok, Tok::RBracket | Tok::Comma) || self.is_expression_start()
                }
            })
    }

    fn expect_close(&mut self, tok: Tok, msg: &str) {
        if self.tok == tok {
            self.next();
        } else {
            self.error_here(1005, msg);
        }
    }

    /// The value `start..` (the tokens consumed so far) as one non-JSON expression: the
    /// conversion errors inside it are replaced by one TS1328 over all of it.
    fn invalid_from(&mut self, start: u32, full_start: u32, mark: usize) -> Value {
        self.conv.truncate(mark);
        let end = (self.prev_end as u32).max(start);
        self.conv_at(start, end, 1328, VALUE_MESSAGE);
        Value {
            kind: ValueKind::Invalid,
            span: (start, end),
            full_start,
        }
    }

    /// Past [`MAX_DEPTH`]: one TS1328 and the token skipped.
    fn too_deep(&mut self) -> Value {
        let full_start = self.prev_end as u32;
        let (s, e) = (self.tok_start as u32, self.tok_end as u32);
        self.conv_at(s, e, 1328, VALUE_MESSAGE);
        if self.tok != Tok::Eof {
            self.next();
        }
        Value {
            kind: ValueKind::Invalid,
            span: (s, e),
            full_start,
        }
    }

    /// One top-level expression of the file.
    fn parse_root_expression(&mut self) -> Value {
        let word = matches!(&self.tok, Tok::Ident { name, .. }
            if matches!(name.as_str(), "true" | "false" | "null"));
        match self.tok {
            Tok::LBracket => self.parse_array(),
            Tok::LBrace => self.parse_object(true),
            _ if word => self.parse_primary(),
            Tok::Op("-") => {
                if self.peek(true) {
                    self.parse_unary()
                } else {
                    self.parse_object(false)
                }
            }
            Tok::Str { .. } | Tok::Num { .. } => {
                if self.peek(false) {
                    self.parse_object(false)
                } else {
                    self.parse_primary()
                }
            }
            _ => self.parse_object(false),
        }
    }

    /// A value: an assignment expression in tsc's grammar, of which only literals, objects,
    /// arrays and negative numbers are JSON.
    fn parse_value(&mut self) -> Value {
        if self.depth >= MAX_DEPTH {
            return self.too_deep();
        }
        let full_start = self.prev_end as u32;
        let start = self.tok_start as u32;
        let mark = self.conv.len();
        self.depth += 1;
        let mut v = self.parse_unary();
        let mut invalid = false;
        loop {
            let binary = match &self.tok {
                Tok::Op(op) => BINARY.contains(op),
                Tok::Ident { name, .. } => name == "in" || name == "instanceof",
                _ => false,
            };
            if binary {
                self.next();
                let _ = self.parse_unary();
                invalid = true;
            } else if self.tok == Tok::Question {
                self.next();
                let _ = self.parse_value();
                self.expect_close(Tok::Colon, "':' expected.");
                let _ = self.parse_value();
                invalid = true;
            } else {
                break;
            }
        }
        self.depth -= 1;
        if invalid {
            v = self.invalid_from(start, full_start, mark);
        }
        v
    }

    fn parse_unary(&mut self) -> Value {
        let prefix = match &self.tok {
            Tok::Op(op @ ("-" | "+" | "!" | "~" | "++" | "--" | "@")) => Some(*op),
            Tok::Ident { name, .. } if matches!(name.as_str(), "typeof" | "void" | "delete") => {
                Some("word")
            }
            _ => None,
        };
        let Some(op) = prefix else {
            return self.parse_postfix();
        };
        if self.depth >= MAX_DEPTH {
            return self.too_deep();
        }
        let full_start = self.prev_end as u32;
        let start = self.tok_start as u32;
        let mark = self.conv.len();
        self.depth += 1;
        self.next();
        if op == "@" {
            // A decorator, then the expression it decorates.
            let _ = self.parse_postfix();
        }
        let operand = self.parse_unary();
        self.depth -= 1;
        if op == "-"
            && let ValueKind::Number(n) = operand.kind
        {
            self.conv.truncate(mark);
            return Value {
                kind: ValueKind::Number(-n),
                span: (start, operand.span.1),
                full_start,
            };
        }
        self.invalid_from(start, full_start, mark)
    }

    fn parse_postfix(&mut self) -> Value {
        let full_start = self.prev_end as u32;
        let start = self.tok_start as u32;
        let mark = self.conv.len();
        let mut v = self.parse_primary();
        let mut invalid = false;
        loop {
            match &self.tok {
                Tok::Dot | Tok::Op("?.") => {
                    let optional = self.tok != Tok::Dot;
                    self.next();
                    if matches!(self.tok, Tok::Ident { .. }) {
                        self.next();
                    } else if !(optional && matches!(self.tok, Tok::LBracket | Tok::LParen)) {
                        self.error_here(1003, "Identifier expected.");
                    }
                }
                Tok::LBracket => {
                    if self.depth >= MAX_DEPTH {
                        break;
                    }
                    self.next();
                    let _ = self.parse_value();
                    self.expect_close(Tok::RBracket, "']' expected.");
                }
                Tok::LParen => {
                    if self.depth >= MAX_DEPTH {
                        break;
                    }
                    self.next();
                    while !matches!(self.tok, Tok::RParen | Tok::Eof) {
                        let before = self.tok_start;
                        let _ = self.parse_value();
                        if self.tok == Tok::Comma {
                            self.next();
                        } else if self.tok_start == before || self.tok != Tok::RParen {
                            break;
                        }
                    }
                    self.expect_close(Tok::RParen, "')' expected.");
                }
                Tok::Template => self.next(),
                Tok::Op("!" | "++" | "--") if !self.nl_before => self.next(),
                _ => break,
            }
            invalid = true;
        }
        if invalid {
            v = self.invalid_from(start, full_start, mark);
        }
        v
    }

    fn parse_primary(&mut self) -> Value {
        let full_start = self.prev_end as u32;
        let start = self.tok_start as u32;
        let end = self.tok_end as u32;
        let mark = self.conv.len();
        let literal = |p: &mut Self, kind: ValueKind| {
            p.next();
            Value {
                kind,
                span: (start, end),
                full_start,
            }
        };
        match std::mem::replace(&mut self.tok, Tok::Eof) {
            Tok::LBrace => {
                self.tok = Tok::LBrace;
                self.parse_object(true)
            }
            Tok::LBracket => {
                self.tok = Tok::LBracket;
                self.parse_array()
            }
            Tok::Str { value, single } => {
                if single {
                    self.conv_at(
                        start,
                        end,
                        1327,
                        "String literal with double quotes expected.",
                    );
                }
                literal(self, ValueKind::String(value))
            }
            Tok::Num {
                value,
                bigint: false,
            } => literal(self, ValueKind::Number(value)),
            Tok::Ident { name, .. } if name == "true" => literal(self, ValueKind::Bool(true)),
            Tok::Ident { name, .. } if name == "false" => literal(self, ValueKind::Bool(false)),
            Tok::Ident { name, .. } if name == "null" => literal(self, ValueKind::Null),
            Tok::Num { .. } | Tok::Ident { .. } | Tok::Template => {
                self.next();
                self.invalid_from(start, full_start, mark)
            }
            Tok::Op(op @ ("/" | "/=")) => {
                self.tok = Tok::Op(op);
                self.rescan_regex();
                self.next();
                self.invalid_from(start, full_start, mark)
            }
            Tok::LParen => {
                self.tok = Tok::LParen;
                if self.depth < MAX_DEPTH {
                    self.next();
                    let _ = self.parse_value();
                    self.expect_close(Tok::RParen, "')' expected.");
                } else {
                    self.next();
                }
                self.invalid_from(start, full_start, mark)
            }
            other => {
                // Nothing here can start a value: a missing one.
                self.tok = other;
                let at = if self.tok == Tok::Eof {
                    self.prev_end
                } else {
                    self.tok_start
                };
                let to = if self.tok == Tok::Eof {
                    at
                } else {
                    self.tok_end
                };
                self.error_at(at, to, 1109, "Expression expected.");
                self.conv_at(start, start, 1328, VALUE_MESSAGE);
                Value {
                    kind: ValueKind::Invalid,
                    span: (start, start),
                    full_start,
                }
            }
        }
    }

    /// An object literal; `has_brace` false for a root that lacks its `{`.
    fn parse_object(&mut self, has_brace: bool) -> Value {
        let full_start = self.prev_end as u32;
        let start = self.tok_start as u32;
        if has_brace {
            self.next();
        } else {
            self.error_here(1005, "'{' expected.");
        }
        self.ctx.push(Ctx::Object);
        let mut members = Vec::new();
        loop {
            match self.tok {
                Tok::RBrace | Tok::Eof => break,
                _ if self.is_property_start() => {
                    members.push(self.parse_member());
                    match self.tok {
                        Tok::Comma => self.next(),
                        Tok::RBrace | Tok::Eof => {}
                        _ => self.error_here(1005, "',' expected."),
                    }
                }
                _ => {
                    self.error_here(1136, "Property assignment expected.");
                    if self.in_some_context() {
                        break;
                    }
                    self.next();
                }
            }
        }
        self.ctx.pop();
        let end = if self.tok == Tok::RBrace {
            let end = self.tok_end as u32;
            self.next();
            end
        } else {
            self.error_here(1005, "'}' expected.");
            self.tok_start as u32
        };
        Value {
            kind: ValueKind::Object(members),
            span: (start, end),
            full_start,
        }
    }

    /// One member; the current token starts a property ([`Self::is_property_start`]).
    fn parse_member(&mut self) -> Member {
        let full_start = self.prev_end as u32;
        let key_start = self.tok_start as u32;
        let mark = self.conv.len();
        let (key, key_form) = match std::mem::replace(&mut self.tok, Tok::Eof) {
            Tok::Str { value, single } => {
                self.next();
                let form = if single {
                    KeyForm::Unquoted
                } else {
                    KeyForm::Quoted
                };
                (value, form)
            }
            Tok::Num { value, .. } => {
                self.next();
                (format_number(value), KeyForm::Unquoted)
            }
            Tok::Ident { name, reserved } => {
                self.next();
                if !reserved && self.tok != Tok::Colon {
                    if self.tok == Tok::Op("=") {
                        self.next();
                        let _ = self.parse_value();
                    }
                    return self.shorthand(key_start, full_start, mark);
                }
                (name, KeyForm::Unquoted)
            }
            Tok::Op("...") => {
                self.next();
                let _ = self.parse_value();
                return self.shorthand(key_start, full_start, mark);
            }
            other => {
                // `[`: a computed name.
                self.tok = other;
                self.next();
                let _ = self.parse_value();
                self.conv.truncate(mark);
                self.expect_close(Tok::RBracket, "']' expected.");
                (String::new(), KeyForm::Computed)
            }
        };
        let key_end = (self.prev_end as u32).max(key_start);
        if key_form != KeyForm::Quoted {
            self.conv_at(
                key_start,
                key_end,
                1327,
                "String literal with double quotes expected.",
            );
        }
        if self.tok == Tok::Colon {
            self.next();
        } else {
            self.error_here(1005, "':' expected.");
        }
        let value = self.parse_value();
        Member {
            key,
            key_span: (key_start, key_end),
            value,
            key_form,
        }
    }

    /// A shorthand property or spread `start..` (consumed): TS1136 over it.
    fn shorthand(&mut self, start: u32, full_start: u32, mark: usize) -> Member {
        self.conv.truncate(mark);
        let end = (self.prev_end as u32).max(start);
        self.conv_at(start, end, 1136, "Property assignment expected.");
        Member {
            key: String::new(),
            key_span: (start, end),
            value: Value {
                kind: ValueKind::Invalid,
                span: (start, end),
                full_start,
            },
            key_form: KeyForm::Shorthand,
        }
    }

    fn parse_array(&mut self) -> Value {
        let full_start = self.prev_end as u32;
        let start = self.tok_start as u32;
        self.next(); // [
        self.ctx.push(Ctx::Array);
        let mut items = Vec::new();
        loop {
            match self.tok {
                Tok::RBracket | Tok::Eof => break,
                Tok::Comma => {
                    // Omitted element: `[a,,b]`.
                    let (s, e) = (self.tok_start as u32, self.tok_end as u32);
                    self.conv_at(s, e, 1328, VALUE_MESSAGE);
                    items.push(Value {
                        kind: ValueKind::Invalid,
                        span: (s, e),
                        full_start: self.prev_end as u32,
                    });
                    self.next();
                }
                _ if self.is_expression_start() => {
                    items.push(self.parse_value());
                    match self.tok {
                        Tok::Comma => self.next(),
                        Tok::RBracket | Tok::Eof => {}
                        _ => self.error_here(1005, "',' expected."),
                    }
                }
                _ => {
                    self.error_here(1137, "Expression or comma expected.");
                    if self.in_some_context() {
                        break;
                    }
                    self.next();
                }
            }
        }
        self.ctx.pop();
        let end = if self.tok == Tok::RBracket {
            let end = self.tok_end as u32;
            self.next();
            end
        } else {
            self.error_here(1005, "']' expected.");
            self.tok_start as u32
        };
        Value {
            kind: ValueKind::Array(items),
            span: (start, end),
            full_start,
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
        // The `]` is parsed as a second root object lacking both braces (tsc 7.0.2).
        assert_eq!(codes(r#"{"a": 1}]"#), [1012, 1005]);
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

    /// `(code, start offset)` of each diagnostic. The cases below were observed with
    /// `tsc -p` (7.0.2); the config oracle's `malformed` fixtures hold the full set.
    fn at(src: &str) -> Vec<(u32, u32)> {
        parse(src, "t.json")
            .diagnostics
            .iter()
            .map(|d| (d.code, d.span.map_or(0, |s| s.0)))
            .collect()
    }

    #[test]
    fn missing_values_are_ts1109_and_ts1328() {
        // The parser's TS1109 and the conversion's TS1328, both at `}`.
        assert_eq!(at(r#"{"a": }"#), [(1109, 6), (1328, 6)]);
        // At the end of the file TS1109 sits right after the `:`, before the trivia.
        assert_eq!(at(r#"{"a": "#), [(1109, 5), (1005, 6), (1328, 6)]);
        // "':' expected." wins over TS1109 at the same position.
        assert_eq!(at(r#"{"a" }"#), [(1005, 5), (1328, 5)]);
    }

    #[test]
    fn shorthand_properties_are_ts1136_only() {
        let p = parse(r#"{ strict, x = 1, ...y }"#, "t.json");
        let codes: Vec<u32> = p.diagnostics.iter().map(|d| d.code).collect();
        assert_eq!(codes, [1136, 1136, 1136]);
        let root = p.root.unwrap();
        let members = root.as_object().unwrap();
        assert!(members.iter().all(|m| m.key_form == KeyForm::Shorthand));
        // A reserved word cannot be a shorthand: it is a key missing its `:`.
        assert_eq!(at("{ true }"), [(1327, 2), (1005, 7), (1328, 7)]);
    }

    #[test]
    fn expressions_are_one_ts1328() {
        for src in [
            r#"{"a": x}"#,
            r#"{"a": "b" + "c"}"#,
            r#"{"a": f(1, 2)}"#,
            r#"{"a": x.y[0]}"#,
            r#"{"a": c ? 1 : 2}"#,
            r#"{"a": -x}"#,
            r#"{"a": !true}"#,
            r#"{"a": true!}"#,
            r#"{"a": /re/g}"#,
            r#"{"a": `t${1}`}"#,
            r#"{"a": 1n}"#,
            r#"{"a": typeof x}"#,
            r#"{"a": (1)}"#,
        ] {
            assert_eq!(codes(src), [1328], "{src}");
        }
        assert_eq!(
            parse(r#"{"a": - 1}"#, "t")
                .root
                .unwrap()
                .get("a")
                .unwrap()
                .kind,
            ValueKind::Number(-1.0)
        );
        // `=` where `:` belongs: "':' expected.", then `<missing> = true` is the value.
        assert_eq!(at(r#"{"a" = true}"#), [(1005, 5), (1328, 5)]);
    }

    #[test]
    fn list_recovery_follows_enclosing_lists() {
        // After an element only `,` or the closer: `}` in an array is "',' expected.".
        assert_eq!(at(r#"{"a": ["b" }"#), [(1005, 11)]);
        // A token an enclosing list can use ends the inner one.
        assert_eq!(at(r#"{"a": [ }"#), [(1137, 8)]);
        assert_eq!(at(r#"[ {"a": 1 ] ]"#), [(1005, 10), (1012, 12), (1005, 13)]);
        // Other tokens are skipped.
        assert_eq!(at(r#"{"a": 1; "b": 2}"#), [(1005, 7)]);
        assert_eq!(at(r#"{"a": 1 @ }"#), [(1005, 8)]);
    }

    #[test]
    fn root_without_braces_and_extra_expressions() {
        assert_eq!(at("hello"), [(1005, 0), (1136, 0), (1005, 5)]);
        assert_eq!(at(r#""a": 1"#), [(1005, 0), (1005, 6)]);
        assert_eq!(at("}"), [(1005, 0)]);
        // One TS1012, then the rest is parsed and checked; the first object is the root.
        assert_eq!(at(r#"{} {"a": x} 1"#), [(1012, 3), (1328, 9)]);
        let p = parse(r#"[1] {"a": 1}"#, "t");
        assert!(p.root.unwrap().get("a").is_some());
        assert!(parse(r#""x" "y""#, "t").root.unwrap().as_str().is_some());
    }

    #[test]
    fn scanner_errors() {
        assert_eq!(at(r#"{"a": "\x1"}"#), [(1125, 10)]);
        assert_eq!(at(r#"{"a": "\u12"}"#), [(1125, 11)]);
        assert_eq!(at(r#"{"a": "\u{}"}"#), [(1125, 10)]);
        assert_eq!(at(r#"{"a": "\u{110000}"}"#), [(1198, 10)]);
        assert_eq!(at(r#"{"a": "\u{41"}"#), [(1199, 12)]);
        // A malformed escape keeps its text.
        let v = parse(r#"{"a": "d\u12"}"#, "t").root.unwrap();
        assert_eq!(v.get("a").unwrap().as_str(), Some(r"d\u12"));
        // Unterminated template and regular expression.
        assert_eq!(at(r#"{"a": `x}"#), [(1328, 6), (1160, 9)]);
        assert_eq!(at(r#"{"a": /x }"#), [(1161, 6), (1328, 6)]);
        assert_eq!(at("{\"a\": 1}\u{1}"), [(1127, 8), (1005, 9)]);
        // Numbers.
        assert_eq!(at(r#"{"a": 0x}"#), [(1125, 8)]);
        assert_eq!(at(r#"{"a": 01}"#), [(1121, 6)]);
        assert_eq!(at(r#"{"a": 08}"#), [(1489, 6)]);
        assert_eq!(at(r#"{"a": 1e}"#), [(1124, 8)]);
        assert_eq!(at(r#"{"a": 1_}"#), [(6188, 7)]);
        assert_eq!(at(r#"{"a": 1__0}"#), [(6189, 8)]);
        assert_eq!(at(r#"{"a": 1x}"#), [(1351, 7), (1136, 7)]);
        // A lone `#` is an invalid character that still names a key.
        assert_eq!(at("{ # }"), [(1127, 2), (1327, 2), (1005, 4), (1328, 4)]);
    }

    #[test]
    fn pathological_input_is_bounded() {
        for src in [
            "!".repeat(100_000),
            "-".repeat(100_000),
            "(".repeat(100_000),
            "{\"a\":".repeat(20_000),
            format!("{{\"a\": {}}}", "[".repeat(50_000)),
            format!("{{\"a\": {}}}", "x.".repeat(50_000)),
            format!("{{\"a\": {}}}", "`${".repeat(20_000)),
            format!("{{\"a\": {}}}", "a ? ".repeat(20_000)),
            format!("{{\"a\": {}}}", "f(".repeat(20_000)),
            "@".repeat(100_000),
            "#".repeat(100_000),
            "/".repeat(100_001),
            "\\".repeat(100_000),
            "\"\\u{".repeat(10_000),
            "1_".repeat(50_000),
            "{ a ".repeat(20_000),
        ] {
            let p = parse(&src, "t");
            // Linear: past the depth limit each token is at most two errors.
            assert!(p.diagnostics.len() <= 2 * src.len() + 2);
        }
    }
}
