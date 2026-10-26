//! CSS for the bundler: a tokenizer ([`lexer`]), a small rule tree ([`Sheet`]), the
//! references a stylesheet makes (`@import`, `url()`, `composes ... from`), local names for
//! the `local-css` loader (CSS modules), and a printer ([`print`]). Bundling (import order,
//! `@import` conditions, deduplication) is in [`order`].
//!
//! The tree keeps tokens as byte ranges into the source; only what the bundler rewrites is
//! decoded: reference targets, local names, `@import` conditions. Behaviour follows
//! esbuild's documented and observed output (its `bundler_css_test.go` cases as data).

pub mod calc;
pub mod color;
pub mod lexer;
pub mod link;
pub mod order;
pub mod print;

use std::sync::Arc;

use rustc_hash::FxHashMap;

use lexer::{T, Token};

/// Nesting limit for blocks and functions: deeper input is reported, not recursed into.
const MAX_DEPTH: usize = 128;

/// One component value (CSS Syntax 5.4.7), with the bundler's rewrites resolved.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Cv {
    Tok(Token),
    /// `(...)`, `[...]` or `{...}`; `open` is the opening byte.
    Block {
        open: u8,
        items: Vec<Cv>,
    },
    /// A function: `name` is the function token (including `(`).
    Func {
        name: Token,
        items: Vec<Cv>,
    },
    /// A `url()` (token or function) that refers to record `record`.
    Url {
        record: u32,
    },
    /// A local name (`local-css`), with `.` or `#` in front of it or nothing (`0`).
    Local {
        name: u32,
        prefix: u8,
    },
}

/// A rule, at-rule or declaration.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Node {
    Decl {
        name: Token,
        value: Vec<Cv>,
        important: bool,
    },
    Rule {
        prelude: Vec<Cv>,
        body: Vec<Node>,
    },
    At {
        /// Lower-case name without `@`.
        name: String,
        /// The name as written (esbuild prints it so, except `@media` and `@layer`).
        raw: Box<str>,
        prelude: Vec<Cv>,
        body: Option<Vec<Node>>,
    },
    /// A top-level `@import` (index into [`Sheet::imports`]).
    Import(u32),
}

/// What a reference is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RecKind {
    /// `@import`.
    Import,
    /// `url()` (also inside `@import` conditions).
    Url,
    /// `composes: x from "file"`.
    Composes,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CssRecord {
    pub kind: RecKind,
    pub spec: String,
    /// Byte offset of the reference, for messages.
    pub pos: u32,
}

/// An `@import` rule's conditions: `layer` / `layer(x)`, `supports(...)` and media queries.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Conditions {
    /// `None`: no layer; `Some(empty)`: anonymous `layer`; else the layer name tokens.
    pub layer: Option<Vec<Cv>>,
    pub supports: Option<Vec<Cv>>,
    pub media: Vec<Cv>,
}

impl Conditions {
    pub fn is_empty(&self) -> bool {
        self.layer.is_none() && self.supports.is_none() && self.media.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ImportRule {
    pub record: u32,
    pub conditions: Conditions,
    pub pos: u32,
}

/// `composes` in a rule for local class `class`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Compose {
    pub class: u32,
    pub names: Vec<String>,
    /// `from "file"`: the record; `from global`: `Global`; same file: `Local`.
    pub from: ComposeFrom,
    pub pos: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ComposeFrom {
    Local,
    Global,
    Record(u32),
}

/// A problem found in a stylesheet.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CssMessage {
    pub error: bool,
    pub pos: u32,
    pub text: String,
}

/// A parsed stylesheet.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Sheet {
    pub src: Arc<str>,
    /// Identifies the module in JavaScript placeholders (`prepare::css_placeholder`).
    pub id: String,
    pub rules: Vec<Node>,
    pub records: Vec<CssRecord>,
    pub imports: Vec<ImportRule>,
    /// Local names in order of first appearance (decoded).
    pub locals: Vec<String>,
    /// Local names a selector defines as a class or ID.
    pub exported: Vec<u32>,
    /// Global class and ID names in selectors (local names avoid them).
    pub globals: Vec<String>,
    pub composes: Vec<Compose>,
    pub messages: Vec<CssMessage>,
    /// The file starts with `@charset` (removed; the output starts with `@charset "UTF-8";`).
    pub charset: bool,
}

