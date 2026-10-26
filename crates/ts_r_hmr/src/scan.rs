//! A small JavaScript scanner that finds module imports and `import.meta.hot` usage.
//!
//! It is not a parser: it tokenizes just enough (comments, strings, template literals,
//! regular expressions, braces) to find `import`/`export ... from` statements, `import("x")`
//! calls, and the `import.meta.hot.accept(...)` / `decline()` / `import.meta.env` patterns that
//! the dev server needs. It never fails: malformed input yields whatever it could find.
//!
//! The real TS transform will produce its import list from the AST; this scanner backs the
//! passthrough JS transform and the server's hot-API detection on transformed output.

/// How a module refers to another module.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ImportKind {
    /// `import x from "s"`, `import "s"`.
    Static,
    /// `export * from "s"`, `export { a } from "s"`.
    ReExport,
    /// `import("s")` with a string literal argument.
    Dynamic,
}

impl ImportKind {
    pub(crate) fn to_u8(self) -> u8 {
        match self {
            ImportKind::Static => 0,
            ImportKind::ReExport => 1,
            ImportKind::Dynamic => 2,
        }
    }

    pub(crate) fn from_u8(v: u8) -> Option<ImportKind> {
        Some(match v {
            0 => ImportKind::Static,
            1 => ImportKind::ReExport,
            2 => ImportKind::Dynamic,
            _ => return None,
        })
    }
}

/// One import specifier found in JS code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportRecord {
    /// The specifier as written (raw text between the quotes).
    pub specifier: String,
    /// Byte offset of the first character inside the quotes.
    pub start: u32,
    /// Byte offset of the closing quote (exclusive end of the specifier text).
    pub end: u32,
    pub kind: ImportKind,
}

/// What a module does with `import.meta.hot` and `import.meta.env`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HotInfo {
    /// The module mentions `import.meta.hot` at all (needs a hot context injected).
    pub uses_hot: bool,
    /// `import.meta.hot.accept()` / `accept(cb)` / `acceptExports(...)`.
    pub self_accepting: bool,
    /// `import.meta.hot.decline()`.
    pub declined: bool,
    /// `import.meta.hot.acceptExports(...)`: the export names passed (a string or an array
    /// of strings), merged over every call, sorted. `None` when it is never called.
    pub accepted_exports: Option<Vec<String>>,
    /// String literals passed to `import.meta.hot.accept("dep", ...)` or `accept([...])`.
    /// Spans point inside the quotes so the server can rewrite them to URLs.
    pub accepted: Vec<ImportRecord>,
    /// The module mentions `import.meta.env`.
    pub uses_env: bool,
    /// `process.env.NODE_ENV` reads: `(start, end)` of the whole member chain.
    pub node_env: Vec<(u32, u32)>,
    /// `new URL("<literal>", import.meta.url)`: the literal's contents (between the quotes).
    pub asset_urls: Vec<ImportRecord>,
    /// `import.meta.glob(...)` calls.
    pub globs: Vec<GlobCall>,
    /// `import(expr)` whose argument is neither a string nor a template literal: the
    /// argument's span. The server wraps it to add `?import` at runtime, like Vite.
    pub dynamic_exprs: Vec<(u32, u32)>,
}

/// One `import.meta.glob(...)` call: `start..end` is the whole call expression,
/// `args_start..args_end` the text between its parentheses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GlobCall {
    pub start: u32,
    pub end: u32,
    pub args_start: u32,
    pub args_end: u32,
}

/// Everything the scanner found in one module.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModuleScan {
    pub imports: Vec<ImportRecord>,
    pub hot: HotInfo,
    /// Saw a static `import` declaration or an `export` keyword (i.e. this is ESM, not CJS).
    pub has_module_syntax: bool,
    /// The names the module exports (`default` for a default export), in source order.
    /// `export * from` adds none; `export * as ns from` adds `ns`.
    pub exports: Vec<String>,
    /// What each import binds from its module, keyed by the specifier's start offset (the
    /// [`ImportRecord::start`] of the same import): `*` for a namespace or dynamic import,
    /// `default`, or the imported names (not the local ones). A side-effect import or a
    /// re-export (`export ... from`) binds nothing.
    pub bindings: Vec<(u32, Vec<String>)>,
    /// `` import(`head${...}`) ``: the static head of a template literal with substitutions
    /// (the text between the backtick and the first `${`), unless a `@vite-ignore` comment
    /// precedes it. The server resolves an aliased head (`@/`, `#sub/`).
    pub template_heads: Vec<(u32, u32)>,
}

/// Whether a dynamic import of the template text `raw` asks for a file the server only
/// serves as a module with `?import` (Vite's `isExplicitImportRequired`): one whose path
/// (query and hash removed) has an extension that is neither a script's nor a stylesheet's.
fn needs_import_query(raw: &[u8]) -> bool {
    let Ok(raw) = std::str::from_utf8(raw) else {
        return false;
    };
    let path = raw.split(['?', '#']).next().unwrap_or(raw);
    let name = path.rsplit('/').next().unwrap_or(path);
    let Some((_, ext)) = name.rsplit_once('.').filter(|(stem, _)| !stem.is_empty()) else {
        return false;
    };
    const STYLES: &[&str] = &[
        "css", "less", "sass", "scss", "styl", "stylus", "pcss", "postcss", "sss",
    ];
    !crate::transform::is_script_extension(ext) && !STYLES.contains(&ext)
}

/// Scans `code` for imports and hot-API usage.
pub fn scan(code: &str) -> ModuleScan {
    let mut s = Scanner {
        src: code.as_bytes(),
        pos: 0,
        last: Last::Start,
        braces: Vec::new(),
        out: ModuleScan::default(),
    };
    s.run();
    s.out
}

