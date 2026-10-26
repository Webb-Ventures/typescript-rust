//! CSS-like AST selectors with the syntax and matching rules of `esquery`, the selector engine
//! ESLint uses for rule listeners (`no-restricted-syntax`, jsdoc `contexts`).
//!
//! The matcher is generic over [`Tree`], so the same selectors run on the ESTree view of a file
//! ([`crate::estree`]) and on the ESTree form of a JSDoc block ([`crate::jsdoc_tree`]).
//!
//! An ancestry is a node plus how many of its ancestors the match may look at (`avail`): the
//! whole parent chain for ESLint listeners, none for `esquery.matches(node, sel)` without an
//! ancestry, and the path back to the subject inside `:has(...)`.

use std::borrow::Cow;

use crate::regex::{Flags, Regex};

/// The value of an attribute path on a node.
#[derive(Debug, Clone, PartialEq)]
pub enum Val<'a> {
    /// `undefined`: no such property.
    Undef,
    Null,
    Bool(bool),
    Num(f64),
    Str(Cow<'a, str>),
    /// A node (an object).
    Node,
    /// An array of the given length (an object).
    List(u32),
}

impl Val<'_> {
    fn is_nullish(&self) -> bool {
        matches!(self, Val::Undef | Val::Null)
    }

    /// `${value}` in JavaScript.
    fn to_js_string(&self) -> Cow<'_, str> {
        match self {
            Val::Undef => "undefined".into(),
            Val::Null => "null".into(),
            Val::Bool(b) => if *b { "true" } else { "false" }.into(),
            Val::Num(n) => js_number(*n).into(),
            Val::Str(s) => Cow::Borrowed(s.as_ref()),
            Val::Node => "[object Object]".into(),
            // Arrays of nodes stringify as comma-joined "[object Object]"s.
            Val::List(n) => vec!["[object Object]"; *n as usize].join(",").into(),
        }
    }

    fn type_of(&self) -> &'static str {
        match self {
            Val::Undef => "undefined",
            Val::Bool(_) => "boolean",
            Val::Num(_) => "number",
            Val::Str(_) => "string",
            Val::Null | Val::Node | Val::List(_) => "object",
        }
    }

    /// `+value` in JavaScript, for the relational operators.
    fn to_number(&self) -> f64 {
        match self {
            Val::Undef | Val::Node => f64::NAN,
            Val::Null => 0.0,
            Val::Bool(b) => f64::from(u8::from(*b)),
            Val::Num(n) => *n,
            Val::Str(s) => {
                let t = s.trim();
                if t.is_empty() {
                    0.0
                } else {
                    t.parse().unwrap_or(f64::NAN)
                }
            }
            Val::List(n) => match n {
                0 => 0.0,
                _ => f64::NAN,
            },
        }
    }
}

/// Formats a number the way `String(n)` does for the values selectors use.
pub fn js_number(n: f64) -> String {
    if n.is_nan() {
        "NaN".into()
    } else if n.is_infinite() {
        if n > 0.0 { "Infinity" } else { "-Infinity" }.into()
    } else if n == n.trunc() && n.abs() < 1e21 {
        format!("{}", n as i128)
    } else {
        format!("{n}")
    }
}

