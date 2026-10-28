//! Type guessing for the eslint-plugin-unicorn rules, without a type checker.
//!
//! The plugin's rules ask "is this expression an array / a string / a number / a `Map`?"
//! and "is it known *not* to be one?". Without type information the answer comes from what
//! the code says locally:
//!
//! - TypeScript annotations on the binding or an `as`/`<T>` assertion, resolved through
//!   local type aliases, interfaces (`extends`), type parameters (their constraint), classes
//!   (`extends`, when the checker follows class heritage) and imports;
//! - JSDoc `@param`, `@type` and `@returns` tags on the declaration (a small JSDoc type
//!   reader: names, unions, intersections, `?T`, `T=`, `T[]`, records, functions);
//! - the expression's shape (`[]`, `new Map()`, `Array.from(x)`, `"" + x`), the initializer
//!   of a `const`, the documented or annotated return type of a called local function;
//! - static evaluation ([`super::unicorn_static`]) when the value is known outright.
//!
//! Each question is a [`Checker`]: the type names that count as the target, the names known
//! not to be it, and hooks for node shapes. A checker answers [`Guess::Target`],
//! [`Guess::NonTarget`] or [`Guess::Unknown`]; unions are the target when every member is
//! (or any member, with `mixed_union_target`), intersections when any member is.

#![allow(dead_code)] // A shared toolkit: each rule uses part of it.

use std::cell::{Cell, RefCell};

use super::prelude::*;
use super::unicorn_static::{
    Static, evaluated_children, parent_np, skip_ts_only, static_value_control_flow,
    static_value_no_side_effects,
};
use ts_r_ast::{Ast, CommentKind, HeritageKind};

// ---- JSDoc types -------------------------------------------------------------------------

/// The facts a JSDoc type gives (anything unsupported is `Unknown`).
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum JType {
    Unknown,
    /// A type name (`Map`, `Foo.Bar`) or a primitive (`string`, `number`, `boolean`, ...).
    Name(String),
    /// `null` / `undefined`.
    Nullish,
    /// An object-like type that isn't a named one (`{a: 1}`, `Object<K, V>`, `...T`).
    NonPrimitive,
    /// A function type, with its return type.
    Function(Box<JType>),
    /// `?T`
    Nullable(Box<JType>),
    /// `T=`
    Optional(Box<JType>),
    Union(Vec<JType>),
    Intersection(Vec<JType>),
}

const JSDOC_PRIMITIVES: &[&str] = &["boolean", "string", "number", "bigint", "symbol", "void"];
const JSDOC_RESERVED: &[&str] = &["import", "typeof", "extends", "new"];
const JSDOC_UNSUPPORTED: &[&str] = &[
    "import", "typeof", "extends", "new", "keyof", "infer", "asserts", "is", "readonly",
];

/// A primitive type name (with the boxed aliases), normalised.
fn jsdoc_primitive(name: &str) -> Option<JType> {
    let n = match name {
        "Boolean" => "boolean",
        "String" => "string",
        "Number" => "number",
        "Void" => "void",
        "Null" => "null",
        "Undefined" => "undefined",
        other => other,
    };
    Some(match n {
        "any" | "unknown" | "never" => JType::Unknown,
        "null" | "undefined" => JType::Nullish,
        "true" | "false" => JType::Name("boolean".into()),
        "boolean" | "string" | "number" | "bigint" | "symbol" | "void" => JType::Name(n.into()),
        _ => return None,
    })
}

fn is_id_start(c: char) -> bool {
    c == '$' || c == '_' || unicode_ident_start(c)
}

fn is_id_continue(c: char) -> bool {
    c == '$' || c == '\u{200c}' || c == '\u{200d}' || unicode_ident_continue(c)
}

fn unicode_ident_start(c: char) -> bool {
    c.is_alphabetic()
}