/// Scans only for hot-API usage (and the rest of [`HotInfo`]); cheap when the code never
/// mentions `import.meta` or `process.env.NODE_ENV`.
pub fn scan_hot(code: &str) -> HotInfo {
    if !code.contains("import.meta")
        && !code.contains("import .meta")
        && !code.contains("process.env.NODE_ENV")
        && !code.contains("import(")
        && !code.contains("import (")
    {
        return HotInfo::default();
    }
    scan(code).hot
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Last {
    /// Start of input: a `/` starts a regex.
    Start,
    /// Punctuator or keyword after which an expression may start (regex allowed).
    Operator,
    /// End of a value (identifier, literal, `)`, `]`): a `/` is division.
    Value,
    /// A `.` or `?.`: the next identifier is a property name.
    Dot,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Brace {
    Block,
    TemplateExpr,
}

struct Scanner<'a> {
    src: &'a [u8],
    pos: usize,
    last: Last,
    braces: Vec<Brace>,
    out: ModuleScan,
}

const REGEX_KEYWORDS: &[&[u8]] = &[
    b"return",
    b"typeof",
    b"instanceof",
    b"in",
    b"of",
    b"new",
    b"delete",
    b"void",
    b"throw",
    b"case",
    b"do",
    b"else",
    b"yield",
    b"await",
];

fn is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c == b'$' || c >= 0x80
}

fn is_ident_char(c: u8) -> bool {
    is_ident_start(c) || c.is_ascii_digit()
}