impl Sheet {
    pub fn text(&self, t: &Token) -> &str {
        t.text(&self.src)
    }
}

/// How names in selectors are read: CSS modules (`local-css`) make classes, IDs and
/// keyframe names local unless `:global`; plain CSS (`css`, `global-css`) only with `:local`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameMode {
    Global,
    Local,
}

/// Parses `src`. `mode` is [`NameMode::Local`] for the `local-css` loader.
pub fn parse(src: Arc<str>, mode: NameMode) -> Sheet {
    let (toks, warnings) = lexer::tokenize(&src);
    let mut p = Parser {
        src: Arc::clone(&src),
        toks,
        i: 0,
        sheet: Sheet {
            src: Arc::clone(&src),
            ..Sheet::default()
        },
        local_ids: FxHashMap::default(),
        mode,
        depth: 0,
        compose_target: None,
        too_deep: false,
    };
    for w in warnings {
        p.sheet.messages.push(CssMessage {
            error: false,
            pos: w.pos,
            text: w.text,
        });
    }
    let cvs = p.component_values(None);
    let rules = p.rules(cvs, true);
    p.sheet.rules = rules;
    p.sheet
}

struct Parser {
    src: Arc<str>,
    toks: Vec<Token>,
    i: usize,
    sheet: Sheet,
    local_ids: FxHashMap<String, u32>,
    mode: NameMode,
    /// The class a `composes` in the current rule applies to (a nested rule whose selector
    /// is empty or `&` composes for its parent).
    compose_target: Option<u32>,
    depth: usize,
    too_deep: bool,
}

fn closer(t: T) -> Option<T> {
    match t {
        T::OpenParen | T::Function => Some(T::CloseParen),
        T::OpenSquare => Some(T::CloseSquare),
        T::OpenCurly => Some(T::CloseCurly),
        _ => None,
    }
}

fn is_ws(c: &Cv) -> bool {
    matches!(c, Cv::Tok(Token { t: T::Ws, .. }))
}

fn trim(v: &[Cv]) -> &[Cv] {
    let mut a = 0;
    let mut b = v.len();
    while a < b && is_ws(&v[a]) {
        a += 1;
    }
    while b > a && is_ws(&v[b - 1]) {
        b -= 1;
    }
    &v[a..b]
}

impl Parser {
    fn text(&self, t: &Token) -> &str {
        t.text(&self.src)
    }

    fn warn(&mut self, pos: u32, text: impl Into<String>) {
        self.sheet.messages.push(CssMessage {
            error: false,
            pos,
            text: text.into(),
        });
    }

    /// Component values until `end` (consumed) or the end of input.
    fn component_values(&mut self, end: Option<(T, u32)>) -> Vec<Cv> {
        let mut out = Vec::new();
        while self.i < self.toks.len() {
            let tok = self.toks[self.i];
            self.i += 1;
            if let Some((e, _)) = end
                && tok.t == e
            {
                return out;
            }
            match closer(tok.t) {
                Some(c) => {
                    if self.depth >= MAX_DEPTH {
                        if !self.too_deep {
                            self.too_deep = true;
                            self.sheet.messages.push(CssMessage {
                                error: true,
                                pos: tok.start,
                                text: "CSS nesting is too deep".into(),
                            });
                        }
                        self.skip_block(c);
                        continue;
                    }
                    self.depth += 1;
                    let items = self.component_values(Some((c, tok.start)));
                    self.depth -= 1;
                    if tok.t == T::Function {
                        out.push(Cv::Func { name: tok, items });
                    } else {
                        let open = self.src.as_bytes()[tok.start as usize];
                        out.push(Cv::Block { open, items });
                    }
                }
                None => out.push(Cv::Tok(tok)),
            }
        }
        if let Some((e, start)) = end {
            let open = match e {
                T::CloseParen => "(",
                T::CloseSquare => "[",
                _ => "{",
            };
            let close = match e {
                T::CloseParen => ")",
                T::CloseSquare => "]",
                _ => "}",
            };
            self.warn(start, format!("Expected \"{close}\" to go with \"{open}\""));
        }
        out
    }

    /// Skips tokens to the end of a block too deep to keep (iteratively).
    fn skip_block(&mut self, close: T) {
        let mut stack = vec![close];
        while self.i < self.toks.len() {
            let t = self.toks[self.i].t;
            self.i += 1;
            if Some(&t) == stack.last() {
                stack.pop();
                if stack.is_empty() {
                    return;
                }
            } else if let Some(c) = closer(t) {
                stack.push(c);
            }
        }
    }