fn unicode_ident_continue(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn is_identifier(s: &str) -> bool {
    let mut cs = s.chars();
    cs.next().is_some_and(is_id_start) && cs.all(is_id_continue)
}

fn is_numeric_token(t: &str) -> bool {
    let b = t.as_bytes();
    let starts = b.first().is_some_and(u8::is_ascii_digit)
        || (b.first() == Some(&b'.') && b.get(1).is_some_and(u8::is_ascii_digit));
    if !starts {
        return false;
    }
    if !t.ends_with('n') {
        return true;
    }
    let lower = t.to_ascii_lowercase();
    lower.starts_with("0b")
        || lower.starts_with("0o")
        || lower.starts_with("0x")
        || !(lower.contains('.') || lower.contains('e'))
}

/// Length of a numeric literal token at the start of `s` (0 if none).
fn numeric_len(s: &str) -> usize {
    let b = s.as_bytes();
    let digits = |from: usize, ok: fn(u8) -> bool| -> usize {
        // `d(_?d)*` from `from`; returns the end (== from when no digit).
        let mut i = from;
        if i >= b.len() || !ok(b[i]) {
            return from;
        }
        i += 1;
        loop {
            if i < b.len() && ok(b[i]) {
                i += 1;
            } else if i + 1 < b.len() && b[i] == b'_' && ok(b[i + 1]) {
                i += 2;
            } else {
                return i;
            }
        }
    };
    let mut end;
    if b.len() >= 2 && b[0] == b'0' && matches!(b[1], b'x' | b'X' | b'b' | b'B' | b'o' | b'O') {
        let ok: fn(u8) -> bool = match b[1] {
            b'x' | b'X' => |c| c.is_ascii_hexdigit(),
            b'b' | b'B' => |c| c == b'0' || c == b'1',
            _ => |c| (b'0'..=b'7').contains(&c),
        };
        end = digits(2, ok);
        if end == 2 {
            // Fall back to a plain `0`.
            end = 1;
        }
    } else {
        let dec: fn(u8) -> bool = |c| c.is_ascii_digit();
        if b.first() == Some(&b'.') {
            end = digits(1, dec);
            if end == 1 {
                return 0;
            }
        } else if b.first() == Some(&b'0') {
            end = 1;
            if b.get(1) == Some(&b'.') {
                end = digits(2, dec).max(2);
            }
        } else if b.first().is_some_and(|c| (b'1'..=b'9').contains(c)) {
            end = digits(0, dec);
            if b.get(end) == Some(&b'.') {
                end = digits(end + 1, dec).max(end + 1);
            }
        } else {
            return 0;
        }
        if matches!(b.get(end), Some(b'e' | b'E')) {
            let mut j = end + 1;
            if matches!(b.get(j), Some(b'+' | b'-')) {
                j += 1;
            }
            let k = digits(j, dec);
            if k > j {
                end = k;
            }
        }
    }
    if b.get(end) == Some(&b'n') {
        end += 1;
    }
    end
}

/// Length of a quoted string token at the start of `s`, or `None` when it is not closed.
fn string_len(s: &str) -> Option<usize> {
    let mut cs = s.char_indices();
    let (_, q) = cs.next()?;
    let mut escaped = false;
    for (i, c) in cs {
        if c == '\n' || c == '\r' {
            return None;
        }
        if escaped {
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == q {
            return Some(i + 1);
        }
    }
    None
}

fn valid_string_escapes(tok: &str) -> bool {
    let inner = &tok[1..tok.len() - 1];
    let mut cs = inner.char_indices().peekable();
    while let Some((_, c)) = cs.next() {
        if c != '\\' {
            continue;
        }
        let Some((j, e)) = cs.next() else {
            break;
        };
        // The escaped character may be wider than one byte (`\é`).
        let rest = &inner[j + e.len_utf8()..];
        if e == 'x' && !(rest.len() >= 2 && rest.as_bytes()[..2].iter().all(u8::is_ascii_hexdigit))
        {
            return false;
        }
        if e == 'u' {
            let four = rest.len() >= 4 && rest.as_bytes()[..4].iter().all(u8::is_ascii_hexdigit);
            let braced = rest.strip_prefix('{').and_then(|r| {
                let end = r.find('}')?;
                let hex = &r[..end];
                (!hex.is_empty() && hex.len() <= 6 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
                    .then(|| u32::from_str_radix(hex, 16).ok())
                    .flatten()
            });
            if !four && braced.is_none_or(|v| v > 0x10_ffff) {
                return false;
            }
        }
    }
    true
}

struct TypeParser {
    toks: Vec<String>,
    breaks: Vec<usize>,
    closes: Vec<(usize, usize)>,
    i: usize,
    depth: u32,
}

type PResult = Result<JType, ()>;

impl TypeParser {
    fn tok(&self, i: usize) -> Option<&str> {
        self.toks.get(i).map(String::as_str)
    }
    fn cur(&self) -> Option<&str> {
        self.tok(self.i)
    }
    fn eat(&mut self, t: &str) -> bool {
        if self.cur() == Some(t) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn expect(&mut self, t: &str) -> Result<(), ()> {
        if self.eat(t) { Ok(()) } else { Err(()) }
    }
    fn enter(&mut self) -> Result<(), ()> {
        self.depth += 1;
        if self.depth > 64 { Err(()) } else { Ok(()) }
    }

    fn optional(&mut self) -> PResult {
        self.enter()?;
        let t = self.union()?;
        let r = if self.eat("=") {
            JType::Optional(Box::new(t))
        } else {
            t
        };
        self.depth -= 1;
        Ok(r)
    }

    fn union(&mut self) -> PResult {
        self.enter()?;
        self.eat("|");
        let mut items = vec![self.intersection()?];
        while self.eat("|") {
            items.push(self.intersection()?);
        }
        self.depth -= 1;
        Ok(if items.len() == 1 {
            items.pop().unwrap_or(JType::Unknown)
        } else {
            JType::Union(items)
        })
    }

    fn intersection(&mut self) -> PResult {
        self.eat("&");
        let mut items = vec![self.postfix()?];
        while self.eat("&") {
            items.push(self.postfix()?);
        }
        Ok(if items.len() == 1 {
            items.pop().unwrap_or(JType::Unknown)
        } else {
            JType::Intersection(items)
        })
    }

    fn postfix(&mut self) -> PResult {
        self.enter()?;
        let mut t = if self.eat("?") {
            if matches!(self.cur(), Some("?" | "!")) {
                return Err(());
            }
            if matches!(
                self.cur(),
                None | Some(")" | ">" | "]" | "}" | "," | ";" | "|" | "&")
            ) {
                JType::Unknown
            } else {
                JType::Nullable(Box::new(self.postfix()?))
            }
        } else if self.eat("!") {
            if self.cur() == Some("!") {
                return Err(());
            }
            self.postfix()?
        } else if self.eat("...") {
            self.postfix()?;
            JType::NonPrimitive
        } else if self.eat("readonly") {
            let t = self.postfix()?;
            if !matches!(&t, JType::Name(n) if n == "Array" || n == "ReadonlyArray") {
                return Err(());
            }
            t
        } else {
            self.primary()?
        };
        while self.cur() == Some("[") && self.tok(self.i + 1) == Some("]") {
            self.i += 2;
            t = JType::Name("Array".into());
        }
        self.depth -= 1;
        if self.eat("?") {
            return Ok(JType::Nullable(Box::new(t)));
        }
        self.eat("!");
        Ok(t)
    }

    fn list(
        &mut self,
        open: &str,
        close: &str,
        member: fn(&mut TypeParser) -> Result<(), ()>,
    ) -> Result<(), ()> {
        self.expect(open)?;
        if self.eat(close) {
            return Ok(());
        }
        loop {
            member(self)?;
            if self.eat(close) {
                return Ok(());
            }
            self.expect(",")?;
            if self.eat(close) {
                return Ok(());
            }
        }
    }

    fn arrow_param(&mut self) -> Result<(), ()> {
        self.eat("...");
        let t = self.cur().unwrap_or("");
        if !is_identifier(t) || JSDOC_RESERVED.contains(&t) {
            return Err(());
        }
        self.i += 1;
        self.eat("?");
        if self.eat(":") {
            self.union()?;
        }
        Ok(())
    }

    fn function_param(&mut self) -> Result<(), ()> {
        if self.eat("this") {
            self.expect(":")?;
        }
        self.optional().map(|_| ())
    }

    fn tuple_member(&mut self) -> Result<(), ()> {
        let name = self.i + usize::from(self.cur() == Some("..."));
        let colon = name
            + if self.tok(name + 1) == Some("?") {
                2
            } else {
                1
            };
        if is_identifier(self.tok(name).unwrap_or("")) && self.tok(colon) == Some(":") {
            self.i = colon + 1;
        }
        self.optional().map(|_| ())
    }

    fn record(&mut self) -> PResult {
        self.expect("{")?;
        if self.eat("}") {
            return Ok(JType::Unknown);
        }
        let mut sigs = Vec::new();
        let mut guard = 0;
        while !self.eat("}") {
            guard += 1;
            if guard > 10_000 || self.cur().is_none() {
                return Err(());
            }
            if self.cur() == Some("readonly")
                && !matches!(
                    self.tok(self.i + 1),
                    Some(":" | "?" | "(" | "," | ";" | "}")
                )
            {
                self.i += 1;
            }
            if self.cur() == Some("(") {
                self.list("(", ")", TypeParser::arrow_param)?;
                self.expect(":")?;
                sigs.push(JType::Function(Box::new(self.union()?)));
            } else if self.eat("[") {
                let t = self.cur().unwrap_or("").to_owned();
                self.i += 1;
                if !is_identifier(&t) {
                    return Err(());
                }
                self.expect(":")?;
                self.union()?;
                self.expect("]")?;
                self.expect(":")?;
                self.union()?;
            } else {
                let key = self.cur().unwrap_or("").to_owned();
                self.i += 1;
                if !is_identifier(&key) && !key.starts_with(['"', '\'']) && !is_numeric_token(&key)
                {
                    return Err(());
                }
                self.eat("?");
                if self.cur() == Some("(") {
                    self.list("(", ")", TypeParser::arrow_param)?;
                }
                if self.eat(":") {
                    self.union()?;
                }
            }
            if !self.eat(",")
                && !self.eat(";")
                && self.cur() != Some("}")
                && !self.breaks.contains(&self.i)
            {
                return Err(());
            }
        }
        Ok(match sigs.len() {
            0 => JType::NonPrimitive,
            1 => sigs.pop().unwrap_or(JType::Unknown),
            _ => JType::Intersection(sigs),
        })
    }

    fn primary(&mut self) -> PResult {
        let t = self.cur().map(str::to_owned);
        match t.as_deref() {
            Some("(") => {
                let close = self
                    .closes
                    .iter()
                    .find(|(o, _)| *o == self.i)
                    .map(|(_, c)| *c);
                if close.is_some_and(|c| self.tok(c + 1) == Some("=>")) {
                    self.list("(", ")", TypeParser::arrow_param)?;
                    self.expect("=>")?;
                    return Ok(JType::Function(Box::new(self.union()?)));
                }
                self.i += 1;
                let t = self.optional()?;
                self.expect(")")?;
                Ok(t)
            }
            Some("[") => {
                self.list("[", "]", TypeParser::tuple_member)?;
                Ok(JType::Name("Array".into()))
            }
            Some("{") => self.record(),
            Some("function") if self.tok(self.i + 1) == Some("(") => {
                self.i += 1;
                self.list("(", ")", TypeParser::function_param)?;
                Ok(JType::Function(Box::new(if self.eat(":") {
                    self.union()?
                } else {
                    JType::Unknown
                })))
            }
            Some("*") => {
                self.i += 1;
                Ok(JType::Unknown)
            }
            Some(s) if s.starts_with(['"', '\'']) => {
                self.i += 1;
                Ok(JType::Name("string".into()))
            }
            Some(s) if s == "-" || is_numeric_token_start(s) => {
                self.eat("-");
                let lit = self.cur().unwrap_or("").to_owned();
                self.i += 1;
                if !is_numeric_token(&lit) {
                    return Err(());
                }
                Ok(JType::Name(
                    if lit.ends_with('n') {
                        "bigint"
                    } else {
                        "number"
                    }
                    .into(),
                ))
            }
            _ => self.named(),
        }
    }

    fn named(&mut self) -> PResult {
        let t = self.cur().unwrap_or("").to_owned();
        if !is_identifier(&t) || JSDOC_UNSUPPORTED.contains(&t.as_str()) {
            return Err(());
        }
        self.i += 1;
        let mut name = t;
        while self.eat(".") {
            if self.cur() == Some("<") {
                break;
            }
            let part = self.cur().unwrap_or("").to_owned();
            if !is_identifier(&part) {
                return Err(());
            }
            self.i += 1;
            name.push('.');
            name.push_str(&part);
        }
        if self.cur() == Some("<") {
            if self.tok(self.i + 1) == Some(">") || jsdoc_primitive(&name).is_some() {
                return Err(());
            }
            self.list("<", ">", |p| p.union().map(|_| ()))?;
            if name == "Object" {
                return Ok(JType::NonPrimitive);
            }
        }
        Ok(jsdoc_primitive(&name).unwrap_or(JType::Name(name)))
    }
}

fn is_numeric_token_start(s: &str) -> bool {
    let b = s.as_bytes();
    b.first().is_some_and(u8::is_ascii_digit)
        || (b.first() == Some(&b'.') && b.get(1).is_some_and(u8::is_ascii_digit))
}

/// Reads a JSDoc type expression (the text between the braces).
pub(crate) fn parse_jsdoc_type(text: &str) -> JType {
    let trimmed = text.trim();
    if let Some(p) = jsdoc_primitive(trimmed) {
        return p;
    }
    if is_identifier(trimmed) {
        return if JSDOC_UNSUPPORTED.contains(&trimmed) {
            JType::Unknown
        } else {
            JType::Name(trimmed.to_owned())
        };
    }
    let mut toks: Vec<String> = Vec::new();
    let mut breaks = Vec::new();
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        let len = if c.is_whitespace() {
            let n = rest
                .char_indices()
                .find(|(_, c)| !c.is_whitespace())
                .map_or(rest.len(), |(i, _)| i);
            if rest[..n].contains(['\n', '\r', '\u{2028}', '\u{2029}']) {
                breaks.push(toks.len());
            }
            rest = &rest[n..];
            continue;
        } else if rest.starts_with("=>") {
            2
        } else if rest.starts_with("...") {
            3
        } else if is_id_start(c) {
            rest.char_indices()
                .find(|(_, c)| !is_id_continue(*c))
                .map_or(rest.len(), |(i, _)| i)
        } else if c.is_ascii_digit()
            || (c == '.' && rest[1..].starts_with(|d: char| d.is_ascii_digit()))
        {
            numeric_len(rest).max(1)
        } else if c == '"' || c == '\'' {
            match string_len(rest) {
                Some(n) => {
                    if !valid_string_escapes(&rest[..n]) {
                        return JType::Unknown;
                    }
                    n
                }
                None => return JType::Unknown,
            }
        } else {
            c.len_utf8()
        };
        toks.push(rest[..len].to_owned());
        rest = &rest[len..];
    }
    let mut closes = Vec::new();
    let mut opens: Vec<usize> = Vec::new();
    for (i, t) in toks.iter().enumerate() {
        match t.as_str() {
            "(" | "[" | "{" | "<" => opens.push(i),
            ")" | "]" | "}" | ">" => {
                let Some(o) = opens.pop() else {
                    return JType::Unknown;
                };
                let want = match toks[o].as_str() {
                    "(" => ")",
                    "[" => "]",
                    "{" => "}",
                    _ => ">",
                };
                if want != t {
                    return JType::Unknown;
                }
                if toks[o] == "(" {
                    closes.push((o, i));
                }
            }
            _ => {}
        }
    }
    if !opens.is_empty() {
        return JType::Unknown;
    }
    let mut p = TypeParser {
        toks,
        breaks,
        closes,
        i: 0,
        depth: 0,
    };
    match p.optional() {
        Ok(t) if p.i == p.toks.len() => t,
        _ => JType::Unknown,
    }
}

// ---- JSDoc tags --------------------------------------------------------------------------

/// A `@param`, `@type` or `@returns` tag.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DocTag {
    pub tag: String,
    pub name: Option<String>,
    pub ty: Option<JType>,
    pub optional: bool,
}

fn is_param_tag(t: &str) -> bool {
    matches!(t, "param" | "arg" | "argument")
}

fn is_return_tag(t: &str) -> bool {
    matches!(t, "return" | "returns")
}

/// Skips spaces and line breaks with their leading `*`.
fn skip_doc_ws(text: &[char], mut i: usize, end: usize) -> usize {
    while i < end {
        while i < end && text[i] != '\n' && text[i].is_whitespace() {
            i += 1;
        }
        if i >= end || text[i] != '\n' {
            return i;
        }
        i += 1;
        while i < end && text[i] != '\n' && text[i].is_whitespace() {
            i += 1;
        }
        if i < end && text[i] == '*' {
            i += 1;
        }
    }
    i
}

/// The index after the bracket closing the one at `start`, quotes skipped.
fn delimited_end(text: &[char], start: usize, end: usize, open: char) -> Option<usize> {
    let close = match open {
        '{' => '}',
        '[' => ']',
        _ => ')',
    };
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut i = start;
    while i < end {
        let c = text[i];
        if let Some(q) = quote {
            if c == '\\' {
                i += 2;
                continue;
            }
            if c == q {
                quote = None;
            }
        } else if matches!(c, '"' | '\'' | '`') {
            quote = Some(c);
        } else if c == open {
            depth += 1;
        } else if c == close {
            depth -= 1;
            if depth == 0 {
                return Some(i + 1);
            }
        }
        i += 1;
    }
    None
}

fn tag_type(text: &[char], start: usize, end: usize) -> Option<(JType, usize)> {
    if text.get(start) != Some(&'{') {
        return None;
    }
    let type_end = delimited_end(text, start, end, '{')?;
    let raw: String = text[start + 1..type_end - 1].iter().collect();
    // Line decorations (`\n  *`) inside a multi-line type become plain line breaks.
    let mut cleaned = String::new();
    let mut lines = raw.split('\n');
    if let Some(first) = lines.next() {
        cleaned.push_str(first.strip_suffix('\r').unwrap_or(first));
    }
    for line in lines {
        cleaned.push('\n');
        let l = line.trim_start_matches(|c: char| c != '\n' && c.is_whitespace());
        let l = l.strip_prefix('*').unwrap_or(l);
        cleaned.push_str(l);
    }
    let cleaned = cleaned.replace('\r', "");
    Some((parse_jsdoc_type(&cleaned), type_end))
}

/// The line-initial tags of a JSDoc comment body (the text between `/*` and `*/`).
pub(crate) fn doc_tags(value: &str) -> Vec<DocTag> {
    let text: Vec<char> = value.chars().collect();
    // (tag name, line start, content start)
    let mut found: Vec<(String, usize, usize)> = Vec::new();
    let mut line_start = 0;
    while line_start < text.len() {
        let line_end = text[line_start..]
            .iter()
            .position(|&c| c == '\n')
            .map_or(text.len(), |p| line_start + p);
        let mut i = line_start;
        while i < line_end && text[i].is_whitespace() {
            i += 1;
        }
        if i < line_end && text[i] == '*' {
            i += 1;
            while i < line_end && text[i].is_whitespace() {
                i += 1;
            }
        }
        if i < line_end && text[i] == '@' && text.get(i + 1).is_some_and(|c| c.is_alphanumeric()) {
            let mut j = i + 2;
            while j < line_end && (text[j].is_alphanumeric() || text[j] == '-') {
                j += 1;
            }
            if j < line_end && text[j] == '!' {
                j += 1;
            }
            let next_ok = j >= line_end || text[j].is_whitespace() || text[j] == '{';
            if next_ok {
                let name: String = text[i + 1..j].iter().collect::<String>().to_lowercase();
                let name = name.strip_suffix('!').unwrap_or(&name).to_owned();
                found.push((name, line_start, j));
            }
        }
        line_start = line_end + 1;
    }
    if found
        .iter()
        .any(|(n, _, _)| matches!(n.as_str(), "typedef" | "callback" | "overload"))
    {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (k, (name, _, content)) in found.iter().enumerate() {
        let end = found.get(k + 1).map_or(text.len(), |n| n.1);
        if let Some(t) = parse_tag(&text, name, *content, end) {
            out.push(t);
        }
    }
    out
}

fn parse_tag(text: &[char], tag: &str, content: usize, end: usize) -> Option<DocTag> {
    let is_param = is_param_tag(tag);
    if !is_param && tag != "type" && !is_return_tag(tag) {
        return None;
    }
    let bare = DocTag {
        tag: tag.to_owned(),
        name: None,
        ty: None,
        optional: false,
    };
    let mut start = skip_doc_ws(text, content, end);
    let mut ty = None;
    if text.get(start) == Some(&'{') {
        let Some((t, type_end)) = tag_type(text, start, end) else {
            return Some(bare);
        };
        ty = Some(t);
        start = skip_doc_ws(text, type_end, end);
    } else if !is_param {
        return Some(bare);
    }
    if !is_param {
        return Some(DocTag { ty, ..bare });
    }
    let optional = text.get(start) == Some(&'[');
    let name_end = if optional {
        delimited_end(text, start, end, '[')?
    } else {
        end
    };
    if name_end < end && !text[name_end].is_whitespace() {
        return None;
    }
    if optional {
        start += 1;
    }
    let mut j = start;
    if j < name_end && is_id_start(text[j]) {
        j += 1;
        while j < name_end && is_id_continue(text[j]) {
            j += 1;
        }
    }
    if j == start {
        return None;
    }
    let name: String = text[start..j].iter().collect();
    let next = skip_doc_ws(text, j, name_end);
    if optional {
        if !matches!(text.get(next), Some(']' | '=')) {
            return None;
        }
    } else if j < end && !text[j].is_whitespace() {
        return None;
    }
    if ty.is_none() {
        let ts = skip_doc_ws(text, if optional { name_end } else { j }, end);
        ty = tag_type(text, ts, end).map(|(t, _)| t);
    }
    Some(DocTag {
        tag: tag.to_owned(),
        name: Some(name),
        ty,
        optional,
    })
}

/// A JSDoc annotation picked for a binding, parameter or function.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DocAnnotation {
    pub ty: Option<JType>,
    pub optional: bool,
}

/// True when the file has any `/** */` comment.
pub(crate) fn has_jsdoc_comments(cx: &Ctx<'_, '_>) -> bool {
    let src = cx.src();
    cx.ast().comments.iter().any(|c| {
        c.kind == CommentKind::Block && {
            let t = c.span.text(src);
            t.len() > 4 && t.starts_with("/**")
        }
    })
}

/// The comment right before `id` (only whitespace between), as ESLint's
/// `getCommentsBefore(node).at(-1)`.
fn comment_before(cx: &Ctx<'_, '_>, id: NodeId) -> Option<ts_r_ast::Comment> {
    let ast = cx.ast();
    let src = cx.src();
    let start = cx.span(id).start;
    let idx = ast.comments.partition_point(|c| c.span.end <= start);
    let c = *ast.comments.get(idx.checked_sub(1)?)?;
    let between = &src[c.span.end as usize..start as usize];
    between.chars().all(char::is_whitespace).then_some(c)
}

/// The tags of the documentation comment attached to the first of `nodes` that has a
/// comment right before it (none when that comment isn't a JSDoc block).
fn documentation_tags(cx: &Ctx<'_, '_>, nodes: &[NodeId]) -> Vec<DocTag> {
    for &n in nodes {
        if let Some(c) = comment_before(cx, n) {
            let t = c.span.text(cx.src());
            if c.kind == CommentKind::Block && t.starts_with("/**") && t.len() > 4 {
                return doc_tags(&t[2..t.len() - 2]);
            }
            return Vec::new();
        }
    }
    Vec::new()
}

/// The one tag matching `pred` (none when zero or several match).
fn single_tag(tags: &[DocTag], pred: impl Fn(&DocTag) -> bool) -> Option<DocAnnotation> {
    let mut it = tags.iter().filter(|t| pred(t));
    let first = it.next()?;
    if it.next().is_some() {
        return None;
    }
    Some(DocAnnotation {
        ty: first.ty.clone(),
        optional: first.optional,
    })
}

fn is_function_node(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(id),
        Node::FunctionDecl { .. } | Node::FunctionExpr { .. } | Node::Arrow { .. }
    )
}

fn is_ts_wrapper(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(id),
        Node::As { .. }
            | Node::Satisfies { .. }
            | Node::NonNull { .. }
            | Node::TypeAssertion { .. }
            | Node::Instantiation { .. }
    )
}

/// The nodes whose comments document a function: the function, the TS wrappers around it,
/// and the declarator / declaration / assignment / property holding it.
fn function_owners(cx: &Ctx<'_, '_>, f: NodeId) -> Vec<NodeId> {
    let ast = cx.ast();
    let mut owners = vec![f];
    let mut node = f;
    while let Some(p) = parent_np(cx, node) {
        if is_ts_wrapper(ast, p) {
            owners.push(p);
            node = p;
        } else {
            break;
        }
    }
    let Some(parent) = parent_np(cx, node) else {
        return owners;
    };
    match *ast.node(parent) {
        Node::VarDeclarator { init: Some(i), .. } if skip_parens(ast, i) == node => {
            owners.push(parent);
            if let Some(decl) = cx.parent(parent)
                && let Node::VarDecl { decls, .. } = *ast.node(decl)
                && ast.list(decls).len() == 1
            {
                owners.push(decl);
            }
        }
        Node::Assign {
            op: AssignOp::Assign,
            right,
            ..
        } if skip_parens(ast, right) == node => {
            if let Some(stmt) = parent_np(cx, parent)
                && matches!(ast.node(stmt), Node::ExprStmt { .. })
            {
                owners.push(parent);
                owners.push(stmt);
            }
        }
        Node::Property { value, .. }
        | Node::PropertyDef {
            value: Some(value), ..
        } if skip_parens(ast, value) == node => {
            owners.push(parent);
        }
        Node::MethodDef { value, .. } if value == node => owners.push(parent),
        Node::ExportDefaultExpr { .. } => owners.push(parent),
        _ => {}
    }
    owners
}

/// A wrapper chain with an `as` or `<T>` assertion (which then wins over JSDoc).
pub(crate) fn has_native_type_assertion(ast: &Ast, mut id: NodeId) -> bool {
    loop {
        match *ast.node(id) {
            Node::As { .. } | Node::TypeAssertion { .. } => return true,
            Node::Satisfies { expr, .. }
            | Node::NonNull { expr }
            | Node::Instantiation { expr, .. }
            | Node::Paren { expr } => id = expr,
            _ => return false,
        }
    }
}

/// `(function, Param)` when `ident` is a plain (non-destructured, non-rest) parameter name.
fn plain_param(cx: &Ctx<'_, '_>, ident: NodeId) -> Option<(NodeId, NodeId)> {
    let ast = cx.ast();
    let p = cx.parent(ident)?;
    let Node::Param {
        pattern,
        rest: false,
        ..
    } = *ast.node(p)
    else {
        return None;
    };
    if pattern != ident {
        return None;
    }
    let f = cx.parent(p)?;
    is_function_node(ast, f).then_some((f, p))
}