/// A tree selectors can run on. Node ids are dense `u32`s.
pub trait Tree {
    fn ty(&self, n: u32) -> &str;
    fn parent(&self, n: u32) -> Option<u32>;
    /// The field of the parent holding `n`.
    fn key(&self, n: u32) -> &str;
    /// `(index, length)` when that field is an array.
    fn list_pos(&self, n: u32) -> Option<(u32, u32)>;
    /// The element `index` of the array holding `n` (`None` for holes and out of range).
    fn list_item(&self, n: u32, index: u32) -> Option<u32>;
    /// Direct children in traversal order.
    fn children(&self, n: u32) -> &[u32];
    /// The value at an attribute path (`params.length`, `callee.name`).
    fn attr(&self, n: u32, path: &[&str]) -> Val<'_>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Debug)]
enum Value {
    Str(String),
    Num(f64),
    Regex { pattern: String, flags: String },
    Type(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Comb {
    Child,
    Descendant,
    Sibling,
    Adjacent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    Statement,
    Declaration,
    Expression,
    Pattern,
    Function,
}

#[derive(Debug)]
enum Sel {
    Wildcard,
    /// Lower-cased type name, and as written (for ESLint's node-type buckets).
    Ident(String, String),
    Attr {
        path: Vec<String>,
        op: Option<(Op, Value)>,
    },
    Field(Vec<String>),
    Not(Vec<Sel>),
    Matches(Vec<Sel>),
    Has(Vec<Sel>),
    /// Positive from the start, negative from the end (`:nth-last-child`).
    Nth(i64),
    Class(Class),
    Compound(Vec<Sel>, bool),
    Binary(Comb, Box<Sel>, Box<Sel>),
    /// The `:has` subject in `:has(> X)`.
    ExactNode,
}

/// A parsed selector.
#[derive(Debug)]
pub struct Selector {
    root: Sel,
    regexes: Vec<RegexCache>,
    pub raw: String,
    /// ESLint's specificity: attribute-like and identifier counts.
    pub attribute_count: u32,
    pub identifier_count: u32,
    /// The node types the selector can match (ESLint's `nodeTypes`), `None` for any.
    pub node_types: Option<Vec<String>>,
}

#[derive(Debug)]
struct RegexCache {
    pattern: String,
    flags: String,
}

const MAX_DEPTH: u32 = 64;

struct Parser<'s> {
    s: &'s [u8],
    src: &'s str,
    pos: usize,
    depth: u32,
}

fn is_name_byte(b: u8) -> bool {
    !matches!(
        b,
        b' ' | b'['
            | b']'
            | b','
            | b'('
            | b')'
            | b':'
            | b'#'
            | b'!'
            | b'='
            | b'>'
            | b'<'
            | b'~'
            | b'+'
            | b'.'
    )
}

type PResult<T> = Result<T, String>;

impl<'s> Parser<'s> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.pos).copied()
    }

    fn eat(&mut self, lit: &str) -> bool {
        if self.src[self.pos..].starts_with(lit) {
            self.pos += lit.len();
            true
        } else {
            false
        }
    }

    fn eat_ci(&mut self, lit: &str) -> bool {
        let end = self.pos + lit.len();
        if end <= self.s.len()
            && self.src.is_char_boundary(end)
            && self.src[self.pos..end].eq_ignore_ascii_case(lit)
        {
            self.pos = end;
            true
        } else {
            false
        }
    }

    fn spaces(&mut self) {
        while self.peek() == Some(b' ') {
            self.pos += 1;
        }
    }

    fn err<T>(&self) -> PResult<T> {
        Err(format!(
            "Expected selector syntax at position {} in `{}`",
            self.pos, self.src
        ))
    }

    fn name(&mut self) -> Option<String> {
        let start = self.pos;
        while let Some(b) = self.peek() {
            if !is_name_byte(b) {
                break;
            }
            self.pos += 1;
        }
        (self.pos > start).then(|| self.src[start..self.pos].to_owned())
    }

    fn selectors(&mut self) -> PResult<Vec<Sel>> {
        let mut out = vec![self.selector()?];
        loop {
            let save = self.pos;
            self.spaces();
            if self.eat(",") {
                self.spaces();
                out.push(self.selector()?);
            } else {
                self.pos = save;
                return Ok(out);
            }
        }
    }

    fn has_selectors(&mut self) -> PResult<Vec<Sel>> {
        let mut out = vec![self.has_selector()?];
        loop {
            let save = self.pos;
            self.spaces();
            if self.eat(",") {
                self.spaces();
                out.push(self.has_selector()?);
            } else {
                self.pos = save;
                return Ok(out);
            }
        }
    }

    fn has_selector(&mut self) -> PResult<Sel> {
        let save = self.pos;
        if let Some(op) = self.combinator() {
            let right = self.selector()?;
            return Ok(Sel::Binary(op, Box::new(Sel::ExactNode), Box::new(right)));
        }
        self.pos = save;
        self.selector()
    }

    /// A combinator, consuming the spaces around it; `None` (position unchanged) if there
    /// is none.
    fn combinator(&mut self) -> Option<Comb> {
        let save = self.pos;
        self.spaces();
        let had_space = self.pos > save;
        let op = match self.peek() {
            Some(b'>') => Some(Comb::Child),
            Some(b'~') => Some(Comb::Sibling),
            Some(b'+') => Some(Comb::Adjacent),
            _ => None,
        };
        if let Some(op) = op {
            self.pos += 1;
            self.spaces();
            return Some(op);
        }
        if had_space {
            return Some(Comb::Descendant);
        }
        self.pos = save;
        None
    }

    fn selector(&mut self) -> PResult<Sel> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err("selector is nested too deeply".into());
        }
        let mut left = self.sequence()?;
        loop {
            let save = self.pos;
            let Some(op) = self.combinator() else { break };
            match self.sequence_opt()? {
                Some(right) => left = Sel::Binary(op, Box::new(left), Box::new(right)),
                None => {
                    self.pos = save;
                    break;
                }
            }
        }
        self.depth -= 1;
        Ok(left)
    }

    fn sequence(&mut self) -> PResult<Sel> {
        match self.sequence_opt()? {
            Some(s) => Ok(s),
            None => self.err(),
        }
    }

    fn sequence_opt(&mut self) -> PResult<Option<Sel>> {
        let save = self.pos;
        let subject = self.eat("!");
        let mut atoms = Vec::new();
        while let Some(a) = self.atom()? {
            atoms.push(a);
        }
        if atoms.is_empty() {
            self.pos = save;
            return Ok(None);
        }
        Ok(Some(if atoms.len() == 1 && !subject {
            atoms.pop().unwrap_or(Sel::Wildcard)
        } else {
            Sel::Compound(atoms, subject)
        }))
    }

    fn atom(&mut self) -> PResult<Option<Sel>> {
        let Some(b) = self.peek() else {
            return Ok(None);
        };
        match b {
            b'*' => {
                self.pos += 1;
                Ok(Some(Sel::Wildcard))
            }
            b'[' => {
                self.pos += 1;
                self.spaces();
                let a = self.attr()?;
                self.spaces();
                if !self.eat("]") {
                    return self.err();
                }
                Ok(Some(a))
            }
            b'.' => {
                self.pos += 1;
                let path = self.path()?;
                Ok(Some(Sel::Field(path)))
            }
            b':' => self.pseudo().map(Some),
            b'#' => {
                self.pos += 1;
                match self.name() {
                    Some(n) => Ok(Some(Sel::Ident(n.to_lowercase(), n))),
                    None => self.err(),
                }
            }
            _ if is_name_byte(b) => {
                let n = self.name().unwrap_or_default();
                Ok(Some(Sel::Ident(n.to_lowercase(), n)))
            }
            _ => Ok(None),
        }
    }

    fn path(&mut self) -> PResult<Vec<String>> {
        let mut out = Vec::new();
        match self.name() {
            Some(n) => out.push(n),
            None => return self.err(),
        }
        while self.peek() == Some(b'.') {
            let save = self.pos;
            self.pos += 1;
            match self.name() {
                Some(n) => out.push(n),
                None => {
                    self.pos = save;
                    break;
                }
            }
        }
        Ok(out)
    }

    fn pseudo(&mut self) -> PResult<Sel> {
        let list = |p: &mut Self, has: bool| -> PResult<Vec<Sel>> {
            p.spaces();
            let ss = if has {
                p.has_selectors()?
            } else {
                p.selectors()?
            };
            p.spaces();
            if !p.eat(")") {
                return p.err();
            }
            Ok(ss)
        };
        if self.eat(":not(") {
            return Ok(Sel::Not(list(self, false)?));
        }
        if self.eat(":matches(") || self.eat(":is(") {
            return Ok(Sel::Matches(list(self, false)?));
        }
        if self.eat(":has(") {
            return Ok(Sel::Has(list(self, true)?));
        }
        if self.eat(":first-child") {
            return Ok(Sel::Nth(1));
        }
        if self.eat(":last-child") {
            return Ok(Sel::Nth(-1));
        }
        for (lit, sign) in [(":nth-child(", 1i64), (":nth-last-child(", -1)] {
            if self.eat(lit) {
                self.spaces();
                let start = self.pos;
                while self.peek().is_some_and(|b| b.is_ascii_digit()) {
                    self.pos += 1;
                }
                let n: i64 = self.src[start..self.pos].parse().or_else(|_| self.err())?;
                self.spaces();
                if !self.eat(")") {
                    return self.err();
                }
                return Ok(Sel::Nth(n.saturating_mul(sign)));
            }
        }
        if self.eat(":") {
            for (name, c) in [
                ("statement", Class::Statement),
                ("expression", Class::Expression),
                ("declaration", Class::Declaration),
                ("function", Class::Function),
                ("pattern", Class::Pattern),
            ] {
                if self.eat_ci(name) {
                    return Ok(Sel::Class(c));
                }
            }
        }
        self.err()
    }

    fn attr(&mut self) -> PResult<Sel> {
        let path = self.path()?;
        let save = self.pos;
        self.spaces();
        let op = if self.eat("!=") {
            Some(Op::Ne)
        } else if self.eat("<=") {
            Some(Op::Le)
        } else if self.eat(">=") {
            Some(Op::Ge)
        } else if self.eat("=") {
            Some(Op::Eq)
        } else if self.eat("<") {
            Some(Op::Lt)
        } else if self.eat(">") {
            Some(Op::Gt)
        } else {
            None
        };
        let Some(op) = op else {
            self.pos = save;
            return Ok(Sel::Attr { path, op: None });
        };
        self.spaces();
        let value = if matches!(op, Op::Eq | Op::Ne) && self.eat("type(") {
            self.spaces();
            let start = self.pos;
            while self.peek().is_some_and(|b| b != b' ' && b != b')') {
                self.pos += 1;
            }
            if self.pos == start {
                return self.err();
            }
            let t = self.src[start..self.pos].to_owned();
            self.spaces();
            if !self.eat(")") {
                return self.err();
            }
            Value::Type(t)
        } else if matches!(op, Op::Eq | Op::Ne) && self.peek() == Some(b'/') {
            self.regex()?
        } else if let Some(q @ (b'"' | b'\'')) = self.peek() {
            self.pos += 1;
            let mut out = String::new();
            loop {
                let Some(c) = self.src[self.pos..].chars().next() else {
                    return self.err();
                };
                self.pos += c.len_utf8();
                if c as u32 == u32::from(q) {
                    break;
                }
                if c == '\\' {
                    let Some(e) = self.src[self.pos..].chars().next() else {
                        return self.err();
                    };
                    self.pos += e.len_utf8();
                    out.push(match e {
                        'b' => '\u{8}',
                        'f' => '\u{c}',
                        'n' => '\n',
                        'r' => '\r',
                        't' => '\t',
                        'v' => '\u{b}',
                        other => other,
                    });
                } else {
                    out.push(c);
                }
            }
            Value::Str(out)
        } else if let Some(n) = self.number() {
            Value::Num(n)
        } else if let Some(n) = self.name() {
            Value::Str(n)
        } else {
            return self.err();
        };
        Ok(Sel::Attr {
            path,
            op: Some((op, value)),
        })
    }

    fn number(&mut self) -> Option<f64> {
        let start = self.pos;
        let digits = |p: &mut Self| {
            let s = p.pos;
            while p.peek().is_some_and(|b| b.is_ascii_digit()) {
                p.pos += 1;
            }
            p.pos > s
        };
        digits(self);
        let int_end = self.pos;
        if self.peek() == Some(b'.') {
            self.pos += 1;
            if !digits(self) {
                self.pos = int_end;
            }
        }
        if self.pos == start {
            return None;
        }
        // The grammar needs digits after an optional `digits.` prefix.
        let text = &self.src[start..self.pos];
        if text.ends_with('.') {
            self.pos = start;
            return None;
        }
        text.parse().ok().or_else(|| {
            self.pos = start;
            None
        })
    }

    fn regex(&mut self) -> PResult<Value> {
        let start = self.pos;
        self.pos += 1;
        let mut pat = String::new();
        loop {
            let Some(c) = self.src[self.pos..].chars().next() else {
                self.pos = start;
                return self.err();
            };
            match c {
                '/' => {
                    self.pos += 1;
                    break;
                }
                '\\' => {
                    self.pos += 1;
                    let Some(e) = self.src[self.pos..].chars().next() else {
                        return self.err();
                    };
                    self.pos += e.len_utf8();
                    pat.push('\\');
                    pat.push(e);
                }
                '[' => {
                    self.pos += 1;
                    pat.push('[');
                    let mut n = 0;
                    loop {
                        let Some(e) = self.src[self.pos..].chars().next() else {
                            return self.err();
                        };
                        self.pos += e.len_utf8();
                        if e == ']' {
                            break;
                        }
                        pat.push(e);
                        n += 1;
                        if e == '\\' {
                            let Some(f) = self.src[self.pos..].chars().next() else {
                                return self.err();
                            };
                            self.pos += f.len_utf8();
                            pat.push(f);
                        }
                    }
                    if n == 0 {
                        return self.err();
                    }
                    pat.push(']');
                }
                _ => {
                    self.pos += c.len_utf8();
                    pat.push(c);
                }
            }
        }
        if pat.is_empty() {
            return self.err();
        }
        let fstart = self.pos;
        while self
            .peek()
            .is_some_and(|b| matches!(b, b'i' | b'm' | b's' | b'u'))
        {
            self.pos += 1;
        }
        let flags = self.src[fstart..self.pos].to_owned();
        if crate::regex::parse(&pat, Flags::new(&flags)).is_err() {
            return Err(format!("Invalid regular expression: /{pat}/{flags}"));
        }
        Ok(Value::Regex {
            pattern: pat,
            flags,
        })
    }
}