    /// Rules and declarations from a list of component values. At the top level there are
    /// only rules (and `<!--`/`-->` are ignored).
    fn rules(&mut self, cvs: Vec<Cv>, top: bool) -> Vec<Node> {
        let mut out = Vec::new();
        let mut it = cvs.into_iter().peekable();
        let mut seen_rule = false;
        let mut seen_import = false;
        while let Some(cv) = it.next() {
            match &cv {
                Cv::Tok(Token { t: T::Ws, .. })
                | Cv::Tok(Token {
                    t: T::Semicolon, ..
                }) => {
                    continue;
                }
                Cv::Tok(Token {
                    t: T::Cdo | T::Cdc, ..
                }) if top => continue,
                Cv::Tok(
                    tok @ Token {
                        t: T::AtKeyword, ..
                    },
                ) => {
                    let tok = *tok;
                    let raw: Box<str> = self.text(&tok)[1..].into();
                    let name = raw.to_ascii_lowercase();
                    let mut prelude = Vec::new();
                    let mut body = None;
                    for cv in it.by_ref() {
                        match cv {
                            Cv::Tok(Token {
                                t: T::Semicolon, ..
                            }) => break,
                            Cv::Block { open: b'{', items } => {
                                body = Some(items);
                                break;
                            }
                            other => prelude.push(other),
                        }
                    }
                    let prelude = trim(&prelude).to_vec();
                    if top && name == "import" && body.is_none() {
                        // A late `@import` stays as written, unbundled (observed in esbuild).
                        if seen_rule {
                            self.warn(tok.start, "All \"@import\" rules must come first");
                            out.push(Node::At {
                                name,
                                raw,
                                prelude,
                                body: None,
                            });
                            continue;
                        }
                        seen_import = true;
                        if let Some(n) = self.import_rule(prelude, tok.start) {
                            out.push(n);
                        }
                        continue;
                    }
                    if top && name == "charset" && body.is_none() {
                        if out.is_empty() && !seen_rule && !seen_import && !self.sheet.charset {
                            self.sheet.charset = true;
                            continue;
                        }
                        self.warn(tok.start, "\"@charset\" must be the first rule in the file");
                    }
                    // `@layer` statements may come before the imports, not between them.
                    if !(name == "charset" || name == "layer" && body.is_none() && !seen_import) {
                        seen_rule = true;
                    }
                    out.push(self.at_rule(name, raw, prelude, body));
                }
                _ => {
                    // A declaration (`name: value`) unless a `{}` block comes before the
                    // next `;` (a nested rule) or this is the top level.
                    let mut items = vec![cv];
                    let mut block = None;
                    while let Some(next) = it.peek() {
                        if matches!(
                            next,
                            Cv::Tok(Token {
                                t: T::Semicolon,
                                ..
                            })
                        ) {
                            it.next();
                            break;
                        }
                        let next = it.next().unwrap_or(Cv::Tok(Token {
                            t: T::Ws,
                            start: 0,
                            end: 0,
                        }));
                        if let Cv::Block { open: b'{', .. } = next {
                            let custom = matches!(&items[0], Cv::Tok(t) if t.t == T::Ident && self.text(t).starts_with("--"));
                            if top || !custom {
                                block = Some(next);
                                break;
                            }
                        }
                        items.push(next);
                    }
                    match block {
                        Some(Cv::Block { items: body, .. }) => {
                            seen_rule = true;
                            // The mode a selector ends in holds for its body too
                            // (`.a :global { animation: x }` names a global animation).
                            let (prelude, mode) = self.selector(trim(&items).to_vec());
                            let target = match trim(&prelude) {
                                [Cv::Local { name, prefix: b'.' }] => Some(*name),
                                [] => self.compose_target,
                                [Cv::Tok(t)] if t.t == T::Delim && self.text(t) == "&" => {
                                    self.compose_target
                                }
                                _ => None,
                            };
                            let saved = (self.mode, self.compose_target);
                            self.mode = mode;
                            self.compose_target = target;
                            self.depth += 1;
                            let body = if self.depth > MAX_DEPTH {
                                Vec::new()
                            } else {
                                self.rules(body, false)
                            };
                            self.depth -= 1;
                            let body = self.rule_composes(body);
                            (self.mode, self.compose_target) = saved;
                            // A nested rule with nothing left of its selector (`:global`)
                            // is its parent's (observed in esbuild).
                            if !top && trim(&prelude).is_empty() {
                                out.extend(body);
                            } else {
                                out.push(Node::Rule { prelude, body });
                            }
                        }
                        _ if top => {
                            if let Some(Cv::Tok(t)) = items.first() {
                                self.warn(t.start, "Unexpected token");
                            }
                        }
                        _ => {
                            if let Some(d) = self.declaration(items) {
                                out.push(d);
                            }
                        }
                    }
                }
            }
        }
        out
    }