/// The annotation of a parameter: an inline `/** @type {T} */` before it, or the function's
/// `@param {T} name`.
fn param_annotation(cx: &Ctx<'_, '_>, ident: NodeId) -> Option<DocAnnotation> {
    if !has_jsdoc_comments(cx) {
        return None;
    }
    let ast = cx.ast();
    let (f, param) = plain_param(cx, ident)?;
    let name = ident_name(ast, ident)?;
    let inline = single_tag(&documentation_tags(cx, &[ident, param]), |t| {
        t.tag == "type"
    });
    let named = single_tag(&documentation_tags(cx, &function_owners(cx, f)), |t| {
        is_param_tag(&t.tag) && t.name.as_deref() == Some(name)
    });
    let opt_kind = |a: &Option<DocAnnotation>| {
        a.as_ref()
            .is_some_and(|a| a.optional || matches!(a.ty, Some(JType::Optional(_))))
    };
    let any_optional = opt_kind(&inline) || opt_kind(&named);
    let chosen = match &inline {
        Some(i) if i.ty.is_some() => inline.clone(),
        _ => named.clone(),
    }?;
    let ty = match chosen.ty {
        Some(JType::Optional(e)) => Some(*e),
        t => t,
    };
    let Node::Param { optional, .. } = *ast.node(param) else {
        return None;
    };
    Some(DocAnnotation {
        ty,
        optional: optional || any_optional,
    })
}

/// The TS annotation written on a binding name, and whether the binding is optional.
pub(crate) fn binding_annotation(cx: &Ctx<'_, '_>, name: NodeId) -> (Option<NodeId>, bool) {
    let ast = cx.ast();
    let Some(p) = cx.parent(name) else {
        return (None, false);
    };
    match *ast.node(p) {
        Node::Param {
            pattern,
            type_ann,
            optional,
            ..
        } if pattern == name => (type_ann, optional),
        Node::VarDeclarator {
            name: n, type_ann, ..
        } if n == name => (type_ann, false),
        Node::CatchClause {
            param: Some(n),
            type_ann,
            ..
        } if n == name => (type_ann, false),
        _ => (None, false),
    }
}

/// `getJSDocumentTypeAnnotation`: the documented type of a binding (a parameter, a
/// declarator name, or a reference resolved to one) or of a property.
pub(crate) fn jsdoc_type_annotation(cx: &Ctx<'_, '_>, id: NodeId) -> Option<DocAnnotation> {
    jsdoc_type_annotation_depth(cx, id, 0)
}

fn jsdoc_type_annotation_depth(cx: &Ctx<'_, '_>, id: NodeId, depth: u32) -> Option<DocAnnotation> {
    if depth > 8 || !has_jsdoc_comments(cx) {
        return None;
    }
    let ast = cx.ast();
    let owners = match *ast.node(id) {
        Node::Property { value, .. } => {
            if has_native_type_assertion(ast, value) {
                return None;
            }
            vec![id]
        }
        Node::PropertyDef {
            type_ann, value, ..
        } => {
            if type_ann.is_some() || value.is_some_and(|v| has_native_type_assertion(ast, v)) {
                return None;
            }
            vec![id]
        }
        Node::Ident { .. } => {
            if binding_annotation(cx, id).0.is_some() {
                return None;
            }
            let parent = cx.parent(id);
            match parent.map(|p| (p, ast.node(p))) {
                Some((p, &Node::VarDeclarator { name, init, .. })) if name == id => {
                    if init.is_some_and(|i| has_native_type_assertion(ast, i)) {
                        return None;
                    }
                    let mut owners = vec![id, p];
                    if let Some(decl) = cx.parent(p)
                        && let Node::VarDecl { decls, .. } = *ast.node(decl)
                        && ast.list(decls).len() == 1
                    {
                        owners.push(decl);
                    }
                    owners
                }
                _ => {
                    if let Some(a) = param_annotation(cx, id)
                        && a.ty.is_some()
                    {
                        return Some(a);
                    }
                    let sem = cx.semantic();
                    let r = sem.reference_at(id)?;
                    let s = sem.reference(r).symbol()?;
                    let [decl] = sem.symbol(s).decls.as_slice() else {
                        return None;
                    };
                    if decl.name == id {
                        return None;
                    }
                    return jsdoc_type_annotation_depth(cx, decl.name, depth + 1);
                }
            }
        }
        _ => return None,
    };
    let a = single_tag(&documentation_tags(cx, &owners), |t| t.tag == "type")?;
    a.ty.is_some().then_some(a)
}

/// `getJSDocumentReturnType`: the documented return type of a function.
pub(crate) fn jsdoc_return_type(cx: &Ctx<'_, '_>, f: NodeId) -> Option<DocAnnotation> {
    if !has_jsdoc_comments(cx) {
        return None;
    }
    let ast = cx.ast();
    let (Node::FunctionDecl { return_type, .. }
    | Node::FunctionExpr { return_type, .. }
    | Node::Arrow { return_type, .. }) = *ast.node(f)
    else {
        return None;
    };
    if return_type.is_some() {
        return None;
    }
    let owners = function_owners(cx, f);
    if owners.iter().any(|&o| match *ast.node(o) {
        Node::VarDeclarator { type_ann, .. } | Node::PropertyDef { type_ann, .. } => {
            type_ann.is_some()
        }
        _ => false,
    }) {
        return None;
    }
    let getter = cx.parent(f).is_some_and(|p| {
        matches!(ast.node(p), Node::MethodDef { kind: MethodKind::Get, value, .. } if *value == f)
    });
    let a = single_tag(&documentation_tags(cx, &owners), |t| {
        is_return_tag(&t.tag) || (t.tag == "type" && getter)
    })?;
    a.ty.is_some().then_some(a)
}

/// `hasJSDocumentTypeReference`: the expression reads a binding with a JSDoc type
/// (directly or through `const` aliases), which then wins over static inference.
pub(crate) fn has_jsdoc_type_reference(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    if !has_jsdoc_comments(cx) {
        return false;
    }
    jsdoc_ref(cx, id, &mut Vec::new(), 0)
}

fn jsdoc_ref(cx: &Ctx<'_, '_>, id: NodeId, visited: &mut Vec<SymbolId>, depth: u32) -> bool {
    if depth > 64 {
        return false;
    }
    let ast = cx.ast();
    let id = skip_ts_only(ast, id);
    match *ast.node(id) {
        Node::Ident { .. } => {
            if jsdoc_type_annotation(cx, id).is_some() {
                return true;
            }
            let sem = cx.semantic();
            let Some(s) = sem.symbol_of_ident(id) else {
                return false;
            };
            let [decl] = sem.symbol(s).decls.as_slice() else {
                return false;
            };
            let Node::VarDeclarator {
                name,
                init: Some(init),
                ..
            } = *ast.node(decl.node)
            else {
                return false;
            };
            if name != decl.name || visited.contains(&s) {
                return false;
            }
            visited.push(s);
            jsdoc_ref(cx, init, visited, depth + 1)
        }
        Node::FunctionExpr { .. } | Node::Arrow { .. } | Node::ClassExpr { .. } => false,
        _ => {
            let mut found = false;
            for c in evaluated_children(ast, id) {
                if !found && jsdoc_ref(cx, c, visited, depth + 1) {
                    found = true;
                }
            }
            found
        }
    }
}

// ---- The guessing engine -------------------------------------------------------------------

/// The answer to "is this the target type?".
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Guess {
    Target,
    NonTarget,
    /// `null` / `undefined` (counts as non-target outside unions).
    Nullish,
    Unknown,
}

/// A node-shape hook: `recurse` asks the same checker about another node.
pub(crate) type TargetNodeFn = fn(&Ctx<'_, '_>, NodeId, &dyn Fn(NodeId) -> bool) -> bool;

/// One type question and its policies (the plugin's `createTypeCheckers` options).
pub(crate) struct Checker {
    /// Type names (annotations, JSDoc, classes) that are the target.
    pub target_names: &'static [&'static str],
    /// Type names known not to be it.
    pub non_target_names: &'static [&'static str],
    /// `(alias, target name)`: `ReadonlyMap` for `Map`.
    pub aliases: &'static [(&'static str, &'static str)],
    /// `(module, names)` whose named imports are the target (other names from those modules
    /// are not).
    pub target_imports: &'static [(&'static str, &'static [&'static str])],
    /// The same for `ns.Name` through a namespace or default import.
    pub target_namespace_imports: &'static [(&'static str, &'static [&'static str])],
    /// `Name(...)` calls that return the target.
    pub target_calls: &'static [&'static str],
    /// `new Name(...)` expressions that create the target.
    pub target_ctors: &'static [&'static str],
    pub target_node: Option<TargetNodeFn>,
    pub non_target_node: Option<fn(&Ctx<'_, '_>, NodeId) -> bool>,
    /// A TS annotation node that is the target (`number`, `5`).
    pub target_annotation: Option<fn(&Ast, NodeId) -> bool>,
    /// A JSDoc type that is the target.
    pub target_jsdoc: Option<fn(&JType) -> bool>,
    /// What a statically known value says.
    pub static_type: Option<fn(&Static, &Ast, NodeId) -> Guess>,
    /// Follow `class X extends Y` to `Y`.
    pub class_heritage: bool,
    /// Answer for `this`, `super` and `new X()` from class declarations.
    pub class_syntax: bool,
    /// Resolve a local type definition before trusting the name.
    pub prefer_definitions: bool,
    /// A union is the target when any member is.
    pub mixed_union_target: bool,
    /// A union is not the target when any member isn't.
    pub mixed_union_non_target: bool,
    /// `null`/`undefined` members don't count in unions.
    pub nullish_in_mixed_union: bool,
}

impl Checker {
    /// The defaults (`checkClassHeritage` on, everything else off).
    pub(crate) const DEFAULT: Checker = Checker {
        target_names: &[],
        non_target_names: &[],
        aliases: &[],
        target_imports: &[],
        target_namespace_imports: &[],
        target_calls: &[],
        target_ctors: &[],
        target_node: None,
        non_target_node: None,
        target_annotation: None,
        target_jsdoc: None,
        static_type: None,
        class_heritage: true,
        class_syntax: false,
        prefer_definitions: false,
        mixed_union_target: false,
        mixed_union_non_target: false,
        nullish_in_mixed_union: false,
    };

    /// The plain answer (`Nullish` folded into `NonTarget`).
    pub(crate) fn guess(&self, cx: &Ctx<'_, '_>, id: NodeId) -> Guess {
        normalize(Engine::new(cx, self).ty(id))
    }

    pub(crate) fn is_target(&self, cx: &Ctx<'_, '_>, id: NodeId) -> bool {
        Engine::new(cx, self).ty(id) == Guess::Target
    }

    pub(crate) fn is_known_non_target(&self, cx: &Ctx<'_, '_>, id: NodeId) -> bool {
        self.guess(cx, id) == Guess::NonTarget
    }

    /// Classifies a TS type annotation node.
    pub(crate) fn annotation(&self, cx: &Ctx<'_, '_>, ty: NodeId) -> Guess {
        let e = Engine::new(cx, self);
        let scope = cx.semantic().enclosing_scope(ty);
        e.annotation(ty, scope)
    }

    /// Classifies a JSDoc annotation.
    pub(crate) fn jsdoc(&self, a: &DocAnnotation) -> Guess {
        jsdoc_guess(self, a)
    }
}

fn normalize(g: Guess) -> Guess {
    if g == Guess::Nullish {
        Guess::NonTarget
    } else {
        g
    }
}

fn combine_union(ck: &Checker, types: Vec<Guess>) -> Guess {
    let types: Vec<Guess> = if ck.nullish_in_mixed_union {
        types.into_iter().filter(|t| *t != Guess::Nullish).collect()
    } else {
        types.into_iter().map(normalize).collect()
    };
    if types.is_empty() {
        return Guess::NonTarget;
    }
    let target = if ck.mixed_union_target {
        types.contains(&Guess::Target)
    } else {
        types.iter().all(|t| *t == Guess::Target)
    };
    if target {
        return Guess::Target;
    }
    let non = if ck.mixed_union_non_target {
        types.contains(&Guess::NonTarget)
    } else {
        types.iter().all(|t| *t == Guess::NonTarget)
    };
    if non {
        Guess::NonTarget
    } else {
        Guess::Unknown
    }
}

fn combine_intersection(types: Vec<Guess>) -> Guess {
    let types: Vec<Guess> = types.into_iter().map(normalize).collect();
    if types.contains(&Guess::Unknown) {
        return Guess::Unknown;
    }
    if types.contains(&Guess::Target) {
        Guess::Target
    } else {
        Guess::NonTarget
    }
}

fn known_name(ck: &Checker, name: &str) -> Guess {
    let alias = ck.aliases.iter().find(|(a, _)| *a == name).map(|(_, t)| *t);
    if ck.target_names.contains(&name) || alias.is_some_and(|a| ck.target_names.contains(&a)) {
        return Guess::Target;
    }
    if ck.non_target_names.contains(&name) {
        Guess::NonTarget
    } else {
        Guess::Unknown
    }
}

fn jsdoc_members(t: &JType) -> Vec<&JType> {
    match t {
        JType::Union(items) => items
            .iter()
            .flat_map(|i| {
                if matches!(i, JType::Union(_)) {
                    jsdoc_members(i)
                } else {
                    vec![i]
                }
            })
            .collect(),
        JType::Intersection(items) => items
            .iter()
            .flat_map(|i| {
                if matches!(i, JType::Intersection(_)) {
                    jsdoc_members(i)
                } else {
                    vec![i]
                }
            })
            .collect(),
        other => vec![other],
    }
}

fn jsdoc_node_guess(ck: &Checker, t: Option<&JType>, depth: u32) -> Guess {
    if depth > 64 {
        return Guess::Unknown;
    }
    let Some(t) = t else {
        return Guess::Unknown;
    };
    match t {
        JType::Nullable(e) | JType::Optional(e) => combine_union(
            ck,
            vec![jsdoc_node_guess(ck, Some(e), depth + 1), Guess::Nullish],
        ),
        JType::Union(_) => combine_union(
            ck,
            jsdoc_members(t)
                .into_iter()
                .map(|m| jsdoc_node_guess(ck, Some(m), depth + 1))
                .collect(),
        ),
        JType::Intersection(items) => combine_intersection(
            items
                .iter()
                .map(|m| jsdoc_node_guess(ck, Some(m), depth + 1))
                .collect(),
        ),
        JType::Nullish => Guess::Nullish,
        _ => {
            if ck.target_jsdoc.is_some_and(|f| f(t)) {
                return Guess::Target;
            }
            if let JType::Name(n) = t {
                let k = known_name(ck, n);
                if k != Guess::Unknown {
                    return k;
                }
                if JSDOC_PRIMITIVES.contains(&n.as_str()) {
                    return Guess::NonTarget;
                }
            }
            if matches!(t, JType::NonPrimitive | JType::Function(_)) {
                Guess::NonTarget
            } else {
                Guess::Unknown
            }
        }
    }
}

fn jsdoc_guess(ck: &Checker, a: &DocAnnotation) -> Guess {
    let g = jsdoc_node_guess(ck, a.ty.as_ref(), 0);
    if a.optional {
        combine_union(ck, vec![g, Guess::Nullish])
    } else {
        g
    }
}

/// The return type of a documented callable (`() => T`, unions and intersections of them).
pub(crate) fn jsdoc_callable_return(t: &JType) -> Option<JType> {
    match t {
        JType::Function(r) => Some((**r).clone()),
        JType::Union(_) | JType::Intersection(_) => {
            let members: Vec<&JType> = if matches!(t, JType::Intersection(_)) {
                jsdoc_members(t)
                    .into_iter()
                    .filter(|m| !matches!(m, JType::NonPrimitive))
                    .collect()
            } else {
                match t {
                    JType::Union(items) => items.iter().collect(),
                    _ => Vec::new(),
                }
            };
            let mut rets = Vec::new();
            for m in members {
                rets.push(jsdoc_callable_return(m)?);
            }
            if rets.is_empty() {
                return None;
            }
            Some(JType::Union(rets))
        }
        _ => None,
    }
}

struct Engine<'c, 'f, 'a> {
    cx: &'c Ctx<'f, 'a>,
    ck: &'c Checker,
    visited: RefCell<Vec<SymbolId>>,
    depth: Cell<u32>,
}