/// Parses a selector (`esquery.parse`).
pub fn parse(raw: &str) -> Result<Selector, String> {
    let mut p = Parser {
        s: raw.as_bytes(),
        src: raw,
        pos: 0,
        depth: 0,
    };
    p.spaces();
    if p.pos == raw.len() {
        return Err(format!("Empty selector `{raw}`"));
    }
    let mut ss = p.selectors()?;
    p.spaces();
    if p.pos != raw.len() {
        return p.err();
    }
    let root = if ss.len() == 1 {
        ss.pop().unwrap_or(Sel::Wildcard)
    } else {
        Sel::Matches(ss)
    };
    let mut regexes = Vec::new();
    collect_regexes(&root, &mut regexes);
    let mut attribute_count = 0;
    let mut identifier_count = 0;
    let node_types = analyze(&root, &mut attribute_count, &mut identifier_count);
    Ok(Selector {
        root,
        regexes,
        raw: raw.to_owned(),
        attribute_count,
        identifier_count,
        node_types,
    })
}

fn collect_regexes(s: &Sel, out: &mut Vec<RegexCache>) {
    match s {
        Sel::Attr {
            op: Some((_, Value::Regex { pattern, flags })),
            ..
        } => out.push(RegexCache {
            pattern: pattern.clone(),
            flags: flags.clone(),
        }),
        Sel::Not(v) | Sel::Matches(v) | Sel::Has(v) | Sel::Compound(v, _) => {
            v.iter().for_each(|s| collect_regexes(s, out))
        }
        Sel::Binary(_, l, r) => {
            collect_regexes(l, out);
            collect_regexes(r, out);
        }
        _ => {}
    }
}