impl Scanner<'_> {
    fn peek(&self, at: usize) -> u8 {
        self.src.get(at).copied().unwrap_or(0)
    }

    fn run(&mut self) {
        // Hashbang.
        if self.src.starts_with(b"#!") {
            self.pos = self.line_end(0);
        }
        while self.pos < self.src.len() {
            self.pos = self.skip_trivia(self.pos);
            let Some(&c) = self.src.get(self.pos) else {
                break;
            };
            match c {
                b'"' | b'\'' => {
                    self.pos = self.string_end(self.pos);
                    self.last = Last::Value;
                }
                b'`' => {
                    self.pos += 1;
                    self.template();
                }
                b'{' => {
                    self.braces.push(Brace::Block);
                    self.pos += 1;
                    self.last = Last::Operator;
                }
                b'}' => {
                    self.pos += 1;
                    if self.braces.pop() == Some(Brace::TemplateExpr) {
                        self.template();
                    } else {
                        self.last = Last::Operator;
                    }
                }
                b'/' => {
                    if matches!(self.last, Last::Start | Last::Operator) {
                        self.regex();
                    } else {
                        self.pos += 1;
                        self.last = Last::Operator;
                    }
                }
                b')' | b']' => {
                    self.pos += 1;
                    self.last = Last::Value;
                }
                b'.' => {
                    if self.peek(self.pos + 1).is_ascii_digit() {
                        self.pos = self.number_end(self.pos);
                        self.last = Last::Value;
                    } else if self.src[self.pos..].starts_with(b"...") {
                        self.pos += 3;
                        self.last = Last::Operator;
                    } else {
                        self.pos += 1;
                        self.last = Last::Dot;
                    }
                }
                b'0'..=b'9' => {
                    self.pos = self.number_end(self.pos);
                    self.last = Last::Value;
                }
                c if is_ident_start(c) => self.word(),
                b'#' => {
                    // Private name `#x`.
                    self.pos += 1;
                    let end = self.ident_end(self.pos);
                    self.pos = end;
                    self.last = Last::Value;
                }
                _ => {
                    self.pos += 1;
                    self.last = Last::Operator;
                }
            }
        }
    }

    fn word(&mut self) {
        let start = self.pos;
        let end = self.ident_end(start);
        let word = &self.src[start..end];
        let after_dot = self.last == Last::Dot;
        self.pos = end;
        if !after_dot {
            if word == b"import" {
                self.import(end);
                return;
            }
            if word == b"process" {
                self.process_env(start, end);
            } else if word == b"global" || word == b"globalThis" {
                // `global.process.env.NODE_ENV` and `globalThis.process…` are replaced too.
                if let Some((a, b)) = self.member(end)
                    && &self.src[a..b] == b"process"
                {
                    self.process_env(start, b);
                }
            } else if word == b"new" {
                self.new_url(end);
            }
            if word == b"export" {
                self.out.has_module_syntax = true;
                self.export(end);
                return;
            }
        }
        self.last = if !after_dot && REGEX_KEYWORDS.contains(&word) {
            Last::Operator
        } else {
            Last::Value
        };
    }

    /// After the `import` keyword at `after`.
    fn import(&mut self, after: usize) {
        let p = self.skip_trivia(after);
        match self.peek(p) {
            b'(' => {
                let q = self.skip_trivia(p + 1);
                if matches!(self.peek(q), b'"' | b'\'') {
                    let end = self.string_end(q);
                    let r = self.skip_trivia(end);
                    if matches!(self.peek(r), b')' | b',') {
                        self.record(q, end, ImportKind::Dynamic);
                        self.bind(q, vec!["*".to_owned()]);
                    }
                } else if !matches!(self.peek(q), b'`' | b')' | 0) {
                    if let Some(end) = self.argument_end(q) {
                        self.out.hot.dynamic_exprs.push((q as u32, end as u32));
                    }
                } else if self.peek(q) == b'`' {
                    let ignored = self.src[p + 1..q]
                        .windows(b"@vite-ignore".len())
                        .any(|w| w == b"@vite-ignore");
                    if !ignored && let Some(end) = self.template_head_end(q + 1) {
                        self.out.template_heads.push((q as u32 + 1, end as u32));
                    }
                    if let Some(end) = self.template_end(q)
                        && needs_import_query(&self.src[q + 1..end - 1])
                    {
                        self.out.hot.dynamic_exprs.push((q as u32, end as u32));
                    }
                }
                // Let the main loop lex the arguments.
                self.pos = p;
                self.last = Last::Value;
            }
            b'.' => {
                self.import_meta(after - 6, p);
            }
            b'"' | b'\'' => {
                let end = self.string_end(p);
                self.record(p, end, ImportKind::Static);
                self.bind(p, Vec::new());
                self.out.has_module_syntax = true;
                self.pos = end;
                self.last = Last::Value;
            }
            c if is_ident_start(c) || c == b'{' || c == b'*' => {
                // `import x, { a as b } from "s"`, `import * as ns from "s"`.
                let mut q = p;
                loop {
                    q = self.skip_trivia(q);
                    match self.peek(q) {
                        b'{' => match self.matching_brace(q) {
                            Some(close) => q = close + 1,
                            None => return self.keep_lexing(after),
                        },
                        b'"' | b'\'' => {
                            let end = self.string_end(q);
                            self.record(q, end, ImportKind::Static);
                            let names = self.import_bindings(p, q);
                            self.bind(q, names);
                            self.out.has_module_syntax = true;
                            self.pos = end;
                            self.last = Last::Value;
                            return;
                        }
                        b',' | b'*' => q += 1,
                        c if is_ident_start(c) => q = self.ident_end(q),
                        _ => return self.keep_lexing(after),
                    }
                }
            }
            _ => self.keep_lexing(after),
        }
    }

    /// Records what the import whose specifier starts at quote `quote` binds.
    fn bind(&mut self, quote: usize, names: Vec<String>) {
        self.out.bindings.push(((quote + 1) as u32, names));
    }

    /// The bindings of the clause `from..to` of `import <clause> from "x"`: `*` for
    /// `* as ns`, `default` for a default import, the imported names of `{ a, b as c }`.
    fn import_bindings(&self, from: usize, to: usize) -> Vec<String> {
        let mut names = Vec::new();
        let mut p = from;
        while p < to {
            p = self.skip_trivia(p);
            match self.peek(p) {
                _ if p >= to => break,
                b'*' => {
                    names.push("*".to_owned());
                    // `* as ns`: the local name is not a default import.
                    let a = self.skip_trivia(p + 1);
                    p = if self.word_at(a, b"as") {
                        let n = self.skip_trivia(a + 2);
                        self.ident_end(n)
                    } else {
                        p + 1
                    };
                }
                b'{' => {
                    let close = self.matching_brace(p).unwrap_or(to).min(to);
                    self.named_bindings(p + 1, close, &mut names);
                    p = close + 1;
                }
                c if is_ident_start(c) => {
                    let end = self.ident_end(p);
                    if !matches!(&self.src[p..end], b"from" | b"type") {
                        names.push("default".to_owned());
                    }
                    p = end;
                }
                _ => p += 1,
            }
        }
        names.sort();
        names.dedup();
        names
    }

    /// The imported names of the specifiers between `{` and `}` (`a`, `b as c`, `"x" as y`,
    /// `type T` skipped).
    fn named_bindings(&self, from: usize, close: usize, names: &mut Vec<String>) {
        let mut q = from;
        while q < close {
            // One specifier: up to the next comma.
            let start = self.skip_trivia(q);
            let mut end = start;
            while end < close && self.peek(end) != b',' {
                end = match self.peek(end) {
                    b'"' | b'\'' => self.string_end(end),
                    _ => end + 1,
                };
            }
            q = end + 1;
            let mut first = start;
            let mut first_end = match self.peek(first) {
                b'"' | b'\'' => self.string_end(first),
                c if is_ident_start(c) => self.ident_end(first),
                _ => continue,
            };
            let next = self.skip_trivia(first_end);
            if &self.src[first..first_end] == b"type" && next < end && !self.word_at(next, b"as") {
                continue; // `type T`: erased
            }
            if matches!(self.peek(first), b'"' | b'\'') {
                if first_end < first + 2 {
                    continue;
                }
                first += 1;
                first_end -= 1;
            }
            if let Ok(n) = std::str::from_utf8(&self.src[first..first_end]) {
                names.push(n.to_owned());
            }
        }
    }

    fn keep_lexing(&mut self, at: usize) {
        self.pos = at;
        self.last = Last::Value;
    }

    /// `process.env.NODE_ENV` (read, not assigned) starting at the `process` word.
    fn process_env(&mut self, start: usize, end: usize) {
        let Some((a, b)) = self.member(end) else {
            return;
        };
        if &self.src[a..b] != b"env" {
            return;
        }
        let Some((c, d)) = self.member(b) else {
            return;
        };
        if &self.src[c..d] != b"NODE_ENV" || is_ident_char(self.peek(d)) {
            return;
        }
        let after = self.skip_trivia(d);
        let assigned = self.peek(after) == b'=' && self.peek(after + 1) != b'=';
        if !assigned {
            self.out.hot.node_env.push((start as u32, d as u32));
        }
    }

    /// After `new`: `URL("<literal>", import.meta.url)` with a string or a template literal
    /// without substitutions.
    fn new_url(&mut self, after: usize) {
        let p = self.skip_trivia(after);
        if !self.word_at(p, b"URL") {
            return;
        }
        let q = self.skip_trivia(p + 3);
        if self.peek(q) != b'(' {
            return;
        }
        let lit = self.skip_trivia(q + 1);
        let end = match self.peek(lit) {
            b'"' | b'\'' => self.string_end(lit),
            b'`' => {
                let mut i = lit + 1;
                loop {
                    match self.peek(i) {
                        0 => return,
                        b'\\' => i += 2,
                        b'$' if self.peek(i + 1) == b'{' => return,
                        b'`' => break i + 1,
                        _ => i += 1,
                    }
                }
            }
            _ => return,
        };
        let open = self.src[lit];
        if end < lit + 2 || self.src.get(end - 1) != Some(&open) {
            return;
        }
        let comma = self.skip_trivia(end);
        if self.peek(comma) != b',' {
            return;
        }
        let mut m = self.skip_trivia(comma + 1);
        for (i, part) in [&b"import"[..], b"meta", b"url"].iter().enumerate() {
            if i > 0 {
                if self.peek(m) != b'.' {
                    return;
                }
                m = self.skip_trivia(m + 1);
            }
            if !self.word_at(m, part) {
                return;
            }
            m = self.skip_trivia(m + part.len());
        }
        if !matches!(self.peek(m), b')' | b',') {
            return;
        }
        if let Ok(text) = std::str::from_utf8(&self.src[lit + 1..end - 1]) {
            self.out.hot.asset_urls.push(ImportRecord {
                specifier: text.to_owned(),
                start: (lit + 1) as u32,
                end: (end - 1) as u32,
                kind: ImportKind::Dynamic,
            });
        }
    }

    /// `import.meta.glob` whose name ends at `name_end`: records the call up to its closing
    /// parenthesis (strings, templates and comments skipped; brackets counted).
    fn glob_call(&mut self, import_start: usize, name_end: usize) {
        let open = self.skip_trivia(name_end);
        if self.peek(open) != b'(' {
            return;
        }
        let mut depth = 0usize;
        let mut p = open;
        while p < self.src.len() {
            p = self.skip_trivia(p);
            match self.peek(p) {
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        self.out.hot.globs.push(GlobCall {
                            start: import_start as u32,
                            end: (p + 1) as u32,
                            args_start: (open + 1) as u32,
                            args_end: p as u32,
                        });
                        return;
                    }
                }
                b'"' | b'\'' => {
                    p = self.string_end(p);
                    continue;
                }
                b'`' => {
                    p += 1;
                    while p < self.src.len() && self.src[p] != b'`' {
                        p += if self.src[p] == b'\\' { 2 } else { 1 };
                    }
                }
                0 => return,
                _ => {}
            }
            p += 1;
        }
    }

    /// At the `.` after `import` (which starts at `import_start`).
    fn import_meta(&mut self, import_start: usize, dot: usize) {
        let p = self.skip_trivia(dot + 1);
        let end = self.ident_end(p);
        if &self.src[p..end] != b"meta" {
            return self.keep_lexing(dot);
        }
        self.pos = end;
        self.last = Last::Value;
        let Some((name_start, name_end)) = self.member(end) else {
            return;
        };
        match &self.src[name_start..name_end] {
            b"env" => self.out.hot.uses_env = true,
            b"glob" => self.glob_call(import_start, name_end),
            b"hot" => {
                self.out.hot.uses_hot = true;
                let Some((m_start, m_end)) = self.member(name_end) else {
                    return;
                };
                let method = &self.src[m_start..m_end];
                let paren = self.skip_trivia(m_end);
                if self.peek(paren) != b'(' {
                    return;
                }
                match method {
                    b"accept" => self.accept_args(paren),
                    b"acceptExports" => self.accept_exports_args(paren),
                    b"decline" => self.out.hot.declined = true,
                    _ => {}
                }
            }
            _ => {}
        }
    }

    /// Parses `.name` or `?.name` at `at`, returning the name's span.
    fn member(&self, at: usize) -> Option<(usize, usize)> {
        let mut p = self.skip_trivia(at);
        if self.peek(p) == b'?' && self.peek(p + 1) == b'.' {
            p += 2;
        } else if self.peek(p) == b'.' {
            p += 1;
        } else {
            return None;
        }
        let p = self.skip_trivia(p);
        let end = self.ident_end(p);
        (end > p).then_some((p, end))
    }

    /// At the `(` of `import.meta.hot.accept(`.
    fn accept_args(&mut self, paren: usize) {
        let q = self.skip_trivia(paren + 1);
        match self.peek(q) {
            b'"' | b'\'' => {
                let end = self.string_end(q);
                if let Some(rec) = self.make_record(q, end, ImportKind::Static) {
                    self.out.hot.accepted.push(rec);
                }
            }
            b'[' => {
                let mut p = q + 1;
                loop {
                    p = self.skip_trivia(p);
                    match self.peek(p) {
                        b'"' | b'\'' => {
                            let end = self.string_end(p);
                            if let Some(rec) = self.make_record(p, end, ImportKind::Static) {
                                self.out.hot.accepted.push(rec);
                            }
                            p = end;
                        }
                        b',' => p += 1,
                        _ => break,
                    }
                }
            }
            _ => self.out.hot.self_accepting = true,
        }
    }

    /// At the `(` of `import.meta.hot.acceptExports(`: the names in its first argument, a
    /// string or an array of strings.
    fn accept_exports_args(&mut self, paren: usize) {
        let mut names = Vec::new();
        let q = self.skip_trivia(paren + 1);
        let mut push = |s: &Self, quote: usize, end: usize| {
            if let Some(rec) = s.make_record(quote, end, ImportKind::Static) {
                names.push(rec.specifier);
            }
        };
        match self.peek(q) {
            b'"' | b'\'' => push(self, q, self.string_end(q)),
            b'[' => {
                let mut p = q + 1;
                loop {
                    p = self.skip_trivia(p);
                    match self.peek(p) {
                        b'"' | b'\'' => {
                            let end = self.string_end(p);
                            push(self, p, end);
                            p = end;
                        }
                        b',' => p += 1,
                        _ => break,
                    }
                }
            }
            _ => {}
        }
        let all = self.out.hot.accepted_exports.get_or_insert_with(Vec::new);
        all.extend(names);
        all.sort();
        all.dedup();
    }

    /// After the `export` keyword at `after`.
    fn export(&mut self, after: usize) {
        let p = self.skip_trivia(after);
        let from_at = match self.peek(p) {
            b'*' => {
                let mut q = self.skip_trivia(p + 1);
                if self.word_at(q, b"as") {
                    q = self.skip_trivia(q + 2);
                    let start = q;
                    q = match self.peek(q) {
                        b'"' | b'\'' => self.string_end(q),
                        _ => self.ident_end(q),
                    };
                    self.export_name(start, q);
                }
                q
            }
            b'{' => match self.matching_brace(p) {
                Some(close) => {
                    self.exported_specifiers(p + 1, close);
                    close + 1
                }
                None => return self.after_export(after),
            },
            _ => {
                self.declared_exports(p);
                return self.after_export(after);
            }
        };
        let f = self.skip_trivia(from_at);
        if self.word_at(f, b"from") {
            let s = self.skip_trivia(f + 4);
            if matches!(self.peek(s), b'"' | b'\'') {
                let end = self.string_end(s);
                self.record(s, end, ImportKind::ReExport);
                self.bind(s, Vec::new());
                self.pos = end;
                self.last = Last::Value;
                return;
            }
        }
        // `export { a, b };` — nothing imported; the names hold no strings worth lexing.
        if self.peek(p) == b'{' {
            self.pos = from_at;
            self.last = Last::Operator;
        } else {
            self.after_export(after);
        }
    }

    /// Records the exported name spelled at `start..end` (an identifier or a string).
    fn export_name(&mut self, start: usize, end: usize) {
        let (mut a, mut b) = (start, end);
        if matches!(self.peek(a), b'"' | b'\'') {
            if b < a + 2 {
                return;
            }
            a += 1;
            b -= 1;
        }
        if b > a
            && let Ok(n) = std::str::from_utf8(&self.src[a..b])
        {
            self.out.exports.push(n.to_owned());
        }
    }

    /// `export { a, b as c, d as default }` between the braces: the exported names.
    fn exported_specifiers(&mut self, from: usize, close: usize) {
        let mut q = from;
        while q < close {
            let start = self.skip_trivia(q);
            let mut end = start;
            while end < close && self.peek(end) != b',' {
                end = match self.peek(end) {
                    b'"' | b'\'' => self.string_end(end),
                    _ => end + 1,
                };
            }
            q = end + 1;
            // The last name of the specifier is the exported one (`a`, `b as c`).
            let mut name = None;
            let mut r = start;
            while r < end {
                r = self.skip_trivia(r);
                if r >= end {
                    break;
                }
                let e = match self.peek(r) {
                    b'"' | b'\'' => self.string_end(r),
                    c if is_ident_start(c) => self.ident_end(r),
                    _ => r + 1,
                };
                if &self.src[r..e] != b"as" {
                    name = Some((r, e));
                }
                r = e;
            }
            if let Some((a, b)) = name {
                let first = self.skip_trivia(start);
                let typed = self.word_at(first, b"type")
                    && first != a
                    && !self.word_at(self.skip_trivia(first + 4), b"as");
                if !typed {
                    self.export_name(a, b);
                }
            }
        }
    }

    /// `export default ...`, `export function f`, `export class C`, `export const a = 1, b`
    /// at `p`: the names declared.
    fn declared_exports(&mut self, p: usize) {
        if self.word_at(p, b"default") {
            self.out.exports.push("default".to_owned());
            return;
        }
        let mut q = p;
        if self.word_at(q, b"async") {
            q = self.skip_trivia(q + 5);
        }
        for kw in [&b"function"[..], b"class"] {
            if self.word_at(q, kw) {
                let mut n = self.skip_trivia(q + kw.len());
                if self.peek(n) == b'*' {
                    n = self.skip_trivia(n + 1);
                }
                let e = self.ident_end(n);
                self.export_name(n, e);
                return;
            }
        }
        for kw in [&b"const"[..], b"let", b"var"] {
            if self.word_at(q, kw) {
                return self.declarators(q + kw.len());
            }
        }
    }

    /// The names bound by `a = 1, b, c = f(x, y)` from `p` (simple identifiers only: a
    /// destructuring pattern is skipped). Stops at `;` or a line break outside brackets
    /// (unless the line ends with `,` or `=`).
    fn declarators(&mut self, mut p: usize) {
        let mut expect_name = true;
        let mut depth = 0usize;
        let mut prev = b' ';
        let mut last = Last::Start;
        while p < self.src.len() {
            let before = p;
            p = self.skip_trivia(p);
            let newline = self.src[before..p]
                .iter()
                .any(|&c| c == b'\n' || c == b'\r');
            if depth == 0 && newline && !expect_name && !matches!(prev, b',' | b'=') {
                return;
            }
            let c = self.peek(p);
            if expect_name && depth == 0 {
                expect_name = false;
                if is_ident_start(c) {
                    let e = self.ident_end(p);
                    self.export_name(p, e);
                    prev = b'a';
                    last = Last::Value;
                    p = e;
                    continue;
                }
            }
            match c {
                0 => return,
                b';' if depth == 0 => return,
                b',' if depth == 0 => expect_name = true,
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => {
                    if depth == 0 {
                        return;
                    }
                    depth -= 1;
                }
                b'"' | b'\'' => {
                    p = self.string_end(p);
                    prev = b'"';
                    last = Last::Value;
                    continue;
                }
                b'/' if matches!(last, Last::Start | Last::Operator) => {
                    let end = self.regex_end(p);
                    last = if end > p + 1 {
                        Last::Value
                    } else {
                        Last::Operator
                    };
                    p = end;
                    prev = b'/';
                    continue;
                }
                c if is_ident_start(c) => {
                    let end = self.ident_end(p);
                    last = if last != Last::Dot && REGEX_KEYWORDS.contains(&&self.src[p..end]) {
                        Last::Operator
                    } else {
                        Last::Value
                    };
                    p = end;
                    prev = b'a';
                    continue;
                }
                b'0'..=b'9' => {
                    p = self.number_end(p);
                    prev = b'0';
                    last = Last::Value;
                    continue;
                }
                b'`' => {
                    p += 1;
                    while p < self.src.len() && self.src[p] != b'`' {
                        p += if self.src[p] == b'\\' { 2 } else { 1 };
                    }
                }
                _ => {}
            }
            prev = c;
            last = match c {
                b')' | b']' | b'}' | b'`' => Last::Value,
                b'.' => Last::Dot,
                _ => Last::Operator,
            };
            p += 1;
        }
    }

    fn after_export(&mut self, at: usize) {
        self.pos = at;
        self.last = Last::Operator;
    }

    fn word_at(&self, at: usize, word: &[u8]) -> bool {
        self.src[at.min(self.src.len())..].starts_with(word)
            && !is_ident_char(self.peek(at + word.len()))
    }

    fn record(&mut self, quote: usize, end: usize, kind: ImportKind) {
        if let Some(rec) = self.make_record(quote, end, kind) {
            self.out.imports.push(rec);
        }
    }

    /// `quote` is the opening quote, `end` is just past the closing quote.
    fn make_record(&self, quote: usize, end: usize, kind: ImportKind) -> Option<ImportRecord> {
        let open = self.src[quote];
        if end < quote + 2 || self.src.get(end - 1) != Some(&open) {
            return None; // unterminated
        }
        let start = quote + 1;
        let stop = end - 1;
        let text = std::str::from_utf8(&self.src[start..stop]).ok()?;
        Some(ImportRecord {
            specifier: text.to_owned(),
            start: start as u32,
            end: stop as u32,
            kind,
        })
    }

    /// Index of the `}` matching the `{` at `open`, skipping strings and comments.
    fn matching_brace(&self, open: usize) -> Option<usize> {
        let mut depth = 0usize;
        let mut p = open;
        while p < self.src.len() {
            p = self.skip_trivia(p);
            match self.peek(p) {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(p);
                    }
                }
                b'"' | b'\'' => {
                    p = self.string_end(p);
                    continue;
                }
                // Import/export clauses never contain these; bail out rather than mis-lex.
                b'(' | b';' | b'`' | 0 => return None,
                _ => {}
            }
            p += 1;
        }
        None
    }

    /// Skips whitespace and comments from `p`.
    fn skip_trivia(&self, mut p: usize) -> usize {
        loop {
            match self.peek(p) {
                b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c => p += 1,
                b'/' if self.peek(p + 1) == b'/' => p = self.line_end(p),
                b'/' if self.peek(p + 1) == b'*' => {
                    p = match memfind(&self.src[p + 2..], b"*/") {
                        Some(i) => p + 2 + i + 2,
                        None => self.src.len(),
                    };
                }
                // Non-ASCII whitespace (NBSP, LS, PS, BOM): skip the UTF-8 sequence.
                0xc2 if self.peek(p + 1) == 0xa0 => p += 2,
                0xe2 if self.peek(p + 1) == 0x80 && matches!(self.peek(p + 2), 0xa8 | 0xa9) => {
                    p += 3
                }
                0xef if self.peek(p + 1) == 0xbb && self.peek(p + 2) == 0xbf => p += 3,
                _ => return p,
            }
        }
    }

    fn line_end(&self, p: usize) -> usize {
        match self.src[p..].iter().position(|&c| c == b'\n' || c == b'\r') {
            Some(i) => p + i,
            None => self.src.len(),
        }
    }

    /// `p` at an opening quote; returns the index after the closing quote (or the line end
    /// for an unterminated string).
    fn string_end(&self, p: usize) -> usize {
        let quote = self.src[p];
        let mut i = p + 1;
        while i < self.src.len() {
            match self.src[i] {
                b'\\' => i += 2,
                b'\n' | b'\r' => return i,
                c if c == quote => return i + 1,
                _ => i += 1,
            }
        }
        self.src.len()
    }

    /// The end of the call argument starting at `from` (before a top-level `,` or `)`,
    /// trailing whitespace trimmed); strings, templates and comments are skipped.
    fn argument_end(&self, from: usize) -> Option<usize> {
        let mut depth = 0usize;
        let mut p = from;
        let mut last = from;
        while p < self.src.len() {
            let q = self.skip_trivia(p);
            match self.peek(q) {
                0 => return None,
                b',' | b')' if depth == 0 => return (last > from).then_some(last),
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => depth = depth.saturating_sub(1),
                b'"' | b'\'' => {
                    p = self.string_end(q);
                    last = p;
                    continue;
                }
                b'`' => {
                    let mut i = q + 1;
                    while i < self.src.len() && self.src[i] != b'`' {
                        i += if self.src[i] == b'\\' { 2 } else { 1 };
                    }
                    p = (i + 1).min(self.src.len());
                    last = p;
                    continue;
                }
                _ => {}
            }
            p = q + 1;
            last = p;
        }
        None
    }

    /// The end (after the closing backtick) of the template literal at `from`, with
    /// substitutions skipped by counting braces.
    fn template_end(&self, from: usize) -> Option<usize> {
        let mut i = from + 1;
        let mut depth = 0usize;
        while i < self.src.len() {
            match self.src[i] {
                b'\\' => i += 1,
                b'`' if depth == 0 => return Some(i + 1),
                b'$' if depth == 0 && self.peek(i + 1) == b'{' => {
                    depth = 1;
                    i += 1;
                }
                b'{' if depth > 0 => depth += 1,
                b'}' if depth > 0 => depth -= 1,
                _ => {}
            }
            i += 1;
        }
        None
    }

    /// Where the first `${` of the template text starting at `from` is, if it has one.
    fn template_head_end(&self, from: usize) -> Option<usize> {
        let mut i = from;
        while i < self.src.len() {
            match self.src[i] {
                b'\\' => i += 2,
                b'`' => return None,
                b'$' if self.peek(i + 1) == b'{' => return Some(i),
                _ => i += 1,
            }
        }
        None
    }

    /// Scans template literal text (after a backtick or a closing `}` of a substitution).
    fn template(&mut self) {
        while self.pos < self.src.len() {
            match self.src[self.pos] {
                b'\\' => self.pos += 2,
                b'`' => {
                    self.pos += 1;
                    self.last = Last::Value;
                    return;
                }
                b'$' if self.peek(self.pos + 1) == b'{' => {
                    self.pos += 2;
                    self.braces.push(Brace::TemplateExpr);
                    self.last = Last::Operator;
                    return;
                }
                _ => self.pos += 1,
            }
        }
        self.pos = self.src.len();
    }

    fn regex(&mut self) {
        let end = self.regex_end(self.pos);
        self.last = if end > self.pos + 1 {
            Last::Value
        } else {
            Last::Operator
        };
        self.pos = end;
    }

    fn regex_end(&self, start: usize) -> usize {
        let mut i = start + 1;
        let mut in_class = false;
        while i < self.src.len() {
            match self.src[i] {
                b'\\' => i += 2,
                b'\n' | b'\r' => break,
                b'[' => {
                    in_class = true;
                    i += 1;
                }
                b']' => {
                    in_class = false;
                    i += 1;
                }
                b'/' if !in_class => {
                    return self.ident_end(i + 1);
                }
                _ => i += 1,
            }
        }
        // Not a regex after all: treat `/` as an operator.
        start + 1
    }

    fn ident_end(&self, mut p: usize) -> usize {
        while p < self.src.len() && is_ident_char(self.src[p]) {
            p += 1;
        }
        p
    }

    fn number_end(&self, mut p: usize) -> usize {
        while p < self.src.len() {
            let c = self.src[p];
            let exponent_sign = (c == b'+' || c == b'-') && matches!(self.src[p - 1], b'e' | b'E');
            if is_ident_char(c) || c == b'.' || exponent_sign {
                p += 1;
            } else {
                break;
            }
        }
        p
    }
}