const ENGINE_DEPTH: u32 = 48;

impl<'c, 'f, 'a> Engine<'c, 'f, 'a> {
    fn new(cx: &'c Ctx<'f, 'a>, ck: &'c Checker) -> Self {
        Engine {
            cx,
            ck,
            visited: RefCell::new(Vec::new()),
            depth: Cell::new(0),
        }
    }

    fn ast(&self) -> &'a Ast {
        self.cx.ast()
    }

    /// `getType`.
    fn ty(&self, id: NodeId) -> Guess {
        let d = self.depth.get();
        if d > ENGINE_DEPTH {
            return Guess::Unknown;
        }
        self.depth.set(d + 1);
        let r = self.ty_inner(id);
        self.depth.set(d);
        r
    }

    fn ty_inner(&self, id: NodeId) -> Guess {
        let cx = self.cx;
        let ast = self.ast();
        let id = skip_parens(ast, id);
        if let Node::Satisfies { expr, .. } = *ast.node(id) {
            return self.ty(expr);
        }
        match *ast.node(id) {
            Node::As { ty, .. } | Node::TypeAssertion { ty, .. } => {
                let g = self.annotation(ty, cx.semantic().enclosing_scope(id));
                if g != Guess::Unknown {
                    return g;
                }
            }
            _ => {
                if let Some(a) = jsdoc_type_annotation(cx, id) {
                    return jsdoc_guess(self.ck, &a);
                }
            }
        }
        let g = self.expression_guess(id);
        if g != Guess::Unknown {
            return g;
        }
        if !matches!(ast.node(id), Node::Ident { .. }) {
            let g = self.static_guess(id);
            if g != Guess::Unknown {
                return g;
            }
        }
        if let Some(f) = self.ck.target_node {
            let recurse = |n: NodeId| self.ty(n) == Guess::Target;
            if f(cx, id, &recurse) {
                return Guess::Target;
            }
        }
        if self.ck.target_ctors.iter().any(|n| is_new_of(ast, id, n))
            || self.ck.target_calls.iter().any(|n| is_call_of(ast, id, n))
        {
            return Guess::Target;
        }
        if self.ck.non_target_node.is_some_and(|f| f(cx, id)) {
            return Guess::NonTarget;
        }
        Guess::Unknown
    }

    fn static_guess(&self, id: NodeId) -> Guess {
        let Some(f) = self.ck.static_type else {
            return Guess::Unknown;
        };
        let Some(v) = static_value_control_flow(self.cx, id) else {
            return Guess::Unknown;
        };
        let g = f(&v, self.ast(), id);
        if g != Guess::Unknown && has_jsdoc_type_reference(self.cx, id) {
            return Guess::Unknown;
        }
        g
    }

    fn expression_guess(&self, id: NodeId) -> Guess {
        let ast = self.ast();
        match *ast.node(id) {
            Node::Ident { .. } => self.variable_guess(id),
            Node::Call { .. } => self.call_guess(id),
            Node::Assign {
                op: AssignOp::Assign,
                right,
                ..
            } => self.ty(right),
            Node::Assign { .. } => Guess::Unknown,
            Node::This {} if self.ck.class_syntax => self.this_type(id),
            Node::Super {} if self.ck.class_syntax => self.super_type(id),
            Node::New { callee, .. } if self.ck.class_syntax => {
                let scope = self.cx.semantic().enclosing_scope(id);
                self.class_ref(callee, scope)
            }
            Node::As { expr, ty } | Node::TypeAssertion { expr, ty } => {
                let g = self.annotation(ty, self.cx.semantic().enclosing_scope(id));
                if g == Guess::Unknown
                    && matches!(
                        ast.node(ty),
                        Node::KeywordType {
                            kind: KeywordTypeKind::Any | KeywordTypeKind::Unknown
                        }
                    )
                {
                    self.ty(expr)
                } else {
                    g
                }
            }
            Node::NonNull { expr } | Node::Paren { expr } => self.ty(expr),
            Node::Seq { exprs } => ast
                .list(exprs)
                .last()
                .map_or(Guess::Unknown, |&e| self.ty(e)),
            Node::Binary {
                left,
                op: BinaryOp::LogicalOr | BinaryOp::LogicalAnd | BinaryOp::Coalesce,
                right,
            } => combine_union(self.ck, vec![self.ty(left), self.ty(right)]),
            Node::Conditional { cons, alt, .. } => {
                combine_union(self.ck, vec![self.ty(cons), self.ty(alt)])
            }
            _ => Guess::Unknown,
        }
    }

    fn variable_guess(&self, id: NodeId) -> Guess {
        let cx = self.cx;
        let ast = self.ast();
        let sem = cx.semantic();
        let Some(r) = sem.reference_at(id) else {
            return Guess::Unknown;
        };
        let Some(s) = sem.reference(r).symbol() else {
            return Guess::Unknown;
        };
        let sym = sem.symbol(s);
        if self.visited.borrow().contains(&s) || sym.decls.len() != 1 {
            return Guess::Unknown;
        }
        if sym.flags.contains(SymbolFlags::COMMENT_GLOBAL) {
            return Guess::Unknown;
        }
        self.visited.borrow_mut().push(s);
        let decl = sym.decls[0];
        let (ann, optional) = binding_annotation(cx, decl.name);
        let mut from_ann = match ann {
            Some(t) => self.annotation(t, sem.enclosing_scope(decl.name)),
            None => Guess::Unknown,
        };
        if optional {
            from_ann = combine_union(self.ck, vec![from_ann, Guess::Nullish]);
        }
        let g = if from_ann != Guess::Unknown {
            from_ann
        } else if decl.kind == DeclKind::Const
            && let Node::VarDeclarator {
                name,
                init: Some(init),
                ..
            } = *ast.node(decl.node)
            && name == decl.name
        {
            self.ty(init)
        } else {
            Guess::Unknown
        };
        self.visited.borrow_mut().retain(|x| *x != s);
        g
    }

    fn call_guess(&self, call: NodeId) -> Guess {
        let cx = self.cx;
        let ast = self.ast();
        let Node::Call { callee, .. } = *ast.node(call) else {
            return Guess::Unknown;
        };
        let callee = skip_parens(ast, callee);
        if !matches!(ast.node(callee), Node::Ident { .. }) {
            return Guess::Unknown;
        }
        let sem = cx.semantic();
        let Some(s) = sem
            .reference_at(callee)
            .and_then(|r| sem.reference(r).symbol())
        else {
            return Guess::Unknown;
        };
        let sym = sem.symbol(s);
        if sym.decls.len() != 1
            || sym.refs.iter().any(|&r| {
                let r = sem.reference(r);
                r.is_write() && !r.flags.contains(RefFlags::INIT)
            })
        {
            return Guess::Unknown;
        }
        let decl = sym.decls[0];
        if binding_annotation(cx, decl.name).0.is_some() {
            return Guess::Unknown;
        }
        let initializer = definition_initializer(cx, &decl);
        let value = if decl.kind == DeclKind::Function {
            Some(decl.node)
        } else {
            initializer.map(|i| skip_ts_only(ast, i))
        };
        if let Some(v) = value {
            let (async_gen, has_ret) = fn_flags(ast, v);
            if async_gen
                || (decl.kind == DeclKind::Param
                    && (has_ret || initializer.is_some_and(|i| has_native_type_assertion(ast, i))))
            {
                return Guess::Unknown;
            }
        }
        let function = if decl.kind == DeclKind::Function {
            Some(decl.node)
        } else if matches!(decl.kind, DeclKind::Var | DeclKind::Let | DeclKind::Const)
            && let Node::VarDeclarator {
                name,
                init: Some(init),
                ..
            } = *ast.node(decl.node)
            && name == decl.name
        {
            let mut i = init;
            while let Node::Satisfies { expr, .. } | Node::NonNull { expr } | Node::Paren { expr } =
                *ast.node(i)
            {
                i = expr;
            }
            is_function_node(ast, i).then_some(i)
        } else {
            None
        };
        if let Some(f) = function {
            let (Node::FunctionDecl {
                type_params,
                return_type,
                ..
            }
            | Node::FunctionExpr {
                type_params,
                return_type,
                ..
            }
            | Node::Arrow {
                type_params,
                return_type,
                ..
            }) = *ast.node(f)
            else {
                return Guess::Unknown;
            };
            if type_params.is_some() {
                return Guess::Unknown;
            }
            if let Some(rt) = return_type {
                let scope = outer_scope(cx, f);
                return self.annotation(rt, scope);
            }
        }
        if let Some(a) = jsdoc_type_annotation(cx, decl.name) {
            if a.optional {
                return Guess::Unknown;
            }
            let ret = a.ty.as_ref().and_then(jsdoc_callable_return);
            return jsdoc_guess(
                self.ck,
                &DocAnnotation {
                    ty: ret,
                    optional: false,
                },
            );
        }
        match function.and_then(|f| jsdoc_return_type(cx, f)) {
            Some(a) => jsdoc_guess(self.ck, &a),
            None => Guess::Unknown,
        }
    }

    /// `getTypeAnnotationType`.
    fn annotation(&self, ty: NodeId, scope: ScopeId) -> Guess {
        let d = self.depth.get();
        if d > ENGINE_DEPTH {
            return Guess::Unknown;
        }
        self.depth.set(d + 1);
        let r = self.annotation_inner(ty, scope);
        self.depth.set(d);
        r
    }

    fn annotation_inner(&self, ty: NodeId, scope: ScopeId) -> Guess {
        let ast = self.ast();
        match *ast.node(ty) {
            Node::ParenType { ty } => self.annotation(ty, scope),
            Node::TypeOperator { op, ty } => {
                if op == ts_r_ast::TypeOperatorKind::Readonly {
                    self.annotation(ty, scope)
                } else {
                    Guess::Unknown
                }
            }
            Node::TypeRef { .. } => self.type_ref(ty, scope),
            Node::UnionType { types } => combine_union(
                self.ck,
                ast.list(types)
                    .iter()
                    .map(|&t| self.annotation(t, scope))
                    .collect(),
            ),
            Node::IntersectionType { types } => combine_intersection(
                ast.list(types)
                    .iter()
                    .map(|&t| self.annotation(t, scope))
                    .collect(),
            ),
            _ => {
                if self.ck.target_annotation.is_some_and(|f| f(ast, ty)) {
                    return Guess::Target;
                }
                if matches!(
                    ast.node(ty),
                    Node::KeywordType {
                        kind: KeywordTypeKind::Null | KeywordTypeKind::Undefined
                    }
                ) {
                    return Guess::Nullish;
                }
                if is_non_target_annotation(ast, ty) {
                    Guess::NonTarget
                } else {
                    Guess::Unknown
                }
            }
        }
    }

    fn type_ref(&self, ty: NodeId, scope: ScopeId) -> Guess {
        let ast = self.ast();
        let Node::TypeRef {
            name: type_name, ..
        } = *ast.node(ty)
        else {
            return Guess::Unknown;
        };
        let Some(name) = type_name_text(ast, type_name) else {
            return Guess::Unknown;
        };
        let ns = self.namespace_import_type(type_name, scope);
        if ns != Guess::Unknown {
            return ns;
        }
        if !self.ck.prefer_definitions {
            let k = known_name(self.ck, &name);
            if k != Guess::Unknown {
                return k;
            }
        }
        let Some((sym, decl)) = type_definition(self.cx, &name, scope) else {
            return known_name(self.ck, &name);
        };
        if self.visited.borrow().contains(&sym) {
            return Guess::Unknown;
        }
        self.visited.borrow_mut().push(sym);
        let def_scope = self.cx.semantic().symbol(sym).scope;
        let import = self.import_binding_type(&decl);
        let g = if import != Guess::Unknown {
            import
        } else {
            match *ast.node(decl.node) {
                Node::TypeAlias { ty, .. } => self.annotation(ty, def_scope),
                Node::TypeParam {
                    constraint: Some(c),
                    ..
                } => self.annotation(c, def_scope),
                Node::Interface { extends, .. } => {
                    let ext = ast.list(extends);
                    if ext.is_empty() {
                        Guess::NonTarget
                    } else {
                        combine_intersection(
                            ext.iter()
                                .map(|&h| self.heritage_ref(h, def_scope))
                                .collect(),
                        )
                    }
                }
                Node::ClassDecl { .. } | Node::ClassExpr { .. } if decl.kind == DeclKind::Class => {
                    self.class_heritage(decl.node, def_scope, true)
                }
                _ => Guess::Unknown,
            }
        };
        self.visited.borrow_mut().retain(|x| *x != sym);
        g
    }

    /// An interface `extends` entry, read as a type reference.
    fn heritage_ref(&self, h: NodeId, scope: ScopeId) -> Guess {
        let ast = self.ast();
        let Node::ExprWithTypeArgs { expr, .. } = *ast.node(h) else {
            return Guess::Unknown;
        };
        let Some(name) = ident_name(ast, expr) else {
            return Guess::Unknown;
        };
        if !self.ck.prefer_definitions {
            let k = known_name(self.ck, name);
            if k != Guess::Unknown {
                return k;
            }
        }
        let Some((sym, decl)) = type_definition(self.cx, name, scope) else {
            return known_name(self.ck, name);
        };
        if self.visited.borrow().contains(&sym) {
            return Guess::Unknown;
        }
        self.visited.borrow_mut().push(sym);
        let def_scope = self.cx.semantic().symbol(sym).scope;
        let import = self.import_binding_type(&decl);
        let g = if import != Guess::Unknown {
            import
        } else {
            match *ast.node(decl.node) {
                Node::TypeAlias { ty, .. } => self.annotation(ty, def_scope),
                Node::TypeParam {
                    constraint: Some(c),
                    ..
                } => self.annotation(c, def_scope),
                Node::Interface { extends, .. } => {
                    let ext = ast.list(extends);
                    if ext.is_empty() {
                        Guess::NonTarget
                    } else {
                        combine_intersection(
                            ext.iter()
                                .map(|&h| self.heritage_ref(h, def_scope))
                                .collect(),
                        )
                    }
                }
                Node::ClassDecl { .. } if decl.kind == DeclKind::Class => {
                    self.class_heritage(decl.node, def_scope, true)
                }
                _ => Guess::Unknown,
            }
        };
        self.visited.borrow_mut().retain(|x| *x != sym);
        g
    }

    fn import_binding_type(&self, decl: &crate::semantic::Decl) -> Guess {
        let ast = self.ast();
        match decl.kind {
            DeclKind::ImportDefault => {
                let Some(local) = ident_name(ast, decl.name) else {
                    return Guess::Unknown;
                };
                if known_name(self.ck, local) == Guess::Unknown {
                    Guess::Unknown
                } else {
                    Guess::NonTarget
                }
            }
            DeclKind::ImportNamed => {
                let Node::ImportSpecifier {
                    imported, local, ..
                } = *ast.node(decl.node)
                else {
                    return Guess::Unknown;
                };
                let local_name = ident_name(ast, local).unwrap_or("");
                let imported_name = match imported {
                    Some(i) => match *ast.node(i) {
                        Node::Ident { name } => ast.atom(name).to_owned(),
                        Node::StringLit { value } => ast.atom(value).to_owned(),
                        _ => return Guess::Unknown,
                    },
                    None => local_name.to_owned(),
                };
                let source = import_source(ast, decl.parent);
                known_imported(
                    self.ck,
                    self.ck.target_imports,
                    source.as_deref(),
                    &imported_name,
                    local_name,
                )
            }
            _ => Guess::Unknown,
        }
    }

    fn namespace_import_type(&self, type_name: NodeId, scope: ScopeId) -> Guess {
        let ast = self.ast();
        if self.ck.target_namespace_imports.is_empty() {
            return Guess::Unknown;
        }
        let Node::QualifiedName { left, right } = *ast.node(type_name) else {
            return Guess::Unknown;
        };
        let (Some(left), Some(right)) = (ident_name(ast, left), ident_name(ast, right)) else {
            return Guess::Unknown;
        };
        let Some((_, decl)) = type_definition(self.cx, left, scope) else {
            return Guess::Unknown;
        };
        if !matches!(
            decl.kind,
            DeclKind::ImportNamespace | DeclKind::ImportDefault
        ) {
            return Guess::Unknown;
        }
        let source = import_source(ast, decl.parent);
        known_imported(
            self.ck,
            self.ck.target_namespace_imports,
            source.as_deref(),
            right,
            right,
        )
    }

    /// `class X extends Y`: the superclass's answer (non-target without one).
    fn class_heritage(&self, class: NodeId, scope: ScopeId, identifier_only: bool) -> Guess {
        let ast = self.ast();
        let Some(sup) = super_class(ast, class) else {
            return Guess::NonTarget;
        };
        if !self.ck.class_heritage {
            return Guess::NonTarget;
        }
        if identifier_only && !matches!(ast.node(sup), Node::Ident { .. }) {
            return Guess::Unknown;
        }
        self.class_ref(sup, scope)
    }

    /// `getClassReferenceTypeFromScope`.
    fn class_ref(&self, node: NodeId, scope: ScopeId) -> Guess {
        let d = self.depth.get();
        if d > ENGINE_DEPTH {
            return Guess::Unknown;
        }
        self.depth.set(d + 1);
        let r = self.class_ref_inner(node, scope);
        self.depth.set(d);
        r
    }

    fn class_ref_inner(&self, node: NodeId, scope: ScopeId) -> Guess {
        let ast = self.ast();
        let sem = self.cx.semantic();
        let node = skip_parens(ast, node);
        match *ast.node(node) {
            Node::Ident { name } => {
                let name = ast.atom(name);
                if !self.ck.class_heritage {
                    return known_name(self.ck, name);
                }
                let Some(sym) = sem.lookup(scope, name) else {
                    return known_name(self.ck, name);
                };
                let s = sem.symbol(sym);
                let Some(decl) = s.decls.first().copied() else {
                    return known_name(self.ck, name);
                };
                if s.flags.contains(SymbolFlags::COMMENT_GLOBAL) {
                    return known_name(self.ck, name);
                }
                if self.visited.borrow().contains(&sym) {
                    return Guess::Unknown;
                }
                self.visited.borrow_mut().push(sym);
                let g = match *ast.node(decl.node) {
                    Node::VarDeclarator {
                        name: n,
                        init: Some(init),
                        ..
                    } if decl.kind == DeclKind::Const && n == decl.name => {
                        self.class_ref(init, s.scope)
                    }
                    Node::ClassDecl { .. } if decl.kind == DeclKind::Class => {
                        self.class_heritage(decl.node, s.scope, false)
                    }
                    _ => Guess::Unknown,
                };
                self.visited.borrow_mut().retain(|x| *x != sym);
                g
            }
            Node::ClassExpr { .. } | Node::ClassDecl { .. } => {
                self.class_heritage(node, scope, false)
            }
            _ => Guess::Unknown,
        }
    }

    fn this_type(&self, id: NodeId) -> Guess {
        let cx = self.cx;
        let ast = self.ast();
        let sem = cx.semantic();
        let mut cur = id;
        while let Some(p) = cx.parent(cur) {
            match *ast.node(p) {
                Node::ClassDecl { .. } | Node::ClassExpr { .. } => {
                    return self.class_heritage(p, sem.enclosing_scope(p), false);
                }
                Node::StaticBlock { .. } => return Guess::NonTarget,
                Node::PropertyDef { modifiers, .. } if modifiers.contains(Modifiers::STATIC) => {
                    return Guess::NonTarget;
                }
                Node::FunctionDecl { params, .. } | Node::FunctionExpr { params, .. } => {
                    let tp = self.this_param_type(params, p);
                    if tp != Guess::Unknown {
                        return tp;
                    }
                    let gp = cx.parent(p);
                    match gp.map(|g| ast.node(g)) {
                        Some(Node::MethodDef { modifiers, .. }) => {
                            if modifiers.contains(Modifiers::STATIC) {
                                return Guess::NonTarget;
                            }
                            // An object-literal method: the object.
                            if gp
                                .and_then(|g| cx.parent(g))
                                .is_some_and(|o| matches!(ast.node(o), Node::ObjectLit { .. }))
                            {
                                return Guess::NonTarget;
                            }
                            cur = p;
                            continue;
                        }
                        Some(Node::Property { .. }) => {
                            if gp
                                .and_then(|g| cx.parent(g))
                                .is_some_and(|o| matches!(ast.node(o), Node::ObjectLit { .. }))
                            {
                                return Guess::NonTarget;
                            }
                            return Guess::Unknown;
                        }
                        _ => return Guess::Unknown,
                    }
                }
                _ => {}
            }
            cur = p;
        }
        Guess::Unknown
    }

    fn this_param_type(&self, params: ts_r_ast::NodeList, f: NodeId) -> Guess {
        let ast = self.ast();
        let Some(&p) = ast.list(params).first() else {
            return Guess::Unknown;
        };
        match *ast.node(p) {
            Node::Param {
                pattern,
                type_ann: Some(t),
                ..
            } if ident_name(ast, pattern) == Some("this") => {
                self.annotation(t, self.cx.semantic().enclosing_scope(f))
            }
            _ => Guess::Unknown,
        }
    }

    fn super_type(&self, id: NodeId) -> Guess {
        let cx = self.cx;
        let ast = self.ast();
        let mut cur = id;
        while let Some(p) = cx.parent(cur) {
            match *ast.node(p) {
                Node::StaticBlock { .. } => return Guess::NonTarget,
                Node::PropertyDef { modifiers, .. } | Node::MethodDef { modifiers, .. }
                    if modifiers.contains(Modifiers::STATIC) =>
                {
                    return Guess::NonTarget;
                }
                Node::ClassDecl { .. } | Node::ClassExpr { .. } => {
                    return match super_class(ast, p) {
                        Some(s) => self.class_ref(s, cx.semantic().enclosing_scope(p)),
                        None => Guess::NonTarget,
                    };
                }
                _ => {}
            }
            cur = p;
        }
        Guess::Unknown
    }
}