/// ESLint's selector analysis: specificity counts and the possible node types.
fn analyze(s: &Sel, attrs: &mut u32, idents: &mut u32) -> Option<Vec<String>> {
    match s {
        Sel::Ident(_, raw) => {
            *idents += 1;
            Some(vec![raw.clone()])
        }
        Sel::Not(v) => {
            for s in v {
                analyze(s, attrs, idents);
            }
            None
        }
        Sel::Matches(v) => {
            let parts: Vec<_> = v.iter().map(|s| analyze(s, attrs, idents)).collect();
            if parts.iter().all(Option::is_some) {
                let mut out: Vec<String> = Vec::new();
                for p in parts.into_iter().flatten() {
                    for t in p {
                        if !out.contains(&t) {
                            out.push(t);
                        }
                    }
                }
                Some(out)
            } else {
                None
            }
        }
        Sel::Compound(v, _) => {
            let parts: Vec<Vec<String>> =
                v.iter().filter_map(|s| analyze(s, attrs, idents)).collect();
            let mut it = parts.into_iter();
            let first = it.next()?;
            Some(it.fold(first, |acc, p| {
                acc.into_iter().filter(|t| p.contains(t)).collect()
            }))
        }
        Sel::Attr { .. } | Sel::Field(_) | Sel::Nth(_) => {
            *attrs += 1;
            None
        }
        Sel::Binary(_, l, r) => {
            analyze(l, attrs, idents);
            analyze(r, attrs, idents)
        }
        Sel::Class(Class::Function) => Some(vec![
            "FunctionDeclaration".into(),
            "FunctionExpression".into(),
            "ArrowFunctionExpression".into(),
        ]),
        _ => None,
    }
}