    fn at_rule(
        &mut self,
        name: String,
        raw: Box<str>,
        prelude: Vec<Cv>,
        body: Option<Vec<Cv>>,
    ) -> Node {
        let prelude = match name.as_str() {
            "keyframes" | "-webkit-keyframes" | "-moz-keyframes" | "-o-keyframes"
            | "counter-style" => self.localize_name_prelude(prelude),
            _ => prelude,
        };
        let body = body.map(|b| {
            self.depth += 1;
            let r = if self.depth > MAX_DEPTH {
                Vec::new()
            } else {
                self.rules(b, false)
            };
            self.depth -= 1;
            r
        });
        Node::At {
            name,
            raw,
            prelude,
            body,
        }
    }

    /// `@keyframes name`: the name is local in local mode (a string name stays as written).
    fn localize_name_prelude(&mut self, prelude: Vec<Cv>) -> Vec<Cv> {
        if self.mode != NameMode::Local {
            return prelude;
        }
        prelude
            .into_iter()
            .map(|cv| match cv {
                Cv::Tok(t) if t.t == T::Ident => {
                    let n = lexer::unescape(self.text(&t));
                    Cv::Local {
                        name: self.local(&n),
                        prefix: 0,
                    }
                }
                other => other,
            })
            .collect()
    }

    fn local(&mut self, name: &str) -> u32 {
        if let Some(&i) = self.local_ids.get(name) {
            return i;
        }
        let i = self.sheet.locals.len() as u32;
        self.sheet.locals.push(name.to_owned());
        self.local_ids.insert(name.to_owned(), i);
        i
    }

    fn exported(&mut self, name: u32) {
        if !self.sheet.exported.contains(&name) {
            self.sheet.exported.push(name);
        }
    }

    /// A selector with local names resolved: `.x` and `#x` become [`Cv::Local`] in local
    /// mode, `:local(...)` / `:global(...)` and bare `:local` / `:global` switch the mode
    /// and are removed.
    fn selector(&mut self, prelude: Vec<Cv>) -> (Vec<Cv>, NameMode) {
        let mut out = Vec::with_capacity(prelude.len());
        let mut mode = self.mode;
        self.selector_into(prelude, &mut mode, &mut out);
        (out, mode)
    }