fn memfind(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn specs(code: &str) -> Vec<(String, ImportKind)> {
        scan(code)
            .imports
            .into_iter()
            .map(|r| {
                assert_eq!(&code[r.start as usize..r.end as usize], r.specifier);
                (r.specifier, r.kind)
            })
            .collect()
    }

    fn s(v: &str, k: ImportKind) -> (String, ImportKind) {
        (v.to_owned(), k)
    }

    use ImportKind::*;

    #[test]
    fn static_imports() {
        let code = r#"
import a from "./a.js";
import { b, c as d } from './b.js'
import * as ns from "ns";
import e, { f } from "e";
import "side-effect";
import {} from "empty";
import { "string name" as g } from "./g.js";
import defer * as lazy from "./lazy.js";
"#;
        assert_eq!(
            specs(code),
            vec![
                s("./a.js", Static),
                s("./b.js", Static),
                s("ns", Static),
                s("e", Static),
                s("side-effect", Static),
                s("empty", Static),
                s("./g.js", Static),
                s("./lazy.js", Static),
            ]
        );
        assert!(scan(code).has_module_syntax);
    }

    #[test]
    fn reexports_and_plain_exports() {
        let code = r#"
export * from "./star.js";
export * as ns from './ns.js';
export { a, b as c } from "./named.js";
export { local };
export const x = "not-a-specifier";
export default "also-not";
export function f() { return "nope" }
"#;
        assert_eq!(
            specs(code),
            vec![
                s("./star.js", ReExport),
                s("./ns.js", ReExport),
                s("./named.js", ReExport)
            ]
        );
    }

    #[test]
    fn dynamic_imports() {
        let code = r#"
const a = import("./a.js");
const b = await import('./b.js', { with: { type: "json" } });
const c = import(name);
const d = import(`./tpl-${x}.js`);
"#;
        assert_eq!(
            specs(code),
            vec![s("./a.js", Dynamic), s("./b.js", Dynamic)]
        );
        assert!(!scan(code).has_module_syntax);
    }

    #[test]
    fn ignores_strings_comments_templates_regexes() {
        let code = r#"
// import x from "comment";
/* import y from "block" */
const s = 'import z from "string"';
const t = `import w from "tpl" ${ import("./in-template.js") } import q from "q"`;
const r = /import v from "regex"/g;
const re2 = x / 2 / y; import u from "./after-division.js";
const cls = /[/]import k from "k"/;
obj.import("not-dynamic");
const o = { import: 1 };
"#;
        assert_eq!(
            specs(code),
            vec![
                s("./in-template.js", Dynamic),
                s("./after-division.js", Static)
            ]
        );
    }

    #[test]
    fn regex_after_keyword_and_brace() {
        let code = "function f() { return /\"/.test(x) }\nimport a from './a.js';\nif (x) {}\n/'/.test(y); import b from './b.js'";
        assert_eq!(specs(code), vec![s("./a.js", Static), s("./b.js", Static)]);
    }

    #[test]
    fn dynamic_import_expressions() {
        let code = "import(/*@vite-ignore*/ view ); import(a.b[c](\")\"), { with: {} }); import('./x.js'); import(`./${y}`); import(";
        let exprs: Vec<&str> = scan(code)
            .hot
            .dynamic_exprs
            .iter()
            .map(|&(s, e)| &code[s as usize..e as usize])
            .collect();
        assert_eq!(exprs, vec!["view", "a.b[c](\")\")"]);
        let code = "import(`../files/${v}.json`); import(`./${v}.js`); import(`./${v}.js?raw`); import(`./${v}.css`); import(`./${`a`}.svg`)";
        let exprs: Vec<&str> = scan(code)
            .hot
            .dynamic_exprs
            .iter()
            .map(|&(s, e)| &code[s as usize..e as usize])
            .collect();
        assert_eq!(exprs, vec!["`../files/${v}.json`", "`./${`a`}.svg`"]);
    }

    #[test]
    fn template_dynamic_import_heads() {
        let code = "import(`@/${a}.js`); import(/* @vite-ignore */ `#x/${b}`); import(`./${c}`); import(`plain`); import(`a\\${x}`)";
        let heads: Vec<&str> = scan(code)
            .template_heads
            .iter()
            .map(|&(s, e)| &code[s as usize..e as usize])
            .collect();
        assert_eq!(heads, vec!["@/", "./"]);
    }

    #[test]
    fn nested_templates() {
        let code = "const t = `a ${`b ${c}`} d ${ {x:1}.x }`; import a from './a.js';";
        assert_eq!(specs(code), vec![s("./a.js", Static)]);
    }

    #[test]
    fn hot_self_accept() {
        let hot = scan("if (import.meta.hot) { import.meta.hot.accept((m) => {}) }").hot;
        assert!(hot.uses_hot && hot.self_accepting && !hot.declined);
        assert!(hot.accepted.is_empty());
        assert!(scan("import.meta.hot.accept()").hot.self_accepting);
        assert!(scan("import.meta.hot?.accept()").hot.self_accepting);
        // `acceptExports` is self-accepting only when it covers every export (the server
        // decides, with the module's export list).
        let partial = scan("import.meta.hot.acceptExports(['a'])").hot;
        assert!(!partial.self_accepting);
        assert_eq!(partial.accepted_exports, Some(vec!["a".to_owned()]));
        let only_dispose = scan("import.meta.hot.dispose(() => {})").hot;
        assert!(only_dispose.uses_hot && !only_dispose.self_accepting);
    }

    #[test]
    fn hot_dep_accept_and_decline() {
        let code = r#"
import.meta.hot.accept('./dep.js', (m) => {});
import.meta.hot.accept(["./a.js", './b.js'], ([a, b]) => {});
import.meta.hot.decline();
const mode = import.meta.env.MODE;
"#;
        let hot = scan(code).hot;
        assert!(!hot.self_accepting);
        assert!(hot.declined && hot.uses_env);
        let deps: Vec<_> = hot.accepted.iter().map(|r| r.specifier.as_str()).collect();
        assert_eq!(deps, ["./dep.js", "./a.js", "./b.js"]);
        for r in &hot.accepted {
            assert_eq!(&code[r.start as usize..r.end as usize], r.specifier);
        }
    }

    #[test]
    fn scan_hot_fast_path() {
        assert_eq!(scan_hot("const x = 1"), HotInfo::default());
        assert!(scan_hot("import.meta.hot.accept()").self_accepting);
    }

    #[test]
    fn malformed_input_does_not_panic() {
        for code in [
            "import",
            "import {",
            "import { a from",
            "import \"unterminated",
            "export {",
            "export * as",
            "`${",
            "/[",
            "import.meta.hot.accept([",
            "import(",
            "a ? .5 : 1",
            "\u{feff}import x from 'bom.js'",
            "#!/usr/bin/env node\nimport a from 'a'",
        ] {
            let _ = scan(code);
        }
        assert_eq!(
            specs("\u{feff}import x from 'bom.js'"),
            vec![s("bom.js", Static)]
        );
        assert_eq!(
            specs("#!/usr/bin/env node\nimport a from 'a'"),
            vec![s("a", Static)]
        );
        for code in [
            "new URL(",
            "new URL('x'",
            "new URL(`a",
            "import.meta.glob(",
            "import.meta.glob('./a/*.js', {",
            "process.env.",
            "process.env.NODE_ENV",
        ] {
            let _ = scan(code);
        }
    }

    #[test]
    fn node_env_urls_and_globs() {
        let code = "const a = process.env.NODE_ENV;\nprocess.env.NODE_ENV = 'x';\nif (process.env.NODE_ENV === 'production') {}\nconst b = x.process.env.NODE_ENV + process.env.NODE_ENVX;\nconst s = \"process.env.NODE_ENV\";\nconst c = [global.process.env.NODE_ENV, globalThis.process.env.NODE_ENV, globalThis.process.env.X];\n\
            const u = new URL('./a.png', import.meta.url);\nconst v = new URL(`./b.svg`, import.meta.url).href;\nconst w = new URL(`./${n}.png`, import.meta.url);\nconst z = new URL('./c.png', base);\n\
            const g = import.meta.glob(['./p/*.js', '!./p/x.js'], { eager: true, query: { a: ')' } });\n";
        let hot = scan_hot(code);
        let spans: Vec<&str> = hot
            .node_env
            .iter()
            .map(|&(s, e)| &code[s as usize..e as usize])
            .collect();
        assert_eq!(
            spans,
            vec![
                "process.env.NODE_ENV",
                "process.env.NODE_ENV",
                "global.process.env.NODE_ENV",
                "globalThis.process.env.NODE_ENV"
            ]
        );
        assert_eq!(
            hot.node_env[1].0 as usize,
            code.find("(process").unwrap() + 1
        );
        let urls: Vec<&str> = hot
            .asset_urls
            .iter()
            .map(|r| r.specifier.as_str())
            .collect();
        assert_eq!(urls, vec!["./a.png", "./b.svg"]);
        for r in &hot.asset_urls {
            assert_eq!(&code[r.start as usize..r.end as usize], r.specifier);
        }
        assert_eq!(hot.globs.len(), 1);
        let g = hot.globs[0];
        assert!(code[g.start as usize..g.end as usize].starts_with("import.meta.glob("));
        assert!(code[g.start as usize..g.end as usize].ends_with("} })"));
        assert_eq!(
            &code[g.args_start as usize..g.args_end as usize],
            "['./p/*.js', '!./p/x.js'], { eager: true, query: { a: ')' } }"
        );
        assert!(scan_hot("const x = 1").node_env.is_empty());
    }

    #[test]
    fn exported_declarators_skip_regex_literals() {
        let m = scan(
            r#"export const pattern = /,oops/;
import.meta.hot.acceptExports(['pattern']);"#,
        );
        assert_eq!(m.exports, ["pattern"]);
        assert_eq!(m.hot.accepted_exports, Some(vec!["pattern".to_owned()]));

        let m = scan(
            r#"export const a = /[,;{}\/]/g, b = 8 / 2, c = b / 2, d = /,fake/;
export const e = () => /,alsoFake/;"#,
        );
        assert_eq!(m.exports, ["a", "b", "c", "d", "e"]);
    }

    #[test]
    fn exports_bindings_and_accepted_exports() {
        let code = r#"import def, { a, b as c, "x-y" as xy } from './t'
import * as ns from "./ns"
import './side'
export { a as b2 } from './re'
export * from './star'
export * as all from './all'
const aliased = 1
export { aliased as c, def as default }
export const p = f(1, 2), q = [3, 4],
  r = { s: 5 }
export let t
export async function g() {}
export function* h() {}
export class K {}
const later = import('./lazy')
if (import.meta.hot) {
  import.meta.hot.acceptExports(['p', 'default'])
  import.meta.hot.acceptExports('q', (m) => m)
}
"#;
        let m = scan(code);
        assert_eq!(
            m.exports,
            [
                "b2", "all", "c", "default", "p", "q", "r", "t", "g", "h", "K"
            ]
        );
        let by_spec: Vec<(&str, Vec<&str>)> = m
            .imports
            .iter()
            .map(|r| {
                let b = m.bindings.iter().find(|(at, _)| *at == r.start).unwrap();
                (
                    r.specifier.as_str(),
                    b.1.iter().map(String::as_str).collect(),
                )
            })
            .collect();
        assert_eq!(
            by_spec,
            vec![
                ("./t", vec!["a", "b", "default", "x-y"]),
                ("./ns", vec!["*"]),
                ("./side", vec![]),
                ("./re", vec![]),
                ("./star", vec![]),
                ("./all", vec![]),
                ("./lazy", vec!["*"]),
            ]
        );
        assert_eq!(
            m.hot.accepted_exports.as_deref(),
            Some(&["default".to_owned(), "p".to_owned(), "q".to_owned()][..])
        );
        assert!(!m.hot.self_accepting);
        let empty = scan_hot("import.meta.hot.acceptExports([])");
        assert_eq!(empty.accepted_exports.as_deref(), Some(&[][..]));
        assert!(
            scan_hot("import.meta.hot.accept()")
                .accepted_exports
                .is_none()
        );
    }
}