impl Selector {
    /// ESLint's listener order for selectors matching the same node.
    pub fn compare(&self, other: &Selector) -> std::cmp::Ordering {
        self.attribute_count
            .cmp(&other.attribute_count)
            .then(self.identifier_count.cmp(&other.identifier_count))
            .then_with(|| self.raw.cmp(&other.raw))
    }

    /// Whether ESLint would try this selector on a node of type `ty` (its node-type buckets
    /// are case-sensitive, unlike type matching inside the selector).
    pub fn may_match_type(&self, ty: &str) -> bool {
        self.node_types
            .as_ref()
            .is_none_or(|ts| ts.iter().any(|t| t == ty))
    }

    /// Parses the selector's regexes once, for matching many nodes.
    pub fn prepare(&self) -> Prepared<'_> {
        Prepared {
            sel: self,
            regexes: self
                .regexes
                .iter()
                .map(|r| crate::regex::parse(&r.pattern, Flags::new(&r.flags)).ok())
                .collect(),
        }
    }

    /// `esquery.matches(node, selector, ancestry)` where the ancestry is the first `avail`
    /// ancestors of `n` (`u32::MAX` for all of them).
    pub fn matches<T: Tree + ?Sized>(&self, t: &T, n: u32, avail: u32) -> bool {
        self.prepare().matches(t, n, avail)
    }
}

/// A selector with its regexes parsed.
pub struct Prepared<'s> {
    pub sel: &'s Selector,
    regexes: Vec<Option<Regex<'s>>>,
}