    fn selector_into(&mut self, items: Vec<Cv>, mode: &mut NameMode, out: &mut Vec<Cv>) {
        let base = self.mode;
        let mut i = 0;
        let items_len = items.len();
        let mut items = items.into_iter().map(Some).collect::<Vec<_>>();
        while i < items_len {
            let Some(cv) = items[i].take() else {
                i += 1;
                continue;
            };
            i += 1;
            match cv {
                Cv::Tok(t) if t.t == T::Comma => {
                    *mode = base;
                    out.push(cv);
                }
                Cv::Tok(t) if t.t == T::Colon => {
                    // `:global`, `:local`, `:global(...)`, `:local(...)`
                    let next = items.get(i).and_then(|x| x.as_ref());
                    match next {
                        Some(Cv::Tok(n)) if n.t == T::Ident => {
                            let word = self.text(n).to_ascii_lowercase();
                            if word == "global" || word == "local" {
                                *mode = if word == "global" {
                                    NameMode::Global
                                } else {
                                    NameMode::Local
                                };
                                items[i] = None;
                                i += 1;
                                // Drop the whitespace after a bare `:global` at the start.
                                if out.iter().all(is_ws) {
                                    while matches!(items.get(i), Some(Some(c)) if is_ws(c)) {
                                        items[i] = None;
                                        i += 1;
                                    }
                                }
                                continue;
                            }
                            out.push(cv);
                        }
                        Some(Cv::Func { name, .. }) => {
                            let word = self.text(name).to_ascii_lowercase();
                            if word == "global(" || word == "local(" {
                                let Some(Some(Cv::Func { items: inner, .. })) =
                                    items.get_mut(i).map(Option::take)
                                else {
                                    continue;
                                };
                                i += 1;
                                let mut m = if word == "global(" {
                                    NameMode::Global
                                } else {
                                    NameMode::Local
                                };
                                self.selector_into(trim(&inner).to_vec(), &mut m, out);
                                continue;
                            }
                            out.push(cv);
                        }
                        _ => out.push(cv),
                    }
                }
                Cv::Tok(t) if t.t == T::Delim && self.text(&t) == "." => {
                    match items.get(i).and_then(|x| x.as_ref()) {
                        Some(Cv::Tok(n)) if n.t == T::Ident && *mode == NameMode::Local => {
                            let name = lexer::unescape(self.text(n));
                            let id = self.local(&name);
                            self.exported(id);
                            items[i] = None;
                            i += 1;
                            out.push(Cv::Local {
                                name: id,
                                prefix: b'.',
                            });
                        }
                        Some(Cv::Tok(n)) if n.t == T::Ident => {
                            let name = lexer::unescape(self.text(n));
                            if !self.sheet.globals.contains(&name) {
                                self.sheet.globals.push(name);
                            }
                            out.push(cv);
                        }
                        _ => out.push(cv),
                    }
                }
                Cv::Tok(t) if t.t == T::IdHash && *mode == NameMode::Global => {
                    let name = lexer::unescape(&self.text(&t)[1..]);
                    if !self.sheet.globals.contains(&name) {
                        self.sheet.globals.push(name);
                    }
                    out.push(cv);
                }
                Cv::Tok(t) if t.t == T::IdHash && *mode == NameMode::Local => {
                    let name = lexer::unescape(&self.text(&t)[1..]);
                    let id = self.local(&name);
                    self.exported(id);
                    out.push(Cv::Local {
                        name: id,
                        prefix: b'#',
                    });
                }
                Cv::Func { name, items: inner } => {
                    // `:is(.a)`, `:not(.b)`, `:has(.c)`: selectors inside keep the mode.
                    let mut m = *mode;
                    let mut v = Vec::new();
                    self.selector_into(inner, &mut m, &mut v);
                    out.push(Cv::Func { name, items: v });
                }
                other => out.push(other),
            }
        }
    }

    /// A declaration from its component values (`name`, `:`, value).
    fn declaration(&mut self, items: Vec<Cv>) -> Option<Node> {
        let items = trim(&items).to_vec();
        let mut it = items.into_iter();
        let name = match it.next()? {
            Cv::Tok(t) if t.t == T::Ident => t,
            Cv::Tok(t) => {
                self.warn(t.start, "Expected identifier");
                return None;
            }
            _ => return None,
        };
        let rest: Vec<Cv> = it.collect();
        let rest = trim(&rest);
        match rest.first() {
            Some(Cv::Tok(t)) if t.t == T::Colon => {}
            _ => {
                self.warn(name.end, "Expected \":\"");
                return None;
            }
        }
        let mut value = trim(&rest[1..]).to_vec();
        // `!important`
        let mut important = false;
        let n = value.len();
        if n >= 2
            && let Cv::Tok(last) = &value[n - 1]
            && last.t == T::Ident
            && self.text(last).eq_ignore_ascii_case("important")
        {
            let mut k = n - 1;
            while k > 0 && is_ws(&value[k - 1]) {
                k -= 1;
            }
            if k > 0
                && let Cv::Tok(bang) = &value[k - 1]
                && bang.t == T::Delim
                && self.text(bang) == "!"
            {
                important = true;
                value.truncate(k - 1);
                value = trim(&value).to_vec();
            }
        }
        let lname = self.text(&name).to_ascii_lowercase();
        let value = self.urls_in(value);
        let value = if self.mode == NameMode::Local
            && matches!(
                lname.as_str(),
                "animation" | "animation-name" | "-webkit-animation" | "-webkit-animation-name"
            ) {
            self.localize_animation(value)
        } else {
            value
        };
        Some(Node::Decl {
            name,
            value,
            important,
        })
    }