fn fn_flags(ast: &Ast, f: NodeId) -> (bool, bool) {
    match *ast.node(f) {
        Node::FunctionDecl {
            flags, return_type, ..
        }
        | Node::FunctionExpr {
            flags, return_type, ..
        }
        | Node::Arrow {
            flags, return_type, ..
        } => (
            flags.intersects(FnFlags::ASYNC | FnFlags::GENERATOR),
            return_type.is_some(),
        ),
        _ => (false, false),
    }
}

/// The scope a function's own declarations don't belong to (its `upper`).
fn outer_scope(cx: &Ctx<'_, '_>, f: NodeId) -> ScopeId {
    let sem = cx.semantic();
    match sem.scope_of_node(f) {
        Some(s) => sem.scope(s).parent.unwrap_or(s),
        None => sem.enclosing_scope(f),
    }
}

/// A variable's initializer, or a simple parameter's default value.
pub(crate) fn definition_initializer(
    cx: &Ctx<'_, '_>,
    decl: &crate::semantic::Decl,
) -> Option<NodeId> {
    let ast = cx.ast();
    match *ast.node(decl.node) {
        Node::VarDeclarator { init, .. } => init,
        Node::Param { pattern, init, .. }
            if decl.kind == DeclKind::Param && pattern == decl.name =>
        {
            init
        }
        _ => None,
    }
}

fn super_class(ast: &Ast, class: NodeId) -> Option<NodeId> {
    let (Node::ClassDecl { heritage, .. } | Node::ClassExpr { heritage, .. }) = *ast.node(class)
    else {
        return None;
    };
    for &h in ast.list(heritage) {
        if let Node::HeritageClause {
            kind: HeritageKind::Extends,
            types,
        } = *ast.node(h)
            && let Some(&t) = ast.list(types).first()
        {
            return Some(match *ast.node(t) {
                Node::ExprWithTypeArgs { expr, .. } => expr,
                _ => t,
            });
        }
    }
    None
}

fn import_source(ast: &Ast, import_decl: NodeId) -> Option<String> {
    match *ast.node(import_decl) {
        Node::ImportDecl { source, .. } => match *ast.node(source) {
            Node::StringLit { value } => Some(ast.atom(value).to_owned()),
            _ => None,
        },
        _ => None,
    }
}

fn known_imported(
    ck: &Checker,
    table: &[(&str, &[&str])],
    source: Option<&str>,
    imported: &str,
    local: &str,
) -> Guess {
    if let Some(src) = source
        && let Some((_, names)) = table.iter().find(|(s, _)| *s == src)
    {
        return if names.contains(&imported) {
            Guess::Target
        } else {
            Guess::NonTarget
        };
    }
    if table.iter().any(|(_, names)| names.contains(&imported))
        || known_name(ck, imported) != Guess::Unknown
        || known_name(ck, local) != Guess::Unknown
    {
        return Guess::NonTarget;
    }
    Guess::Unknown
}

/// `Foo` or `Foo.Bar` of a type reference name.
fn type_name_text(ast: &Ast, id: NodeId) -> Option<String> {
    match *ast.node(id) {
        Node::Ident { name } => Some(ast.atom(name).to_owned()),
        Node::QualifiedName { left, right } => {
            let l = type_name_text(ast, left)?;
            Some(format!("{l}.{}", ident_name(ast, right)?))
        }
        _ => None,
    }
}

/// The nearest type-like declaration (class, import, enum, alias, interface, type
/// parameter) of `name`, from `scope` outwards.
pub(crate) fn type_definition(
    cx: &Ctx<'_, '_>,
    name: &str,
    scope: ScopeId,
) -> Option<(SymbolId, crate::semantic::Decl)> {
    let sem = cx.semantic();
    let mut cur = Some(scope);
    let mut guard = 0;
    while let Some(sc) = cur {
        guard += 1;
        if guard > 10_000 {
            return None;
        }
        let s = sem.scope(sc);
        for &sym in &s.symbols {
            if sem.name(sym) != name {
                continue;
            }
            if let Some(d) = sem.symbol(sym).decls.iter().find(|d| {
                matches!(
                    d.kind,
                    DeclKind::Class
                        | DeclKind::ImportDefault
                        | DeclKind::ImportNamespace
                        | DeclKind::ImportNamed
                        | DeclKind::ImportEquals
                        | DeclKind::Enum
                        | DeclKind::TypeAlias
                        | DeclKind::Interface
                        | DeclKind::TypeParam
                )
            }) {
                return Some((sym, *d));
            }
        }
        cur = s.parent;
    }
    None
}

fn is_literal_type(ast: &Ast, id: NodeId) -> bool {
    match *ast.node(id) {
        Node::StringLit { .. }
        | Node::NumberLit { .. }
        | Node::BigIntLit { .. }
        | Node::BoolLit { .. } => true,
        Node::Unary {
            op: UnaryOp::Minus,
            arg,
        } => matches!(
            ast.node(arg),
            Node::NumberLit { .. } | Node::BigIntLit { .. }
        ),
        Node::TemplateLitType { parts } => ast.list(parts).len() <= 1,
        _ => false,
    }
}

fn is_non_target_annotation(ast: &Ast, id: NodeId) -> bool {
    match *ast.node(id) {
        Node::KeywordType { kind } => matches!(
            kind,
            KeywordTypeKind::BigInt
                | KeywordTypeKind::Boolean
                | KeywordTypeKind::Never
                | KeywordTypeKind::Number
                | KeywordTypeKind::String
                | KeywordTypeKind::Symbol
                | KeywordTypeKind::Void
        ),
        Node::ArrayType { .. }
        | Node::TupleType { .. }
        | Node::TypeLiteral { .. }
        | Node::FunctionType { .. }
        | Node::ConstructorType { .. } => true,
        _ => is_literal_type(ast, id),
    }
}

/// `new Name(...)` (the callee spelled `Name`).
pub(crate) fn is_new_of(ast: &Ast, id: NodeId, name: &str) -> bool {
    matches!(*ast.node(id), Node::New { callee, .. } if ident_name(ast, callee) == Some(name))
}

/// `Name(...)` (not optional).
pub(crate) fn is_call_of(ast: &Ast, id: NodeId, name: &str) -> bool {
    matches!(*ast.node(id), Node::Call { callee, optional: false, .. } if ident_name(ast, callee) == Some(name))
}

/// `(object, method)` of `object.method(...)`: a non-optional call of a non-computed,
/// non-optional member.
pub(crate) fn method_call(ast: &Ast, id: NodeId) -> Option<(NodeId, &str)> {
    let Node::Call {
        callee,
        optional: false,
        ..
    } = *ast.node(id)
    else {
        return None;
    };
    let Node::Member {
        obj,
        optional: false,
        prop,
    } = *ast.node(callee)
    else {
        return None;
    };
    Some((obj, ident_name(ast, prop)?))
}

/// `Object.name` (non-computed, non-optional, `Object` spelled as an identifier).
pub(crate) fn is_static_member(ast: &Ast, id: NodeId, object: &str, names: &[&str]) -> bool {
    matches!(*ast.node(id), Node::Member { obj, optional: false, prop }
        if ident_name(ast, obj) == Some(object)
            && ident_name(ast, prop).is_some_and(|p| names.contains(&p)))
}

/// `Object.name(...)` (non-optional call).
pub(crate) fn is_static_call(ast: &Ast, id: NodeId, object: &str, names: &[&str]) -> bool {
    matches!(*ast.node(id), Node::Call { callee, optional: false, .. }
        if is_static_member(ast, callee, object, names))
}

// ---- The plugin's checkers ---------------------------------------------------------------

pub(crate) const TYPED_ARRAYS: &[&str] = &[
    "Int8Array",
    "Uint8Array",
    "Uint8ClampedArray",
    "Int16Array",
    "Uint16Array",
    "Int32Array",
    "Uint32Array",
    "Float16Array",
    "Float32Array",
    "Float64Array",
    "BigInt64Array",
    "BigUint64Array",
];

const NON_INDEXED_COLLECTIONS: &[&str] = &[
    "Map",
    "ReadonlyMap",
    "WeakMap",
    "Set",
    "ReadonlySet",
    "WeakSet",
    "URLSearchParams",
    "Headers",
    "FormData",
    "CanvasRenderingContext2D",
    "OffscreenCanvasRenderingContext2D",
];

const NON_ARRAYS: &[&str] = &[
    "Int8Array",
    "Uint8Array",
    "Uint8ClampedArray",
    "Int16Array",
    "Uint16Array",
    "Int32Array",
    "Uint32Array",
    "Float16Array",
    "Float32Array",
    "Float64Array",
    "BigInt64Array",
    "BigUint64Array",
    "Map",
    "ReadonlyMap",
    "WeakMap",
    "Set",
    "ReadonlySet",
    "WeakSet",
    "URLSearchParams",
    "Headers",
    "FormData",
    "CanvasRenderingContext2D",
    "OffscreenCanvasRenderingContext2D",
];

const INDEXED_COLLECTIONS: &[&str] = &[
    "Array",
    "ReadonlyArray",
    "Int8Array",
    "Uint8Array",
    "Uint8ClampedArray",
    "Int16Array",
    "Uint16Array",
    "Int32Array",
    "Uint32Array",
    "Float16Array",
    "Float32Array",
    "Float64Array",
    "BigInt64Array",
    "BigUint64Array",
];

const INDEXED_CTORS: &[&str] = &[
    "Array",
    "Int8Array",
    "Uint8Array",
    "Uint8ClampedArray",
    "Int16Array",
    "Uint16Array",
    "Int32Array",
    "Uint32Array",
    "Float16Array",
    "Float32Array",
    "Float64Array",
    "BigInt64Array",
    "BigUint64Array",
];

