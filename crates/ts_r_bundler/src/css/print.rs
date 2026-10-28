//! Prints a [`Sheet`]'s rules: two-space indentation and one declaration per line, or
//! everything on one line with `minify`. Token text is copied as written except whitespace,
//! which collapses to one space; references and local names come from the caller.
//!
//! With `syntax` (`--minify-syntax`), observed in esbuild 0.28.2: a declaration or rule that
//! is printed exactly like a later one in the same list is dropped (the later one wins
//! anyway; `@layer`, `@import` and `@charset` rules stay), and a `& ` that starts a selector
//! outside any style rule is dropped (`@media x { & a {} }` is `@media x { a {} }`).

use super::lexer::{self, T, Token};
use super::{Cv, Node, Sheet};

/// What the printer asks the bundler.
pub struct Printer<'a> {
    pub sheet: &'a Sheet,
    pub minify: bool,
    pub syntax: bool,
    /// The text of a `url()` (unquoted) for record `r`.
    pub url: &'a dyn Fn(u32) -> String,
    /// The final name of local name `n`.
    pub local: &'a dyn Fn(u32) -> String,
}

impl std::fmt::Debug for Printer<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Printer")
            .field("minify", &self.minify)
            .finish()
    }
}

impl Printer<'_> {
    pub fn nodes(&self, nodes: &[Node], indent: usize, out: &mut String) {
        self.list(nodes, indent, false, out);
    }

    fn list(&self, nodes: &[Node], indent: usize, in_style: bool, out: &mut String) {
        if !self.syntax {
            for n in nodes {
                self.node_in(n, indent, in_style, out);
            }
            return;
        }
        let printed = self.each(nodes, indent, in_style);
        for text in dedupe(nodes, printed) {
            out.push_str(&text);
        }
    }

    /// Each node printed on its own.
    pub fn each(&self, nodes: &[Node], indent: usize, in_style: bool) -> Vec<String> {
        nodes
            .iter()
            .map(|n| {
                let mut s = String::new();
                self.node_in(n, indent, in_style, &mut s);
                s
            })
            .collect()
    }

    fn pad(&self, indent: usize, out: &mut String) {
        if !self.minify {
            for _ in 0..indent {
                out.push_str("  ");
            }
        }
    }

    fn nl(&self, out: &mut String) {
        if !self.minify {
            out.push('\n');
        }
    }

    pub fn node(&self, n: &Node, indent: usize, out: &mut String) {
        self.node_in(n, indent, false, out);
    }

    fn node_in(&self, n: &Node, indent: usize, in_style: bool, out: &mut String) {
        match n {
            Node::Decl {
                name,
                value,
                important,
            } => {
                self.pad(indent, out);
                out.push_str(self.sheet.text(name));
                out.push(':');
                if !self.minify && !value.is_empty() {
                    out.push(' ');
                }
                if self.syntax {
                    let colors = super::color::takes_color(self.sheet.text(name));
                    let ctx = Ctx {
                        colors,
                        numbers: true,
                        calc: false,
                        selector: false,
                    };
                    self.values_in(value, ctx, out);
                } else {
                    self.values(value, out);
                }
                if *important {
                    out.push_str(if self.minify {
                        "!important"
                    } else {
                        " !important"
                    });
                }
                out.push(';');
                self.nl(out);
            }
            Node::Rule { prelude, body } => {
                self.pad(indent, out);
                let prelude = match prelude.as_slice() {
                    [Cv::Tok(amp), Cv::Tok(ws), rest @ ..]
                        if self.syntax
                            && !in_style
                            && amp.t == T::Delim
                            && self.sheet.text(amp) == "&"
                            && ws.t == T::Ws
                            && !rest.is_empty()
                            && !rest.iter().any(|cv| {
                                matches!(cv, Cv::Tok(t) if t.t == T::Comma
                                    || t.t == T::Delim && matches!(self.sheet.text(t), "&" | ">" | "+" | "~"))
                            }) =>
                    {
                        rest
                    }
                    p => p,
                };
                if self.minify {
                    self.selector(prelude, out);
                } else {
                    // One selector of a list per line, as esbuild prints them.
                    // Top-level commas (not those inside a written `:is(`).
                    let mut depth = 0usize;
                    let is_comma = |cv: &Cv| match cv {
                        Cv::Raw(r) if r.ends_with('(') => {
                            depth += 1;
                            false
                        }
                        Cv::Raw(r) if &**r == ")" => {
                            depth = depth.saturating_sub(1);
                            false
                        }
                        Cv::Tok(t) => t.t == T::Comma && depth == 0,
                        Cv::Raw(r) => &**r == "," && depth == 0,
                        _ => false,
                    };
                    for (i, sel) in prelude.split(is_comma).enumerate() {
                        if i > 0 {
                            out.push_str(",\n");
                            self.pad(indent, out);
                        }
                        let ws =
                            |cv: &Cv| matches!(cv, Cv::Space | Cv::Tok(Token { t: T::Ws, .. }));
                        let start = sel.iter().position(|c| !ws(c)).unwrap_or(sel.len());
                        let end = sel.iter().rposition(|c| !ws(c)).map_or(start, |e| e + 1);
                        self.selector(&sel[start..end.max(start)], out);
                    }
                }
                self.block(body, indent, true, out);
            }
            Node::At {
                name,
                raw,
                prelude,
                body,
            } => {
                self.pad(indent, out);
                out.push('@');
                out.push_str(if matches!(name.as_str(), "media" | "layer") {
                    name
                } else {
                    raw
                });
                if !prelude.is_empty() {
                    out.push(' ');
                    self.values(prelude, out);
                }
                match body {
                    Some(b) if self.syntax && name.ends_with("keyframes") => {
                        self.keyframes(b, indent, out);
                    }
                    Some(b) => self.block(b, indent, in_style, out),
                    None => {
                        out.push(';');
                        self.nl(out);
                    }
                }
            }
            // Bundling removes imports; the printer only sees them without `--bundle`.
            Node::Import(i) => {
                let Some(imp) = self.sheet.imports.get(*i as usize) else {
                    return;
                };
                self.pad(indent, out);
                let c = self.conditions_text(&imp.conditions);
                out.push_str(&super::order::with_conditions(
                    &lexer::quote(&(self.url)(imp.record)),
                    &c,
                    self.minify,
                ));
                self.nl(out);
            }
        }
    }

    /// `layer(x) supports(y) media` as written after an `@import`'s URL.
    pub fn conditions_text(&self, c: &super::Conditions) -> String {
        let mut parts = Vec::new();
        if let Some(l) = &c.layer {
            if l.is_empty() {
                parts.push("layer".to_owned());
            } else {
                parts.push(format!("layer({})", self.values_text(l)));
            }
        }
        if let Some(s) = &c.supports {
            parts.push(format!("supports({})", self.values_text(s)));
        }
        if !c.media.is_empty() {
            parts.push(self.values_text(&c.media));
        }
        parts.join(" ")
    }

    fn block(&self, body: &[Node], indent: usize, in_style: bool, out: &mut String) {
        out.push_str(if self.minify { "{" } else { " {\n" });
        self.list(body, indent + 1, in_style, out);
        if self.minify && out.ends_with(';') {
            out.pop();
        }
        self.pad(indent, out);
        out.push('}');
        self.nl(out);
    }

    /// A `@keyframes` body with `--minify-syntax`: `from` is `0%` and `100%` is `to`.
    fn keyframes(&self, body: &[Node], indent: usize, out: &mut String) {
        out.push_str(if self.minify { "{" } else { " {\n" });
        let printed: Vec<String> = body
            .iter()
            .map(|n| {
                let mut s = String::new();
                match n {
                    Node::Rule { prelude, body } => {
                        self.pad(indent + 1, &mut s);
                        let text = self.values_text(prelude);
                        let sel: Vec<&str> = text
                            .split(',')
                            .map(|k| match k.trim() {
                                k if k.eq_ignore_ascii_case("from") => "0%",
                                "100%" => "to",
                                k => k,
                            })
                            .collect();
                        s.push_str(&sel.join(if self.minify { "," } else { ", " }));
                        self.block(body, indent + 1, true, &mut s);
                    }
                    other => self.node_in(other, indent + 1, false, &mut s),
                }
                s
            })
            .collect();
        for text in dedupe(body, printed) {
            out.push_str(&text);
        }
        self.pad(indent, out);
        out.push('}');
        self.nl(out);
    }

    pub fn values_text(&self, v: &[Cv]) -> String {
        let mut s = String::new();
        self.values(v, &mut s);
        s
    }

    pub fn values(&self, v: &[Cv], out: &mut String) {
        self.values_in(v, Ctx::default(), out);
    }

    /// A style rule's selector: like [`Printer::values`], but a space before `:` is a
    /// descendant combinator (`a :hover` is not `a:hover`), so it stays.
    fn selector(&self, v: &[Cv], out: &mut String) {
        let ctx = Ctx {
            selector: true,
            ..Ctx::default()
        };
        self.values_in(v, ctx, out);
    }

    fn values_in(&self, v: &[Cv], ctx: Ctx, out: &mut String) {
        let Ctx {
            colors, numbers, ..
        } = ctx;
        let tight = |cv: &Cv| {
            is_tight(cv, self.sheet)
                || matches!(cv, Cv::Raw(r) if &**r == ",")
                || ctx.calc
                    && matches!(cv, Cv::Tok(t) if t.t == T::Delim && matches!(self.sheet.text(t), "*" | "/"))
        };
        let n = v.len();
        for (i, cv) in v.iter().enumerate() {
            match cv {
                Cv::Raw(text) => out.push_str(text),
                Cv::Tok(Token { t: T::Ws, .. }) | Cv::Space => {
                    if i == 0 || i + 1 == n {
                        continue;
                    }
                    let next_tight = tight(&v[i + 1])
                        && !(ctx.selector && matches!(&v[i + 1], Cv::Tok(t) if t.t == T::Colon));
                    if self.minify && (tight(&v[i - 1]) || next_tight) {
                        continue;
                    }
                    out.push(' ');
                }
                Cv::Tok(t) => {
                    let short = (colors && matches!(t.t, T::Ident | T::Hash | T::IdHash))
                        .then(|| super::color::shorten_token(self.sheet.text(t)))
                        .flatten();
                    let short = short.or_else(|| {
                        (numbers && matches!(t.t, T::Number | T::Percentage | T::Dimension))
                            .then(|| super::calc::short_number(self.sheet.text(t)))
                            .flatten()
                    });
                    match short {
                        Some(c) => out.push_str(&c),
                        None => self.token(t, out),
                    }
                }
                Cv::Func { name, items } => {
                    if colors && let Some(c) = self.color_function(name, items) {
                        out.push_str(&c);
                        continue;
                    }
                    let fname = self.sheet.text(name);
                    let mut tree = ctx.calc;
                    if numbers && fname.eq_ignore_ascii_case("calc(") {
                        match super::calc::fold(self.sheet, items) {
                            Ok(folded) => {
                                out.push_str(&folded);
                                continue;
                            }
                            Err(why) => tree = why == super::calc::Unfolded::Tree,
                        }
                    }
                    out.push_str(fname);
                    let inner = Ctx {
                        colors: colors && !fname.eq_ignore_ascii_case("var("),
                        numbers,
                        calc: tree,
                        selector: ctx.selector,
                    };
                    self.values_in(items, inner, out);
                    out.push(')');
                }
                Cv::Block { open, items } => {
                    out.push(*open as char);
                    let inner = Ctx {
                        colors: false,
                        ..ctx
                    };
                    self.values_in(items, inner, out);
                    out.push(match open {
                        b'(' => ')',
                        b'[' => ']',
                        _ => '}',
                    });
                }
                Cv::Url { record } => {
                    let u = (self.url)(*record);
                    out.push_str("url(");
                    if !u.is_empty()
                        && !u.contains(|c: char| {
                            c.is_ascii_whitespace() || matches!(c, '"' | '\'' | '(' | ')' | '\\')
                        })
                        && self.minify
                    {
                        out.push_str(&u);
                    } else {
                        out.push_str(&lexer::quote(&u));
                    }
                    out.push(')');
                }
                Cv::Local { name, prefix } => {
                    if *prefix != 0 {
                        out.push(*prefix as char);
                    }
                    out.push_str(&lexer::escape_ident(&(self.local)(*name)));
                }
            }
        }
    }

    /// `rgb(255, 0, 0)` and the like as their shortest color text.
    fn color_function(&self, name: &Token, items: &[Cv]) -> Option<String> {
        let mut args = Vec::new();
        for cv in items {
            match cv {
                Cv::Tok(t) if matches!(t.t, T::Ws | T::Comma) => {}
                Cv::Tok(t) if t.t == T::Delim && self.sheet.text(t) == "/" => {}
                Cv::Tok(t) if matches!(t.t, T::Number | T::Percentage | T::Dimension) => {
                    args.push(self.sheet.text(t));
                }
                _ => return None,
            }
        }
        super::color::shorten_function(self.sheet.text(name), &args)
    }

    fn token(&self, t: &Token, out: &mut String) {
        let text = self.sheet.text(t);
        match t.t {
            T::Str => out.push_str(&lexer::quote(&lexer::string_value(text))),
            _ => out.push_str(text),
        }
    }
}