    /// Animation names in local mode: identifiers that aren't animation keywords.
    fn localize_animation(&mut self, value: Vec<Cv>) -> Vec<Cv> {
        const KEYWORDS: &[&str] = &[
            "none",
            "initial",
            "inherit",
            "unset",
            "revert",
            "revert-layer",
            "infinite",
            "normal",
            "reverse",
            "alternate",
            "alternate-reverse",
            "forwards",
            "backwards",
            "both",
            "running",
            "paused",
            "ease",
            "ease-in",
            "ease-out",
            "ease-in-out",
            "linear",
            "step-start",
            "step-end",
            "auto",
        ];
        value
            .into_iter()
            .map(|cv| match cv {
                Cv::Tok(t)
                    if t.t == T::Ident
                        && !KEYWORDS.contains(&self.text(&t).to_ascii_lowercase().as_str()) =>
                {
                    let n = lexer::unescape(self.text(&t));
                    Cv::Local {
                        name: self.local(&n),
                        prefix: 0,
                    }
                }
                other => other,
            })
            .collect()
    }

    /// `url()` references in a value become records (nested in functions too).
    fn urls_in(&mut self, value: Vec<Cv>) -> Vec<Cv> {
        value
            .into_iter()
            .map(|cv| match cv {
                Cv::Tok(t) if t.t == T::Url => {
                    let spec = lexer::url_value(self.text(&t));
                    self.url_record(spec, t.start)
                }
                Cv::Func { name, items } if self.text(&name).eq_ignore_ascii_case("url(") => {
                    let inner = trim(&items);
                    match inner {
                        [Cv::Tok(s)] if s.t == T::Str => {
                            let spec = lexer::string_value(self.text(s));
                            self.url_record(spec, name.start)
                        }
                        _ => Cv::Func { name, items },
                    }
                }
                Cv::Func { name, items } => {
                    let items = self.urls_in(items);
                    Cv::Func { name, items }
                }
                Cv::Block { open, items } => {
                    let items = self.urls_in(items);
                    Cv::Block { open, items }
                }
                other => other,
            })
            .collect()
    }

    fn url_record(&mut self, spec: String, pos: u32) -> Cv {
        let record = self.sheet.records.len() as u32;
        self.sheet.records.push(CssRecord {
            kind: RecKind::Url,
            spec,
            pos,
        });
        Cv::Url { record }
    }

    /// `@import "x" layer(...) supports(...) media;`
    fn import_rule(&mut self, prelude: Vec<Cv>, pos: u32) -> Option<Node> {
        let mut it = prelude.into_iter().peekable();
        let (spec, spec_pos) = match it.next() {
            Some(Cv::Tok(t)) if t.t == T::Str => (lexer::string_value(self.text(&t)), t.start),
            Some(Cv::Tok(t)) if t.t == T::Url => (lexer::url_value(self.text(&t)), t.start),
            Some(Cv::Func { name, items }) if self.text(&name).eq_ignore_ascii_case("url(") => {
                match trim(&items) {
                    [Cv::Tok(s)] if s.t == T::Str => {
                        (lexer::string_value(self.text(s)), name.start)
                    }
                    _ => {
                        self.warn(name.start, "Expected URL or string");
                        return None;
                    }
                }
            }
            Some(_) | None => {
                self.warn(pos, "Expected URL or string");
                return None;
            }
        };
        let mut c = Conditions::default();
        let rest: Vec<Cv> = it.collect();
        let rest = trim(&rest).to_vec();
        let mut i = 0;
        let skip_ws = |i: &mut usize| {
            while *i < rest.len() && is_ws(&rest[*i]) {
                *i += 1;
            }
        };
        skip_ws(&mut i);
        match rest.get(i) {
            Some(Cv::Tok(t)) if t.t == T::Ident && self.text(t).eq_ignore_ascii_case("layer") => {
                c.layer = Some(Vec::new());
                i += 1;
            }
            Some(Cv::Func { name, items }) if self.text(name).eq_ignore_ascii_case("layer(") => {
                c.layer = Some(trim(items).to_vec());
                i += 1;
            }
            _ => {}
        }
        skip_ws(&mut i);
        if let Some(Cv::Func { name, items }) = rest.get(i)
            && self.text(name).eq_ignore_ascii_case("supports(")
        {
            // `url()`s in conditions are copied, never resolved (observed in esbuild).
            c.supports = Some(trim(items).to_vec());
            i += 1;
        }
        skip_ws(&mut i);
        c.media = rest[i.min(rest.len())..].to_vec();
        let record = self.sheet.records.len() as u32;
        self.sheet.records.push(CssRecord {
            kind: RecKind::Import,
            spec,
            pos: spec_pos,
        });
        let idx = self.sheet.imports.len() as u32;
        self.sheet.imports.push(ImportRule {
            record,
            conditions: c,
            pos,
        });
        Some(Node::Import(idx))
    }