/// A simple rest parameter (`...args`) that is never reassigned.
fn is_rest_parameter(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    if !matches!(ast.node(id), Node::Ident { .. }) {
        return false;
    }
    let sem = cx.semantic();
    let Some(s) = sem.reference_at(id).and_then(|r| sem.reference(r).symbol()) else {
        return false;
    };
    let sym = sem.symbol(s);
    let [decl] = sym.decls.as_slice() else {
        return false;
    };
    decl.kind == DeclKind::Param
        && matches!(*ast.node(decl.node), Node::Param { rest: true, pattern, .. } if pattern == decl.name)
        && sym.refs.iter().all(|&r| !sem.reference(r).is_write())
}

fn is_typed_array_factory(ast: &Ast, id: NodeId) -> bool {
    TYPED_ARRAYS
        .iter()
        .any(|t| is_static_call(ast, id, t, &["from", "of"]))
}

fn array_node(cx: &Ctx<'_, '_>, id: NodeId, recv_is_array: &dyn Fn(NodeId) -> bool) -> bool {
    let ast = cx.ast();
    if matches!(ast.node(id), Node::ArrayLit { .. }) || is_rest_parameter(cx, id) {
        return true;
    }
    if is_static_call(ast, id, "Array", &["from", "of"])
        || is_static_call(
            ast,
            id,
            "Object",
            &[
                "keys",
                "values",
                "entries",
                "getOwnPropertyNames",
                "getOwnPropertySymbols",
            ],
        )
    {
        return true;
    }
    matches!(method_call(ast, id), Some((obj, m)) if matches!(m,
        "copyWithin" | "fill" | "reverse" | "sort" | "toReversed" | "toSorted" | "toSpliced" | "with")
        && recv_is_array(obj))
}

fn non_array_node(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    matches!(
        ast.node(id),
        Node::New { .. }
            | Node::ObjectLit { .. }
            | Node::FunctionExpr { .. }
            | Node::Arrow { .. }
            | Node::ClassExpr { .. }
            | Node::TemplateLit { .. }
    ) || is_typed_array_factory(ast, id)
}

fn array_static_type(v: &Static, ast: &Ast, id: NodeId) -> Guess {
    if matches!(v, Static::Array(_)) {
        return Guess::Target;
    }
    if matches!(
        ast.node(id),
        Node::Ident { .. } | Node::Member { .. } | Node::Index { .. }
    ) {
        Guess::Unknown
    } else {
        Guess::NonTarget
    }
}

fn array_annotation(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(id),
        Node::ArrayType { .. } | Node::TupleType { .. }
    )
}

pub(crate) static ARRAY: Checker = Checker {
    class_heritage: false,
    prefer_definitions: true,
    target_names: &["Array", "ReadonlyArray"],
    non_target_names: NON_ARRAYS,
    target_calls: &["Array"],
    target_ctors: &["Array"],
    target_node: Some(array_node),
    non_target_node: Some(non_array_node),
    target_annotation: Some(array_annotation),
    static_type: Some(array_static_type),
    ..Checker::DEFAULT
};

fn indexed_node(cx: &Ctx<'_, '_>, id: NodeId, _recurse: &dyn Fn(NodeId) -> bool) -> bool {
    array_node(cx, id, &|r| ARRAY.is_target(cx, r)) || is_typed_array_factory(cx.ast(), id)
}

pub(crate) static INDEXED_COLLECTION: Checker = Checker {
    target_names: INDEXED_COLLECTIONS,
    non_target_names: NON_INDEXED_COLLECTIONS,
    target_ctors: INDEXED_CTORS,
    target_node: Some(indexed_node),
    ..ARRAY_BASE
};

const ARRAY_BASE: Checker = Checker {
    class_heritage: false,
    prefer_definitions: true,
    target_names: &["Array", "ReadonlyArray"],
    non_target_names: NON_ARRAYS,
    target_calls: &["Array"],
    target_ctors: &["Array"],
    target_node: Some(array_node),
    non_target_node: Some(non_array_node),
    target_annotation: Some(array_annotation),
    static_type: Some(array_static_type),
    ..Checker::DEFAULT
};

fn bigint_typed_array_node(cx: &Ctx<'_, '_>, id: NodeId, _r: &dyn Fn(NodeId) -> bool) -> bool {
    let ast = cx.ast();
    is_typed_array_factory(ast, id)
        && matches!(*ast.node(id), Node::Call { callee, .. }
            if matches!(*ast.node(callee), Node::Member { obj, .. }
                if matches!(ident_name(ast, obj), Some("BigInt64Array" | "BigUint64Array"))))
}

pub(crate) static BIGINT_TYPED_ARRAY: Checker = Checker {
    mixed_union_target: true,
    target_names: &["BigInt64Array", "BigUint64Array"],
    target_ctors: &["BigInt64Array", "BigUint64Array"],
    target_node: Some(bigint_typed_array_node),
    ..Checker::DEFAULT
};

/// `isArray`.
pub(crate) fn is_array(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    ARRAY.is_target(cx, id)
}

/// `isKnownNonArray`.
pub(crate) fn is_known_non_array(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    ARRAY.is_known_non_target(cx, id)
}

/// `isIndexedCollection`: an array or a typed array.
pub(crate) fn is_indexed_collection(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    INDEXED_COLLECTION.is_target(cx, id)
}

/// `isKnownNonIndexedCollection`.
pub(crate) fn is_known_non_indexed_collection(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    INDEXED_COLLECTION.is_known_non_target(cx, id)
}

/// `isKnownBigIntTypedArray`.
pub(crate) fn is_known_bigint_typed_array(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    BIGINT_TYPED_ARRAY.is_target(cx, id)
}

/// `shouldSkipKnownNonArrayReceiver`: an array-method rule leaves this receiver alone (it is
/// known not to be an array or typed array, and isn't written as a literal at the call).
pub(crate) fn should_skip_known_non_array_receiver(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let id = skip_parens(ast, id);
    let direct = matches!(
        ast.node(id),
        Node::ArrayLit { .. }
            | Node::FunctionExpr { .. }
            | Node::ObjectLit { .. }
            | Node::TemplateLit { .. }
            | Node::StringLit { .. }
            | Node::NumberLit { .. }
            | Node::BigIntLit { .. }
            | Node::BoolLit { .. }
            | Node::NullLit {}
            | Node::RegexLit { .. }
    );
    !direct && is_known_non_indexed_collection(cx, id)
}

macro_rules! builtin_checker {
    ($name:ident, $target:literal, [$($alias:literal),*], heritage = $h:expr) => {
        pub(crate) static $name: Checker = Checker {
            target_names: &[$target],
            aliases: &[$(($alias, $target)),*],
            target_ctors: &[$target],
            class_heritage: $h,
            ..Checker::DEFAULT
        };
    };
}

builtin_checker!(MAP, "Map", ["ReadonlyMap"], heritage = true);
builtin_checker!(SET, "Set", ["ReadonlySet"], heritage = true);
builtin_checker!(WEAK_MAP, "WeakMap", [], heritage = true);
builtin_checker!(WEAK_SET, "WeakSet", [], heritage = true);
builtin_checker!(DATE, "Date", [], heritage = false);

pub(crate) fn is_map(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    MAP.is_target(cx, id)
}
pub(crate) fn is_known_non_map(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    MAP.is_known_non_target(cx, id)
}
pub(crate) fn is_set(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    SET.is_target(cx, id)
}
pub(crate) fn is_known_non_set(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    SET.is_known_non_target(cx, id)
}
pub(crate) fn is_weak_map(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    WEAK_MAP.is_target(cx, id)
}
pub(crate) fn is_weak_set(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    WEAK_SET.is_target(cx, id)
}
pub(crate) fn is_date(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    DATE.is_target(cx, id)
}

fn regexp_node(cx: &Ctx<'_, '_>, id: NodeId, _r: &dyn Fn(NodeId) -> bool) -> bool {
    matches!(cx.node(id), Node::RegexLit { .. })
}

fn regexp_static_type(v: &Static, _ast: &Ast, _id: NodeId) -> Guess {
    if matches!(v, Static::RegExp { .. }) {
        Guess::Target
    } else {
        Guess::Unknown
    }
}

pub(crate) static REGEXP: Checker = Checker {
    target_names: &["RegExp"],
    target_ctors: &["RegExp"],
    target_calls: &["RegExp"],
    target_node: Some(regexp_node),
    static_type: Some(regexp_static_type),
    ..Checker::DEFAULT
};

/// `RegExp` without following class heritage (used by `isString` for `.source`/`.flags`).
pub(crate) static REGEXP_NO_HERITAGE: Checker = Checker {
    class_heritage: false,
    ..REGEXP_BASE
};

const REGEXP_BASE: Checker = Checker {
    target_names: &["RegExp"],
    target_ctors: &["RegExp"],
    target_calls: &["RegExp"],
    target_node: Some(regexp_node),
    static_type: Some(regexp_static_type),
    ..Checker::DEFAULT
};

pub(crate) fn is_regexp(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    REGEXP.is_target(cx, id)
}
pub(crate) fn is_known_non_regexp(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    REGEXP.is_known_non_target(cx, id)
}

/// `Map`/`Set` and their read-only types: what `.size` is a number on.
pub(crate) static SIZED_COLLECTION: Checker = Checker {
    target_names: &["Map", "Set", "ReadonlyMap", "ReadonlySet"],
    target_ctors: &["Map", "Set"],
    class_heritage: false,
    ..Checker::DEFAULT
};

// -- strings

const STRING_RETURNING: &[&str] = &[
    "charAt",
    "concat",
    "normalize",
    "padEnd",
    "padStart",
    "repeat",
    "slice",
    "substring",
    "toLocaleLowerCase",
    "toLocaleUpperCase",
    "toLowerCase",
    "toString",
    "toUpperCase",
    "toWellFormed",
    "trim",
    "trimEnd",
    "trimStart",
    "valueOf",
];

const DATE_STRING_RETURNING: &[&str] = &[
    "toDateString",
    "toISOString",
    "toLocaleDateString",
    "toLocaleString",
    "toLocaleTimeString",
    "toString",
    "toTimeString",
    "toUTCString",
];

fn string_annotation(ast: &Ast, id: NodeId) -> bool {
    match *ast.node(id) {
        Node::KeywordType {
            kind: KeywordTypeKind::String,
        } => true,
        Node::StringLit { .. } => true,
        Node::TemplateLitType { parts } => ast.list(parts).len() <= 1,
        _ => false,
    }
}

fn string_static_type(v: &Static, _ast: &Ast, _id: NodeId) -> Guess {
    if matches!(v, Static::Str(_)) {
        Guess::Target
    } else {
        Guess::Unknown
    }
}

fn string_node(cx: &Ctx<'_, '_>, id: NodeId, recurse: &dyn Fn(NodeId) -> bool) -> bool {
    let ast = cx.ast();
    match *ast.node(id) {
        Node::StringLit { .. } | Node::TemplateLit { .. } => return true,
        Node::Unary {
            op: UnaryOp::TypeOf,
            ..
        } => return true,
        _ => {}
    }
    if is_call_of(ast, id, "String")
        || is_static_call(ast, id, "String", &["fromCharCode", "fromCodePoint", "raw"])
    {
        return true;
    }
    match *ast.node(id) {
        Node::Member {
            obj,
            optional: false,
            prop,
        } => {
            matches!(ident_name(ast, prop), Some("source" | "flags"))
                && REGEXP_NO_HERITAGE.is_target(cx, obj)
        }
        Node::TaggedTemplate { tag, .. } => is_static_member(ast, tag, "String", &["raw"]),
        Node::Call { .. } => {
            let Some((obj, m)) = method_call(ast, id) else {
                return false;
            };
            if m == "join" {
                return is_indexed_collection(cx, obj);
            }
            if DATE_STRING_RETURNING.contains(&m) && is_date(cx, obj) {
                return true;
            }
            STRING_RETURNING.contains(&m) && is_string_with(cx, obj, recurse)
        }
        Node::Assign {
            left,
            op: AssignOp::Assign,
            right,
        } => {
            let _ = left;
            is_string_with(cx, right, recurse)
        }
        Node::Binary {
            left,
            op: BinaryOp::Add,
            right,
        }
        | Node::Assign {
            left,
            op: AssignOp::AddAssign,
            right,
        } => is_string_with(cx, left, recurse) || is_string_with(cx, right, recurse),
        _ => false,
    }
}

pub(crate) static STRING: Checker = Checker {
    target_names: &["string"],
    target_calls: &[
        "String",
        "Date",
        "encodeURI",
        "encodeURIComponent",
        "decodeURI",
        "decodeURIComponent",
        "atob",
        "btoa",
    ],
    target_node: Some(string_node),
    target_annotation: Some(string_annotation),
    static_type: Some(string_static_type),
    ..Checker::DEFAULT
};

/// A `let`/`var` with an initializer that is never written again.
fn is_unmodified_mutable_variable(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    if !matches!(ast.node(id), Node::Ident { .. }) {
        return false;
    }
    let sem = cx.semantic();
    let Some(s) = sem.symbol_of_ident(id) else {
        return false;
    };
    let sym = sem.symbol(s);
    let [decl] = sym.decls.as_slice() else {
        return false;
    };
    matches!(decl.kind, DeclKind::Let | DeclKind::Var)
        && matches!(*ast.node(decl.node), Node::VarDeclarator { name, init: Some(_), .. } if name == decl.name)
        && sym.refs.iter().all(|&r| {
            let r = sem.reference(r);
            !r.is_write() || r.flags.contains(RefFlags::INIT)
        })
}

fn is_string_with(cx: &Ctx<'_, '_>, id: NodeId, target: &dyn Fn(NodeId) -> bool) -> bool {
    let ast = cx.ast();
    let id = skip_parens(ast, id);
    if let Node::Satisfies { ty, .. } = *ast.node(id)
        && string_annotation(ast, ty)
    {
        return true;
    }
    if target(id) {
        return true;
    }
    let v = if is_unmodified_mutable_variable(cx, id) {
        static_value_no_side_effects(cx, id)
    } else {
        static_value_control_flow(cx, id)
    };
    matches!(v, Some(Static::Str(_))) && !has_jsdoc_type_reference(cx, id)
}

/// `isString`.
pub(crate) fn is_string(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    is_string_with(cx, id, &|n| STRING.is_target(cx, n))
}

/// `isKnownNonString`.
pub(crate) fn is_known_non_string(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    STRING.is_known_non_target(cx, id)
}

// -- numbers

const MATH_PROPERTIES: &[&str] = &[
    "E", "LN2", "LN10", "LOG2E", "LOG10E", "PI", "SQRT1_2", "SQRT2",
];
const MATH_METHODS: &[&str] = &[
    "abs", "acos", "acosh", "asin", "asinh", "atan", "atanh", "atan2", "cbrt", "ceil", "clz32",
    "cos", "cosh", "exp", "expm1", "f16round", "floor", "fround", "hypot", "imul", "log", "log1p",
    "log10", "log2", "max", "min", "pow", "random", "round", "sign", "sin", "sinh", "sqrt", "tan",
    "tanh", "trunc",
];
const STRING_NUMBER_METHODS: &[&str] = &["charCodeAt", "indexOf", "lastIndexOf", "localeCompare"];
const INDEXED_NUMBER_METHODS: &[&str] = &["indexOf", "lastIndexOf", "findIndex", "findLastIndex"];
const ARRAY_NUMBER_METHODS: &[&str] = &["push", "unshift"];
const DATE_NUMBER_METHODS: &[&str] = &[
    "getDate",
    "getDay",
    "getFullYear",
    "getHours",
    "getMilliseconds",
    "getMinutes",
    "getMonth",
    "getSeconds",
    "getTime",
    "getTimezoneOffset",
    "getUTCDate",
    "getUTCDay",
    "getUTCFullYear",
    "getUTCHours",
    "getUTCMilliseconds",
    "getUTCMinutes",
    "getUTCMonth",
    "getUTCSeconds",
    "setDate",
    "setFullYear",
    "setHours",
    "setMilliseconds",
    "setMinutes",
    "setMonth",
    "setSeconds",
    "setTime",
    "setUTCDate",
    "setUTCFullYear",
    "setUTCHours",
    "setUTCMilliseconds",
    "setUTCMinutes",
    "setUTCMonth",
    "setUTCSeconds",
    "valueOf",
];
const NUMBER_PROPERTIES: &[&str] = &[
    "EPSILON",
    "MAX_SAFE_INTEGER",
    "MAX_VALUE",
    "MIN_SAFE_INTEGER",
    "MIN_VALUE",
    "NaN",
    "NEGATIVE_INFINITY",
    "POSITIVE_INFINITY",
];