/// How declaration values print with `--minify-syntax`.
#[derive(Clone, Copy, Debug, Default)]
struct Ctx {
    /// Colors in their shortest form (`color`).
    colors: bool,
    /// Short numbers and folded `calc()` (`calc`).
    numbers: bool,
    /// Inside a `calc()` that didn't fold for its structure (`calc::Unfolded::Tree`).
    calc: bool,
    /// Printing a selector (functions like `:is()` included).
    selector: bool,
}

/// Whether a node may be dropped for an identical later one.
pub fn droppable(n: &Node) -> bool {
    match n {
        Node::Import(_) => false,
        Node::At { name, .. } => !matches!(name.as_str(), "layer" | "import" | "charset"),
        _ => true,
    }
}

/// `printed` without the texts an identical later droppable node repeats.
fn dedupe(nodes: &[Node], printed: Vec<String>) -> Vec<String> {
    let mut seen = rustc_hash::FxHashSet::default();
    let mut keep = vec![true; printed.len()];
    for i in (0..printed.len()).rev() {
        let key = printed[i].trim();
        if droppable(&nodes[i]) && !seen.insert(key.to_owned()) {
            keep[i] = false;
        }
    }
    printed
        .into_iter()
        .zip(keep)
        .filter(|(_, k)| *k)
        .map(|(t, _)| t)
        .collect()
}