    /// Moves `composes` declarations of a rule out of its body, for the class
    /// `compose_target` names (only a single class selector may compose).
    fn rule_composes(&mut self, body: Vec<Node>) -> Vec<Node> {
        let has = body.iter().any(|n| matches!(n, Node::Decl { name, .. } if self.text(name).eq_ignore_ascii_case("composes")));
        if !has {
            return body;
        }
        let class = self.compose_target;
        let mut out = Vec::with_capacity(body.len());
        for n in body {
            match n {
                Node::Decl { name, value, .. }
                    if self.text(&name).eq_ignore_ascii_case("composes") =>
                {
                    let Some(class) = class else {
                        self.warn(
                            name.start,
                            "\"composes\" only works inside single class selectors",
                        );
                        continue;
                    };
                    self.compose(class, value, name.start);
                }
                other => out.push(other),
            }
        }
        out
    }

    fn compose(&mut self, class: u32, value: Vec<Cv>, pos: u32) {
        let mut names = Vec::new();
        let mut from = ComposeFrom::Local;
        let mut it = value.iter().filter(|c| !is_ws(c));
        while let Some(cv) = it.next() {
            match cv {
                Cv::Tok(t) if t.t == T::Ident && self.text(t).eq_ignore_ascii_case("from") => {
                    match it.next() {
                        Some(Cv::Tok(s)) if s.t == T::Str => {
                            let spec = lexer::string_value(self.text(s));
                            let record = self.sheet.records.len() as u32;
                            self.sheet.records.push(CssRecord {
                                kind: RecKind::Composes,
                                spec,
                                pos: s.start,
                            });
                            from = ComposeFrom::Record(record);
                        }
                        Some(Cv::Tok(g))
                            if g.t == T::Ident && self.text(g).eq_ignore_ascii_case("global") =>
                        {
                            from = ComposeFrom::Global;
                        }
                        _ => self.warn(pos, "Unexpected \"composes\" syntax"),
                    }
                    break;
                }
                Cv::Tok(t) if t.t == T::Ident => names.push(lexer::unescape(self.text(t))),
                Cv::Tok(t) if t.t == T::Comma => {}
                _ => {}
            }
        }
        if from == ComposeFrom::Local {
            if self.mode == NameMode::Local {
                for n in &names {
                    self.local(n);
                }
            } else {
                // `:global { composes: a }` composes global names.
                from = ComposeFrom::Global;
            }
        }
        self.sheet.composes.push(Compose {
            class,
            names,
            from,
            pos,
        });
    }
}

/// A stylesheet printed on its own (no bundling): references and names as written.
pub fn print_sheet(sheet: &Sheet, minify: bool, syntax: bool) -> String {
    let url = |r: u32| {
        sheet
            .records
            .get(r as usize)
            .map(|r| r.spec.clone())
            .unwrap_or_default()
    };
    let local = |n: u32| sheet.locals.get(n as usize).cloned().unwrap_or_default();
    let mut out = String::new();
    if sheet.charset {
        out.push_str(if minify {
            CHARSET
        } else {
            "@charset \"UTF-8\";\n"
        });
    }
    print::Printer {
        sheet,
        minify,
        syntax,
        url: &url,
        local: &local,
    }
    .nodes(&sheet.rules, 0, &mut out);
    if minify && !out.is_empty() {
        out.push('\n');
    }
    out
}

/// What a stylesheet that started with `@charset` starts with in the output.
pub const CHARSET: &str = "@charset \"UTF-8\";";