impl std::fmt::Debug for Prepared<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Prepared")
            .field("sel", &self.sel.raw)
            .finish()
    }
}

impl Prepared<'_> {
    /// See [`Selector::matches`].
    pub fn matches<T: Tree + ?Sized>(&self, t: &T, n: u32, avail: u32) -> bool {
        let m = Matcher {
            t,
            regexes: &self.regexes,
            ri: &self.sel.regexes,
            steps: std::cell::Cell::new(0),
        };
        m.matches(&self.sel.root, n, avail)
    }

    /// Matches like ESLint's traverser: the node must be in one of the selector's type
    /// buckets, and the whole ancestry is available.
    pub fn matches_listener<T: Tree + ?Sized>(&self, t: &T, n: u32) -> bool {
        self.sel.may_match_type(t.ty(n)) && self.matches(t, n, u32::MAX)
    }
}

struct Matcher<'m, 'r, T: Tree + ?Sized> {
    t: &'m T,
    regexes: &'m [Option<Regex<'r>>],
    ri: &'m [RegexCache],
    steps: std::cell::Cell<u32>,
}

/// Bound on matcher work per top-level match (`:has` inside `:has` over a large file).
const MAX_STEPS: u32 = 5_000_000;

impl<T: Tree + ?Sized> Matcher<'_, '_, T> {
    fn tick(&self) -> bool {
        let s = self.steps.get().saturating_add(1);
        self.steps.set(s);
        s <= MAX_STEPS
    }

    fn parent(&self, n: u32, avail: u32) -> Option<u32> {
        if avail == 0 { None } else { self.t.parent(n) }
    }

    fn regex_index(&self, pattern: &str, flags: &str) -> Option<usize> {
        self.ri
            .iter()
            .position(|r| r.pattern == pattern && r.flags == flags)
    }

    fn regex_test(&self, pattern: &str, flags: &str, text: &str) -> bool {
        let Some(i) = self.regex_index(pattern, flags) else {
            return false;
        };
        match &self.regexes[i] {
            Some(r) => r.is_match(text, flags.contains('i')),
            None => false,
        }
    }

    fn matches(&self, s: &Sel, n: u32, avail: u32) -> bool {
        if !self.tick() {
            return false;
        }
        let t = self.t;
        match s {
            Sel::Wildcard => true,
            Sel::ExactNode => avail == 0,
            Sel::Ident(lower, _) => t.ty(n).eq_ignore_ascii_case(lower),
            Sel::Attr { path, op } => {
                let path: Vec<&str> = path.iter().map(String::as_str).collect();
                let v = t.attr(n, &path);
                match op {
                    None => !v.is_nullish(),
                    Some((op, value)) => self.compare(&v, *op, value),
                }
            }
            Sel::Field(path) => self.in_field(n, avail, path),
            Sel::Not(v) => !v.iter().any(|s| self.matches(s, n, avail)),
            Sel::Matches(v) => v.iter().any(|s| self.matches(s, n, avail)),
            Sel::Compound(v, _) => v.iter().all(|s| self.matches(s, n, avail)),
            Sel::Has(v) => v.iter().any(|s| self.has(s, n)),
            Sel::Nth(k) => self.nth(n, avail, *k),
            Sel::Class(c) => self.class(*c, n, avail),
            Sel::Binary(op, l, r) => match op {
                Comb::Child => {
                    self.matches(r, n, avail)
                        && self
                            .parent(n, avail)
                            .is_some_and(|p| self.matches(l, p, avail - 1))
                }
                Comb::Descendant => {
                    if !self.matches(r, n, avail) {
                        return false;
                    }
                    let mut cur = n;
                    let mut a = avail;
                    while let Some(p) = self.parent(cur, a) {
                        a -= 1;
                        if self.matches(l, p, a) {
                            return true;
                        }
                        cur = p;
                    }
                    false
                }
                Comb::Sibling => {
                    (self.matches(r, n, avail) && self.sibling(n, avail, l, true, false))
                        || (is_subject(l)
                            && self.matches(l, n, avail)
                            && self.sibling(n, avail, r, false, false))
                }
                Comb::Adjacent => {
                    (self.matches(r, n, avail) && self.sibling(n, avail, l, true, true))
                        || (is_subject(r)
                            && self.matches(l, n, avail)
                            && self.sibling(n, avail, r, false, true))
                }
            },
        }
    }