/// Tokens that need no space around them in minified output.
fn is_tight(cv: &Cv, sheet: &Sheet) -> bool {
    match cv {
        Cv::Tok(t) => match t.t {
            T::Comma | T::Colon | T::Semicolon => true,
            T::Delim => matches!(sheet.text(t), ">" | "~"),
            _ => false,
        },
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::super::{NameMode, parse};
    use super::*;
    use std::sync::Arc;

    fn print(src: &str, minify: bool) -> String {
        print_with(src, minify, false, NameMode::Local)
    }

    fn print_with(src: &str, minify: bool, syntax: bool, mode: NameMode) -> String {
        let s = parse(Arc::from(src), mode);
        let mut out = String::new();
        let url = |r: u32| format!("u{r}");
        let local = |n: u32| format!("f_{}", s.locals[n as usize]);
        Printer {
            sheet: &s,
            minify,
            syntax,
            url: &url,
            local: &local,
        }
        .nodes(&s.rules, 0, &mut out);
        out
    }

    #[test]
    fn pretty_and_minified() {
        assert_eq!(
            print(".a  >  b { color :  red ; background: url(x.png) }", false),
            ".f_a > b {\n  color: red;\n  background: url(\"u0\");\n}\n"
        );
        assert_eq!(
            print("@media screen { .a { x: y !important } }", true),
            "@media screen{.f_a{x:y!important}}"
        );
    }

    /// A space before `:` in a selector is a descendant combinator and survives minifying;
    /// after a combinator or comma it still goes.
    #[test]
    fn minified_selectors_keep_descendant_space_before_pseudo() {
        let min = |src: &str| print_with(src, true, false, NameMode::Global);
        assert_eq!(min("a :hover { color: red }"), "a :hover{color:red}");
        assert_eq!(
            min(".p :is(.c, .d :focus) { x: y }"),
            ".p :is(.c,.d :focus){x:y}"
        );
        assert_eq!(
            min("a ::before, b > :hover , c :first-child { x: y }"),
            "a ::before,b>:hover,c :first-child{x:y}"
        );
        assert_eq!(min("a:hover { x : y }"), "a:hover{x:y}");
        assert_eq!(
            min("@media (min-width : 1px) { a { x: y } }"),
            "@media (min-width:1px){a{x:y}}"
        );
    }

    /// `--minify-syntax`, as esbuild prints it: numbers, `calc()`, keyframe selectors and
    /// at-rule names as written (but `@media` and `@layer`).
    #[test]
    fn minify_syntax_values() {
        let min = |src: &str| print_with(src, true, true, NameMode::Global);
        assert_eq!(
            min("a { a: CaLc(20Px + 30pX); b: calc(2em - 3px + 1px); c: calc((1px + 2px) * 2) }"),
            "a{a:50Px;b:calc(2em - 2px);c:6px}"
        );
        assert_eq!(
            min(
                "a { a: calc(.1px + .2px); b: calc(10px / 3 * 3); c: calc(1px / 0); d: calc(2 * (1px + 1em)) }"
            ),
            "a{a:calc(.1px + .2px);b:10px;c:calc(1px / 0);d:calc(2*(1px + 1em))}"
        );
        assert_eq!(
            min(
                "a { a: 0.50px; b: -0.0px; c: 1e3px; d: calc(1px+2px); e: max(calc(1px + 1px), 1em) }"
            ),
            "a{a:.5px;b:-0px;c:1e3px;d:calc(1px+2px);e:max(2px,1em)}"
        );
        assert_eq!(
            min(
                "@KeyFrames k { froM { a: 0 } 100% { a: 1 } 50.0% { a: 2 } } @MEDIA x { b { c: d } }"
            ),
            "@KeyFrames k{0%{a:0}to{a:1}50.0%{a:2}}@media x{b{c:d}}"
        );
        // Deep nesting stays as written instead of recursing without bound.
        let deep = format!("a {{ b: calc({}1px{}) }}", "(".repeat(100), ")".repeat(100));
        assert!(min(&deep).starts_with("a{b:calc(("), "{}", min(&deep));
    }
}