/// The names a stylesheet's layers declare, outermost first (`@layer a { @layer b; }` is
/// `a`, `a.b`), stopping at anonymous layers. Used for the `@layer` statement that keeps
/// layer order when a duplicate import is dropped.
pub fn declared_layers(sheet: &Sheet, nodes: &[Node], prefix: &str, out: &mut Vec<String>) {
    for n in nodes {
        if let Node::At {
            name,
            prelude,
            body,
            ..
        } = n
            && name == "layer"
        {
            let names = layer_names(sheet, prelude);
            match body {
                None => {
                    for l in names {
                        push_unique(out, join_layer(prefix, &l));
                    }
                }
                Some(b) => {
                    if let [one] = names.as_slice() {
                        let full = join_layer(prefix, one);
                        push_unique(out, full.clone());
                        declared_layers(sheet, b, &full, out);
                    }
                }
            }
        }
    }
}

fn push_unique(out: &mut Vec<String>, s: String) {
    if !out.contains(&s) {
        out.push(s);
    }
}

fn join_layer(prefix: &str, l: &str) -> String {
    if prefix.is_empty() {
        l.to_owned()
    } else {
        format!("{prefix}.{l}")
    }
}

/// `a, b.c` as `["a", "b.c"]` (empty for an anonymous layer).
pub fn layer_names(sheet: &Sheet, prelude: &[Cv]) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for cv in prelude {
        match cv {
            Cv::Tok(t) if t.t == T::Comma => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            Cv::Tok(t) if t.t == T::Ws => {}
            Cv::Tok(t) => cur.push_str(sheet.text(t)),
            _ => {}
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str, mode: NameMode) -> Sheet {
        parse(Arc::from(s), mode)
    }

    #[test]
    fn rules_and_declarations() {
        let s = p(
            "a { color: red !important; b:hover { x: y } } @media screen { c {} }",
            NameMode::Global,
        );
        assert_eq!(s.rules.len(), 2);
        let Node::Rule { body, .. } = &s.rules[0] else {
            panic!()
        };
        assert!(matches!(
            &body[0],
            Node::Decl {
                important: true,
                ..
            }
        ));
        assert!(matches!(&body[1], Node::Rule { .. }));
        assert!(matches!(&s.rules[1], Node::At { name, body: Some(_), .. } if name == "media"));
    }

    #[test]
    fn imports_and_urls() {
        let s = p(
            "@import url(a.css) layer(x) supports(display: grid) screen; a { b: url('c.png') url(d.png) }",
            NameMode::Global,
        );
        assert_eq!(s.records.len(), 3);
        assert_eq!(s.records[0].spec, "a.css");
        assert_eq!(s.records[1].spec, "c.png");
        let c = &s.imports[0].conditions;
        assert!(c.layer.is_some() && c.supports.is_some() && !c.media.is_empty());
    }

    #[test]
    fn local_names() {
        let s = p(
            ".a .b:hover, :global(.g) .c, :global .h .i {} .y { composes: d e; } #e {} @keyframes k {} .x { animation: k 1s ease }",
            NameMode::Local,
        );
        assert_eq!(s.locals, ["a", "b", "c", "y", "d", "e", "k", "x"]);
        assert_eq!(s.globals, ["g", "h", "i"]);
        assert_eq!(s.composes.len(), 1);
        // `composes` only works in a rule for one class.
        let s = p(".a, .b { composes: c; }", NameMode::Local);
        assert!(s.composes.is_empty());
        assert!(!s.messages.is_empty());
    }

    #[test]
    fn selector_mode_holds_for_the_body_and_nested_composes() {
        // `.b :global` makes `y` a global animation; `:global { ... }` nested in `.a` (global `x`) is
        // `.a`'s own body, and its `composes` names global classes, recorded before `.a`'s.
        let s = p(
            ".a { composes: c; :global { composes: G; animation: x } } .b :global { animation: y } .c {}",
            NameMode::Local,
        );
        assert_eq!(s.locals, ["a", "c", "b"]);
        assert_eq!(s.composes.len(), 2);
        assert!(matches!(s.composes[0].from, ComposeFrom::Global));
        assert_eq!(s.composes[0].names, ["G"]);
        let Node::Rule { body, .. } = &s.rules[0] else {
            panic!()
        };
        assert!(matches!(&body[..], [Node::Decl { .. }]));
    }

    #[test]
    fn deep_nesting_is_reported() {
        let src = "(".repeat(10_000);
        let s = p(&src, NameMode::Global);
        assert!(s.messages.iter().any(|m| m.error));
        let src = "a{".repeat(5_000);
        let s = p(&src, NameMode::Global);
        assert!(s.messages.iter().any(|m| m.error));
    }
}