    fn compare(&self, v: &Val<'_>, op: Op, value: &Value) -> bool {
        match (op, value) {
            (Op::Eq, Value::Regex { pattern, flags }) => {
                matches!(v, Val::Str(s) if self.regex_test(pattern, flags, s))
            }
            (Op::Ne, Value::Regex { pattern, flags }) => {
                !self.regex_test(pattern, flags, &v.to_js_string())
            }
            (Op::Eq, Value::Type(ty)) => v.type_of() == ty,
            (Op::Ne, Value::Type(ty)) => v.type_of() != ty,
            (Op::Eq | Op::Ne, lit) => {
                let lit = match lit {
                    Value::Str(s) => Cow::Borrowed(s.as_str()),
                    Value::Num(n) => Cow::Owned(js_number(*n)),
                    _ => return false,
                };
                (v.to_js_string() == lit) == (op == Op::Eq)
            }
            (_, lit) => {
                let ord = match (v, lit) {
                    (Val::Str(a), Value::Str(b)) => Some(a.as_ref().cmp(b.as_str())),
                    (_, Value::Num(b)) => v.to_number().partial_cmp(b),
                    (_, Value::Str(b)) => {
                        let b: f64 = b.trim().parse().unwrap_or(f64::NAN);
                        v.to_number().partial_cmp(&b)
                    }
                    _ => None,
                };
                let Some(ord) = ord else { return false };
                match op {
                    Op::Lt => ord.is_lt(),
                    Op::Le => ord.is_le(),
                    Op::Gt => ord.is_gt(),
                    Op::Ge => ord.is_ge(),
                    _ => false,
                }
            }
        }
    }

    fn in_field(&self, n: u32, avail: u32, path: &[String]) -> bool {
        let len = path.len() as u32;
        if avail < len {
            return false;
        }
        let mut cur = n;
        for key in path.iter().rev() {
            if self.t.key(cur) != key {
                return false;
            }
            match self.t.parent(cur) {
                Some(p) => cur = p,
                None => return false,
            }
        }
        true
    }

    fn nth(&self, n: u32, avail: u32, k: i64) -> bool {
        if k == 0 || self.parent(n, avail).is_none() {
            return false;
        }
        let Some((idx, len)) = self.t.list_pos(n) else {
            return false;
        };
        let want = if k < 0 { i64::from(len) + k } else { k - 1 };
        want == i64::from(idx)
    }

    fn sibling(&self, n: u32, avail: u32, s: &Sel, left: bool, adjacent: bool) -> bool {
        if self.parent(n, avail).is_none() {
            return false;
        }
        let Some((idx, len)) = self.t.list_pos(n) else {
            return false;
        };
        let range: Vec<u32> = match (left, adjacent) {
            (true, true) => idx.checked_sub(1).into_iter().collect(),
            (false, true) => (idx + 1 < len).then_some(idx + 1).into_iter().collect(),
            (true, false) => (0..idx).collect(),
            (false, false) => (idx + 1..len).collect(),
        };
        range.into_iter().any(|i| {
            self.t
                .list_item(n, i)
                .is_some_and(|m| self.matches(s, m, avail))
        })
    }

    fn has(&self, s: &Sel, root: u32) -> bool {
        let mut stack = vec![(root, 0u32)];
        while let Some((n, depth)) = stack.pop() {
            if !self.tick() {
                return false;
            }
            if self.matches(s, n, depth) {
                return true;
            }
            for &c in self.t.children(n).iter().rev() {
                stack.push((c, depth + 1));
            }
        }
        false
    }

    fn class(&self, c: Class, n: u32, avail: u32) -> bool {
        let ty = self.t.ty(n);
        let expression = || {
            ty.ends_with("Expression")
                || ty.ends_with("Literal")
                || (ty == "Identifier"
                    && self
                        .parent(n, avail)
                        .is_none_or(|p| self.t.ty(p) != "MetaProperty"))
                || ty == "MetaProperty"
        };
        match c {
            Class::Statement => ty.ends_with("Statement") || ty.ends_with("Declaration"),
            Class::Declaration => ty.ends_with("Declaration"),
            Class::Pattern => ty.ends_with("Pattern") || expression(),
            Class::Expression => expression(),
            Class::Function => matches!(
                ty,
                "FunctionDeclaration" | "FunctionExpression" | "ArrowFunctionExpression"
            ),
        }
    }
}