fn number_annotation(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(id),
        Node::KeywordType {
            kind: KeywordTypeKind::Number,
        } | Node::NumberLit { .. }
    )
}

pub(crate) static NUMBER: Checker = Checker {
    target_names: &["number"],
    target_calls: &["Number"],
    target_annotation: Some(number_annotation),
    ..Checker::DEFAULT
};

/// A binding of `id` declared `: number` (or a numeric literal type), not optional.
fn has_number_annotation(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let sem = cx.semantic();
    let Some(s) = sem.symbol_of_ident(id) else {
        return false;
    };
    sem.symbol(s).decls.iter().any(|d| {
        let (ann, optional) = binding_annotation(cx, d.name);
        !optional && ann.is_some_and(|t| number_annotation(ast, t))
    })
}

fn is_number_returning_method_call(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let Some((obj, m)) = method_call(ast, id) else {
        return false;
    };
    (STRING_NUMBER_METHODS.contains(&m) && is_string(cx, obj))
        || (ARRAY_NUMBER_METHODS.contains(&m) && is_array(cx, obj))
        || (INDEXED_NUMBER_METHODS.contains(&m) && is_indexed_collection(cx, obj))
        || (DATE_NUMBER_METHODS.contains(&m) && is_date(cx, obj))
}

fn is_numeric_property(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let Node::Member {
        obj,
        optional: false,
        prop,
    } = *ast.node(id)
    else {
        return false;
    };
    match ident_name(ast, prop) {
        Some("length") => {
            let o = skip_parens(ast, obj);
            is_function_node(ast, o) || is_string(cx, obj) || is_indexed_collection(cx, obj)
        }
        Some("size") => SIZED_COLLECTION.is_target(cx, obj),
        _ => false,
    }
}

/// `isNumber`: the expression is known to evaluate to a number (not a BigInt).
pub(crate) fn is_number(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    is_number_depth(cx, id, 0)
}

fn is_number_depth(cx: &Ctx<'_, '_>, id: NodeId, depth: u32) -> bool {
    if depth > 48 {
        return false;
    }
    let ast = cx.ast();
    let id = skip_parens(ast, id);
    let rec = |n: NodeId| is_number_depth(cx, n, depth + 1);
    if matches!(ast.node(id), Node::NumberLit { .. })
        || is_static_member(ast, id, "Math", MATH_PROPERTIES)
        || is_static_call(ast, id, "Math", MATH_METHODS)
        || is_call_of(ast, id, "Number")
        || is_number_returning_method_call(cx, id)
        || is_static_member(ast, id, "Number", NUMBER_PROPERTIES)
        || is_static_call(ast, id, "Number", &["parseFloat", "parseInt"])
        || is_static_call(ast, id, "Date", &["now", "parse", "UTC"])
        || is_call_of(ast, id, "parseInt")
        || is_call_of(ast, id, "parseFloat")
        || is_numeric_property(cx, id)
    {
        return true;
    }
    let math_op = |op: BinaryOp| {
        matches!(
            op,
            BinaryOp::Sub
                | BinaryOp::Mul
                | BinaryOp::Div
                | BinaryOp::Mod
                | BinaryOp::Exp
                | BinaryOp::Shl
                | BinaryOp::Shr
                | BinaryOp::BitOr
                | BinaryOp::BitXor
                | BinaryOp::BitAnd
        )
    };
    let binary_number = |op: Option<BinaryOp>, left: NodeId, right: NodeId| -> bool {
        match op {
            Some(BinaryOp::Add) => rec(left) && rec(right),
            Some(BinaryOp::UShr) => true,
            Some(op) if math_op(op) => rec(left) || rec(right),
            _ => false,
        }
    };
    let found = match *ast.node(id) {
        Node::Assign { left, op, right } => {
            if op == AssignOp::Assign {
                rec(right)
            } else {
                binary_number(compound_binary(op), left, right)
            }
        }
        Node::Binary { left, op, right } => binary_number(Some(op), left, right),
        Node::Unary { op, arg } => match op {
            UnaryOp::Plus => true,
            UnaryOp::Minus | UnaryOp::BitNot => rec(arg),
            _ => false,
        },
        Node::Update { arg, .. } => rec(arg),
        Node::Conditional { test, cons, alt } => {
            let c = rec(cons);
            let a = rec(alt);
            if c && a {
                true
            } else {
                match static_value_control_flow(cx, test) {
                    Some(v) if !has_jsdoc_type_reference(cx, test) => {
                        (c && v.truthy()) || (a && !v.truthy())
                    }
                    _ => false,
                }
            }
        }
        Node::Seq { exprs } => ast.list(exprs).last().is_some_and(|&e| rec(e)),
        Node::Ident { .. } => has_number_annotation(cx, id),
        Node::As { ty, .. } | Node::Satisfies { ty, .. } | Node::TypeAssertion { ty, .. } => {
            number_annotation(ast, ty)
        }
        Node::NonNull { expr } => rec(expr),
        _ => false,
    };
    if found {
        return true;
    }
    NUMBER.is_target(cx, id)
        || (!has_jsdoc_type_reference(cx, id)
            && matches!(static_value_control_flow(cx, id), Some(Static::Num(_))))
}

fn compound_binary(op: AssignOp) -> Option<BinaryOp> {
    Some(match op {
        AssignOp::AddAssign => BinaryOp::Add,
        AssignOp::SubAssign => BinaryOp::Sub,
        AssignOp::MulAssign => BinaryOp::Mul,
        AssignOp::DivAssign => BinaryOp::Div,
        AssignOp::ModAssign => BinaryOp::Mod,
        AssignOp::ExpAssign => BinaryOp::Exp,
        AssignOp::ShlAssign => BinaryOp::Shl,
        AssignOp::ShrAssign => BinaryOp::Shr,
        AssignOp::UShrAssign => BinaryOp::UShr,
        AssignOp::BitOrAssign => BinaryOp::BitOr,
        AssignOp::BitXorAssign => BinaryOp::BitXor,
        AssignOp::BitAndAssign => BinaryOp::BitAnd,
        _ => return None,
    })
}

/// `isKnownNonNumber`.
pub(crate) fn is_known_non_number(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    NUMBER.is_known_non_target(cx, id)
}

// -- bigints

fn bigint_annotation(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(id),
        Node::KeywordType {
            kind: KeywordTypeKind::BigInt,
        } | Node::BigIntLit { .. }
    )
}

fn bigint_static_type(v: &Static, _ast: &Ast, _id: NodeId) -> Guess {
    if matches!(v, Static::BigInt(_)) {
        Guess::Target
    } else {
        Guess::Unknown
    }
}

fn bigint_node(cx: &Ctx<'_, '_>, id: NodeId, recurse: &dyn Fn(NodeId) -> bool) -> bool {
    let ast = cx.ast();
    if matches!(ast.node(id), Node::BigIntLit { .. })
        || is_call_of(ast, id, "BigInt")
        || is_static_call(ast, id, "BigInt", &["asIntN", "asUintN"])
    {
        return true;
    }
    match *ast.node(id) {
        Node::Unary {
            op: UnaryOp::Minus | UnaryOp::BitNot,
            arg,
        } => recurse(skip_parens(ast, arg)),
        Node::Binary { left, op, right } => {
            matches!(
                op,
                BinaryOp::Add
                    | BinaryOp::Sub
                    | BinaryOp::Mul
                    | BinaryOp::Div
                    | BinaryOp::Mod
                    | BinaryOp::Exp
                    | BinaryOp::Shl
                    | BinaryOp::Shr
                    | BinaryOp::BitOr
                    | BinaryOp::BitXor
                    | BinaryOp::BitAnd
            ) && recurse(skip_parens(ast, left))
                && recurse(skip_parens(ast, right))
        }
        Node::Update { arg, .. } => recurse(skip_parens(ast, arg)),
        _ => false,
    }
}

pub(crate) static BIGINT: Checker = Checker {
    target_names: &["bigint"],
    target_calls: &["BigInt"],
    target_node: Some(bigint_node),
    target_annotation: Some(bigint_annotation),
    static_type: Some(bigint_static_type),
    ..Checker::DEFAULT
};

/// `BigInt` with `treatMixedUnionAsTarget` (a union with any bigint member counts).
pub(crate) static BIGINT_MIXED_UNION: Checker = Checker {
    mixed_union_target: true,
    ..BIGINT_BASE
};

const BIGINT_BASE: Checker = Checker {
    target_names: &["bigint"],
    target_calls: &["BigInt"],
    target_node: Some(bigint_node),
    target_annotation: Some(bigint_annotation),
    static_type: Some(bigint_static_type),
    ..Checker::DEFAULT
};

/// `isBigInt`.
pub(crate) fn is_bigint(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    is_bigint_with(cx, id, &BIGINT)
}

/// `isBigInt` with a checker variant (`BIGINT_MIXED_UNION`).
pub(crate) fn is_bigint_with(cx: &Ctx<'_, '_>, id: NodeId, checker: &Checker) -> bool {
    let ast = cx.ast();
    let id = skip_parens(ast, id);
    if let Node::Satisfies { ty, .. } = *ast.node(id)
        && bigint_annotation(ast, ty)
    {
        return true;
    }
    if checker.is_target(cx, id) {
        return true;
    }
    matches!(
        static_value_no_side_effects(cx, id),
        Some(Static::BigInt(_))
    ) && !has_jsdoc_type_reference(cx, id)
}

/// `isKnownNonBigInt`.
pub(crate) fn is_known_non_bigint(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    BIGINT.is_known_non_target(cx, id)
}

// ---- Booleans (`isBoolean`) --------------------------------------------------------------

/// Global `Object.method`s that return booleans.
const BOOLEAN_STATIC_METHODS: &[(&str, &[&str])] = &[
    ("Array", &["isArray"]),
    ("ArrayBuffer", &["isView"]),
    ("Atomics", &["isLockFree"]),
    ("Error", &["isError"]),
    (
        "Number",
        &["isFinite", "isInteger", "isNaN", "isSafeInteger"],
    ),
    (
        "Object",
        &["hasOwn", "is", "isExtensible", "isFrozen", "isSealed"],
    ),
    (
        "Reflect",
        &[
            "defineProperty",
            "deleteProperty",
            "has",
            "isExtensible",
            "preventExtensions",
            "set",
            "setPrototypeOf",
        ],
    ),
    ("URL", &["canParse"]),
];

static KEYED_COLLECTION: Checker = Checker {
    target_names: &[
        "Map",
        "ReadonlyMap",
        "WeakMap",
        "URLSearchParams",
        "Headers",
        "FormData",
    ],
    class_heritage: false,
    ..Checker::DEFAULT
};

static NAMED_SET: Checker = Checker {
    target_names: &["Set", "ReadonlySet"],
    class_heritage: false,
    ..Checker::DEFAULT
};

static NAMED_WEAK_SET: Checker = Checker {
    target_names: &["WeakSet"],
    class_heritage: false,
    ..Checker::DEFAULT
};

static NAMED_REGEXP: Checker = Checker {
    target_names: &["RegExp"],
    class_heritage: false,
    ..Checker::DEFAULT
};

static MUTABLE_KEYED_COLLECTION: Checker = Checker {
    target_names: &["Map", "Set", "WeakMap", "WeakSet"],
    non_target_names: &["ReadonlyMap", "ReadonlySet"],
    target_ctors: &["Map", "Set", "WeakMap", "WeakSet"],
    class_heritage: false,
    ..Checker::DEFAULT
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum ValueKind {
    Array,
    KeyedCollection,
    RegExp,
    Set,
    String,
    WeakSet,
}

fn checked_value_kind(cx: &Ctx<'_, '_>, id: NodeId) -> Option<ValueKind> {
    if is_indexed_collection(cx, id) {
        Some(ValueKind::Array)
    } else if KEYED_COLLECTION.is_target(cx, id) {
        Some(ValueKind::KeyedCollection)
    } else if NAMED_REGEXP.is_target(cx, id) {
        Some(ValueKind::RegExp)
    } else if NAMED_SET.is_target(cx, id) {
        Some(ValueKind::Set)
    } else if is_string(cx, id) {
        Some(ValueKind::String)
    } else if NAMED_WEAK_SET.is_target(cx, id) {
        Some(ValueKind::WeakSet)
    } else {
        None
    }
}

/// What kind of value a method's receiver is, by syntax first, then by guessing.
fn value_kind(cx: &Ctx<'_, '_>, id: NodeId, depth: u32) -> Option<ValueKind> {
    let ast = cx.ast();
    if depth > 50 {
        return None;
    }
    let id = skip_parens(ast, id);
    match *ast.node(id) {
        Node::ArrayLit { .. } => Some(ValueKind::Array),
        Node::StringLit { .. } | Node::TemplateLit { .. } => Some(ValueKind::String),
        Node::RegexLit { .. } => Some(ValueKind::RegExp),
        Node::NumberLit { .. }
        | Node::BoolLit { .. }
        | Node::NullLit {}
        | Node::BigIntLit { .. } => None,
        Node::New { callee, .. } => {
            let global = |n: &str| cx.is_global_ref(callee, n);
            if global("RegExp") {
                Some(ValueKind::RegExp)
            } else if ["Map", "WeakMap", "URLSearchParams", "Headers", "FormData"]
                .iter()
                .any(|n| global(n))
            {
                Some(ValueKind::KeyedCollection)
            } else if global("Set") {
                Some(ValueKind::Set)
            } else if global("WeakSet") {
                Some(ValueKind::WeakSet)
            } else {
                checked_value_kind(cx, id)
            }
        }
        Node::Ident { .. } => {
            if let Some(k) = checked_value_kind(cx, id) {
                return Some(k);
            }
            if jsdoc_type_annotation(cx, id).is_some() {
                return None;
            }
            let (_, init) = super::unicorn_static::const_definition(cx, id)?;
            value_kind(cx, init, depth + 1)
        }
        Node::NonNull { expr } | Node::Satisfies { expr, .. } => value_kind(cx, expr, depth + 1),
        _ => checked_value_kind(cx, id),
    }
}

/// The name of a non-optional `obj.name` / `obj["name"]` member.
fn member_name(ast: &Ast, id: NodeId) -> Option<(NodeId, String)> {
    match *ast.node(id) {
        Node::Member {
            obj,
            prop,
            optional: false,
        } => ident_name(ast, prop).map(|n| (obj, n.to_owned())),
        Node::Index {
            obj,
            index,
            optional: false,
        } => super::unicorn_text::static_string_value(ast, index).map(|n| (obj, n)),
        _ => None,
    }
}

fn boolean_call(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let Node::Call { callee, .. } = *ast.node(id) else {
        return false;
    };
    let callee = skip_parens(ast, callee);
    if let Some((obj, method)) = member_name(ast, callee) {
        let obj = skip_parens(ast, obj);
        if matches!(ast.node(obj), Node::Ident { .. })
            && BOOLEAN_STATIC_METHODS
                .iter()
                .any(|(o, ms)| cx.is_global_ref(obj, o) && ms.contains(&method.as_str()))
        {
            return true;
        }
        if method == "delete" {
            return MUTABLE_KEYED_COLLECTION.is_target(cx, obj);
        }
        let methods: &[&str] = match value_kind(cx, obj, 0) {
            Some(ValueKind::Array) => &["every", "includes", "some"],
            Some(ValueKind::KeyedCollection) | Some(ValueKind::WeakSet) => &["has"],
            Some(ValueKind::RegExp) => &["test"],
            Some(ValueKind::Set) => &["has", "isDisjointFrom", "isSubsetOf", "isSupersetOf"],
            Some(ValueKind::String) => &["endsWith", "includes", "isWellFormed", "startsWith"],
            None => &[],
        };
        return methods.contains(&method.as_str());
    }
    ["Boolean", "isFinite", "isNaN"]
        .iter()
        .any(|n| cx.is_global_ref(callee, n))
}

fn boolean_node(cx: &Ctx<'_, '_>, id: NodeId, recurse: &dyn Fn(NodeId) -> bool) -> bool {
    let ast = cx.ast();
    match *ast.node(id) {
        Node::Unary {
            op: UnaryOp::Not | UnaryOp::Delete,
            ..
        } => true,
        Node::Binary { left, op, right } => match op {
            BinaryOp::Lt
            | BinaryOp::LtEq
            | BinaryOp::Gt
            | BinaryOp::GtEq
            | BinaryOp::Eq
            | BinaryOp::StrictEq
            | BinaryOp::NotEq
            | BinaryOp::StrictNotEq
            | BinaryOp::In
            | BinaryOp::InstanceOf => true,
            BinaryOp::LogicalAnd | BinaryOp::LogicalOr | BinaryOp::Coalesce => {
                recurse(left) && recurse(right)
            }
            _ => false,
        },
        Node::Conditional { cons, alt, .. } => recurse(cons) && recurse(alt),
        Node::Seq { exprs } => ast.list(exprs).last().is_some_and(|&e| recurse(e)),
        Node::Await { arg } => recurse(arg),
        Node::NonNull { expr } => recurse(expr),
        Node::Call { .. } => boolean_call(cx, id),
        Node::Member { .. } | Node::Index { .. } => {
            member_name(ast, id).is_some_and(|(obj, name)| {
                value_kind(cx, obj, 0) == Some(ValueKind::RegExp)
                    && [
                        "dotAll",
                        "global",
                        "hasIndices",
                        "ignoreCase",
                        "multiline",
                        "sticky",
                        "unicode",
                        "unicodeSets",
                    ]
                    .contains(&name.as_str())
            })
        }
        _ => false,
    }
}

fn boolean_annotation(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(id),
        Node::KeywordType {
            kind: KeywordTypeKind::Boolean,
        } | Node::BoolLit { .. }
            | Node::TypePredicate { asserts: false, .. }
    )
}