fn is_subject(s: &Sel) -> bool {
    matches!(s, Sel::Compound(_, true))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One test node: (type, parent, key, list position).
    type TNode = (&'static str, Option<u32>, &'static str, Option<(u32, u32)>);

    /// A tiny tree.
    struct T {
        nodes: Vec<TNode>,
        kids: Vec<Vec<u32>>,
        names: Vec<Option<&'static str>>,
    }

    impl Tree for T {
        fn ty(&self, n: u32) -> &str {
            self.nodes[n as usize].0
        }
        fn parent(&self, n: u32) -> Option<u32> {
            self.nodes[n as usize].1
        }
        fn key(&self, n: u32) -> &str {
            self.nodes[n as usize].2
        }
        fn list_pos(&self, n: u32) -> Option<(u32, u32)> {
            self.nodes[n as usize].3
        }
        fn list_item(&self, n: u32, i: u32) -> Option<u32> {
            let p = self.parent(n)?;
            let key = self.key(n);
            self.kids[p as usize]
                .iter()
                .copied()
                .filter(|&c| self.key(c) == key)
                .find(|&c| self.list_pos(c).is_some_and(|(j, _)| j == i))
        }
        fn children(&self, n: u32) -> &[u32] {
            &self.kids[n as usize]
        }
        fn attr(&self, n: u32, path: &[&str]) -> Val<'_> {
            match path {
                ["name"] => self.names[n as usize].map_or(Val::Undef, |s| Val::Str(s.into())),
                ["params", "length"] => Val::Num(self.kids[n as usize].len() as f64),
                _ => Val::Undef,
            }
        }
    }

    /// Program > FunctionDeclaration(params: [Identifier a, Identifier b]) > BlockStatement
    fn tree() -> T {
        T {
            nodes: vec![
                ("Program", None, "", None),
                ("FunctionDeclaration", Some(0), "body", Some((0, 1))),
                ("Identifier", Some(1), "params", Some((0, 2))),
                ("Identifier", Some(1), "params", Some((1, 2))),
            ],
            kids: vec![vec![1], vec![2, 3], vec![], vec![]],
            names: vec![None, None, Some("a"), Some("b")],
        }
    }

    fn m(sel: &str, n: u32) -> bool {
        parse(sel).unwrap().matches(&tree(), n, u32::MAX)
    }

    #[test]
    fn basics() {
        assert!(m("Identifier", 2));
        assert!(m("identifier", 2));
        assert!(m("*", 0));
        assert!(m("Identifier[name=\"a\"]", 2));
        assert!(!m("Identifier[name='b']", 2));
        assert!(m("Identifier[name!=b]", 2));
        assert!(m("Identifier[name=/^a$/]", 2));
        assert!(m("FunctionDeclaration[params.length>1]", 1));
        assert!(!m("FunctionDeclaration[params.length>2]", 1));
        assert!(m("FunctionDeclaration > Identifier", 3));
        assert!(m("Program Identifier", 3));
        assert!(!m("Program > Identifier", 3));
        assert!(m("Identifier ~ Identifier", 3));
        assert!(!m("Identifier ~ Identifier", 2));
        assert!(m("Identifier + Identifier", 3));
        assert!(m(":first-child", 2));
        assert!(m(":last-child", 3));
        assert!(m(":nth-child(2)", 3));
        assert!(m(":nth-last-child(2)", 2));
        assert!(m("FunctionDeclaration:has(Identifier[name=b])", 1));
        assert!(m("FunctionDeclaration:has(> Identifier)", 1));
        assert!(!m("Program:has(> Identifier)", 0));
        assert!(m(":not(Program)", 1));
        assert!(m(":matches(Program, FunctionDeclaration)", 1));
        assert!(m(":is(Program, FunctionDeclaration)", 1));
        assert!(m(".params", 2));
        assert!(!m(".body", 2));
        assert!(m(":function", 1));
        assert!(m(":statement", 1));
        assert!(m(":expression", 2));
        assert!(m("Identifier[name]", 2));
        assert!(!m("Program[name]", 0));
        assert!(m("Identifier[name=type(string)]", 2));
    }

    #[test]
    fn ancestry_limits() {
        let s = parse("FunctionDeclaration > Identifier").unwrap();
        assert!(!s.matches(&tree(), 2, 0));
        assert!(s.matches(&tree(), 2, 1));
    }

    #[test]
    fn specificity() {
        let a = parse("Identifier").unwrap();
        let b = parse("Identifier[name=\"bar\"]").unwrap();
        assert!(a.compare(&b).is_lt());
        assert_eq!(b.attribute_count, 1);
        assert_eq!(
            b.node_types.as_deref(),
            Some(&["Identifier".to_owned()][..])
        );
    }

    #[test]
    fn errors() {
        for bad in [
            "", "[", "Foo[", ":nope", "a >", ":not(", "[a=/b/x]", "[a=/(/]",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
        let deep = ":not(".repeat(200) + "A" + &")".repeat(200);
        assert!(parse(&deep).is_err());
    }
}