fn boolean_static_type(v: &Static, _ast: &Ast, _id: NodeId) -> Guess {
    if matches!(v, Static::Bool(_)) {
        Guess::Target
    } else {
        Guess::Unknown
    }
}

pub(crate) static BOOLEAN: Checker = Checker {
    target_names: &["boolean"],
    non_target_names: &["Array", "ReadonlyArray", "Promise", "PromiseLike"],
    target_node: Some(boolean_node),
    target_annotation: Some(boolean_annotation),
    static_type: Some(boolean_static_type),
    ..Checker::DEFAULT
};

/// `isBoolean`.
pub(crate) fn is_boolean(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    BOOLEAN.is_target(cx, id)
}

// ---- Built-in collections (`getBuiltinCollectionType`) ----------------------------------

/// The built-in collection names an expression may hold (`Map`, `ReadonlyMap`, `Set`,
/// `ReadonlySet`), or `None` when unknown.
fn collection_types(
    cx: &Ctx<'_, '_>,
    id: NodeId,
    visited: &mut Vec<SymbolId>,
) -> Option<Vec<&'static str>> {
    let ast = cx.ast();
    let id = skip_parens(ast, id);
    if visited.len() > 100 {
        return None;
    }
    match *ast.node(id) {
        Node::New { callee, .. } => {
            let name = match ident_name(ast, callee) {
                Some("Map") => "Map",
                Some("Set") => "Set",
                _ => return None,
            };
            (matches!(ast.node(callee), Node::Ident { .. }) && cx.is_global_ref(callee, name))
                .then(|| vec![name])
        }
        Node::Ident { .. } => {
            let sem = cx.semantic();
            let sym = sem.symbol_of_ident(id)?;
            let symbol = sem.symbol(sym);
            let [decl] = symbol.decls.as_slice() else {
                return None;
            };
            if visited.contains(&sym) {
                return None;
            }
            visited.push(sym);
            let types = collection_annotation_types(cx, binding_annotation(cx, decl.name).0, 0)
                .or_else(|| match *ast.node(decl.node) {
                    Node::VarDeclarator {
                        name,
                        init: Some(init),
                        ..
                    } if decl.kind == DeclKind::Const && name == decl.name => {
                        collection_types(cx, init, visited)
                    }
                    _ => None,
                });
            visited.retain(|s| *s != sym);
            types
        }
        Node::As { expr, ty } | Node::Satisfies { expr, ty } | Node::TypeAssertion { expr, ty } => {
            collection_annotation_types(cx, Some(ty), 0)
                .or_else(|| collection_types(cx, expr, visited))
        }
        Node::NonNull { expr } => collection_types(cx, expr, visited),
        Node::Conditional { cons, alt, .. } => {
            let mut a = collection_types(cx, cons, visited)?;
            a.extend(collection_types(cx, alt, visited)?);
            Some(a)
        }
        _ => None,
    }
}

fn collection_annotation_types(
    cx: &Ctx<'_, '_>,
    ty: Option<NodeId>,
    depth: u32,
) -> Option<Vec<&'static str>> {
    let ast = cx.ast();
    let ty = ty?;
    if depth > 100 {
        return None;
    }
    match *ast.node(ty) {
        Node::ParenType { ty } => collection_annotation_types(cx, Some(ty), depth + 1),
        Node::TypeOperator {
            op: ts_r_ast::TypeOperatorKind::Readonly,
            ty,
        } => collection_annotation_types(cx, Some(ty), depth + 1),
        Node::TypeRef { name, .. } if matches!(ast.node(name), Node::Ident { .. }) => {
            let n = ident_name(ast, name)?;
            if type_definition(cx, n, cx.semantic().enclosing_scope(name)).is_some() {
                return None;
            }
            ["Map", "ReadonlyMap", "Set", "ReadonlySet"]
                .into_iter()
                .find(|c| *c == n)
                .map(|c| vec![c])
        }
        Node::UnionType { types } => {
            let mut out = Vec::new();
            for &t in ast.list(types) {
                out.extend(collection_annotation_types(cx, Some(t), depth + 1)?);
            }
            Some(out)
        }
        _ => None,
    }
}

/// `getBuiltinCollectionType`: `"Map"` or `"Set"` when an expression is known to be only
/// maps or only sets (annotations, `new Map`/`new Set`, `const` initializers).
pub(crate) fn builtin_collection_type(cx: &Ctx<'_, '_>, id: NodeId) -> Option<&'static str> {
    let types = collection_types(cx, id, &mut Vec::new())?;
    let map = types.iter().any(|t| t.ends_with("Map"));
    let set = types.iter().any(|t| t.ends_with("Set"));
    match (map, set) {
        (true, false) => Some("Map"),
        (false, true) => Some("Set"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(n: &str) -> JType {
        JType::Name(n.into())
    }

    #[test]
    fn jsdoc_types() {
        assert_eq!(parse_jsdoc_type("string"), name("string"));
        assert_eq!(parse_jsdoc_type("Number"), name("number"));
        assert_eq!(parse_jsdoc_type("unknown"), JType::Unknown);
        assert_eq!(
            parse_jsdoc_type("?Date"),
            JType::Nullable(Box::new(name("Date")))
        );
        assert_eq!(
            parse_jsdoc_type("Map<string, number> | Set<boolean>"),
            JType::Union(vec![name("Map"), name("Set")])
        );
        assert_eq!(parse_jsdoc_type("{size: string}"), JType::NonPrimitive);
        assert_eq!(
            parse_jsdoc_type("() => number"),
            JType::Function(Box::new(name("number")))
        );
        assert_eq!(parse_jsdoc_type("string[]"), name("Array"));
        assert_eq!(parse_jsdoc_type("[string, boolean]"), name("Array"));
        assert_eq!(
            parse_jsdoc_type("number="),
            JType::Optional(Box::new(name("number")))
        );
        assert_eq!(
            parse_jsdoc_type("{getTime: () => string}"),
            JType::NonPrimitive
        );
        assert_eq!(parse_jsdoc_type("Array<"), JType::Unknown);
        // An escaped character wider than one byte.
        assert!(valid_string_escapes("'\\é'"));
        assert!(!valid_string_escapes("'\\xé'"));
        let _ = parse_jsdoc_type("'\\é'");
        assert_eq!(
            parse_jsdoc_type("'a' | 1n"),
            JType::Union(vec![name("string"), name("bigint")])
        );
    }

    #[test]
    fn jsdoc_tag_reading() {
        let tags = doc_tags("* @param {unknown} value ");
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].name.as_deref(), Some("value"));
        assert_eq!(tags[0].ty, Some(JType::Unknown));
        let tags = doc_tags("*\n * @param {number} [value]\n * @returns {Map<K, V>}\n ");
        assert_eq!(tags.len(), 2);
        assert!(tags[0].optional);
        assert_eq!(tags[1].ty, Some(name("Map")));
        assert!(doc_tags("* @typedef {Object} Foo\n * @type {string} ").is_empty());
    }

    use crate::config::{ConfigFile, ConfigKind, ResolveOptions, objects_from_eslint_json};
    use crate::diagnostic::Severity;
    use crate::engine::{ConfiguredRule, RuleSet, lint_source};
    use crate::registry::Registry;
    use crate::types::{FileKey, NoTypeInfo};
    use std::sync::Arc;

    static PROBE: RuleDef = RuleDef {
        meta: meta("test/probe", "", NO_PRESET, Category::Correctness),
        create: create_default::<Probe>,
    };

    /// Reports each `probe(x)` argument as `<checker>:<answer>` for every checker.
    #[derive(Debug, Default)]
    struct Probe;

    impl Rule for Probe {
        fn kinds(&self) -> &'static [NodeKind] {
            &[NodeKind::Call]
        }
        fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
            let ast = cx.ast();
            let Node::Call { callee, args, .. } = *cx.node(node) else {
                return;
            };
            if ident_name(ast, callee) != Some("probe") {
                return;
            }
            let Some(&arg) = ast.list(args).first() else {
                return;
            };
            let answer = |yes: bool, no: bool| match (yes, no) {
                (true, _) => "T",
                (_, true) => "N",
                _ => "?",
            };
            let msg = format!(
                "array:{} string:{} number:{} map:{} set:{} regexp:{} bigint:{} indexed:{}",
                answer(is_array(cx, arg), is_known_non_array(cx, arg)),
                answer(is_string(cx, arg), is_known_non_string(cx, arg)),
                answer(is_number(cx, arg), is_known_non_number(cx, arg)),
                answer(is_map(cx, arg), is_known_non_map(cx, arg)),
                answer(is_set(cx, arg), is_known_non_set(cx, arg)),
                answer(is_regexp(cx, arg), is_known_non_regexp(cx, arg)),
                answer(is_bigint(cx, arg), is_known_non_bigint(cx, arg)),
                answer(
                    is_indexed_collection(cx, arg),
                    is_known_non_indexed_collection(cx, arg)
                ),
            );
            cx.report_node(arg, msg);
        }
    }

    /// The probe's answers, one string per `probe(...)` call, for `checker`.
    fn probe(code: &str, checker: &str) -> Vec<String> {
        let path = if code.contains(": ") || code.contains(" as ") {
            "a.ts"
        } else {
            "a.js"
        };
        let cfg = serde_json::json!([{
            "files": ["**/*.ts", "**/*.js"],
            "languageOptions": { "parser": "typescript-eslint/parser" },
        }]);
        let objs = objects_from_eslint_json(&cfg, Registry::global()).unwrap();
        let cfg = ConfigFile::new(ConfigKind::Eslint, None, "/p".into(), objs);
        let mut fc = (*cfg.file_config(path, &ResolveOptions::new()).unwrap()).clone();
        fc.rules = Arc::new(RuleSet::new(vec![ConfiguredRule {
            def: &PROBE,
            severity: Severity::Error,
            rule: Arc::new(Probe),
        }]));
        lint_source(&format!("/p/{path}"), code, &fc, &NoTypeInfo, FileKey(0))
            .diagnostics
            .iter()
            .filter(|d| d.rule == Some("test/probe"))
            .map(|d| {
                let prefix = format!("{checker}:");
                d.message
                    .split(' ')
                    .find_map(|part| part.strip_prefix(&prefix))
                    .unwrap_or("")
                    .to_owned()
            })
            .collect()
    }

    #[test]
    fn arrays_from_shape_annotation_and_const() {
        let code = "const a = [1]; let b: string[] = f(); const c = new Map();
            function g(...rest) { probe(rest); }
            probe(a); probe(b); probe(c); probe(Array.from(x)); probe(x); probe({});
            probe(a.toSorted());";
        assert_eq!(
            probe(code, "array"),
            ["T", "T", "T", "N", "T", "?", "N", "T"]
        );
    }

    #[test]
    fn strings_numbers_and_jsdoc() {
        let code = "/**
             * @param {boolean} d
             * @param {string} s
             */
            function f(d, s) { probe(d); probe(s); }
            const n = 1; probe(n); probe(n + 1); probe('a' + x); probe(x.length);
            probe('abc'.length); probe(Math.PI); probe(typeof x);";
        assert_eq!(
            probe(code, "string"),
            ["N", "T", "?", "?", "T", "?", "?", "?", "T"]
        );
        assert_eq!(
            probe(code, "number"),
            ["N", "N", "T", "T", "?", "?", "T", "T", "?"]
        );
    }

    #[test]
    fn maps_sets_and_class_heritage() {
        let code = "class M extends Map {} const m = new M(); let s: ReadonlySet<number>;
            interface I extends Map<string, number> {} let i: I;
            probe(m); probe(s); probe(i); probe(new Set()); probe(/x/);";
        // `new M()` needs `class_syntax`, which the `Map` checker leaves off.
        assert_eq!(probe(code, "map"), ["?", "?", "T", "?", "?"]);
        assert_eq!(probe(code, "set"), ["?", "T", "?", "T", "?"]);
        assert_eq!(probe(code, "regexp"), ["?", "?", "?", "?", "T"]);
    }

    #[test]
    fn unions_and_nullish() {
        let code = "let a: string | number; let b: string | undefined; let c: 'x' | `y`;
            probe(a); probe(b); probe(c); probe(x ? 'a' : 'b'); probe(cond ? 1n : 2n);";
        // Literals of other types are not "known non-strings": only annotations say that.
        assert_eq!(probe(code, "string"), ["?", "?", "T", "T", "?"]);
        assert_eq!(probe(code, "bigint"), ["N", "N", "N", "?", "T"]);
    }

    #[test]
    fn unions_and_intersections() {
        let ck = &MAP;
        assert_eq!(
            combine_union(ck, vec![Guess::Target, Guess::Nullish]),
            Guess::Unknown
        );
        assert_eq!(
            combine_union(ck, vec![Guess::NonTarget, Guess::Nullish]),
            Guess::NonTarget
        );
        assert_eq!(
            combine_intersection(vec![Guess::Target, Guess::NonTarget]),
            Guess::Target
        );
        assert_eq!(
            combine_intersection(vec![Guess::Target, Guess::Unknown]),
            Guess::Unknown
        );
        assert_eq!(known_name(ck, "ReadonlyMap"), Guess::Target);
    }
}
