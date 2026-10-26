//! CSS modules (`x.module.css`): class and keyframe names made local to the file, and the
//! map from each local name to the names the page uses.
//!
//! The rules (the CSS Modules convention as Vite applies it in dev, through postcss-modules
//! or lightningcss: `vite: packages/vite/src/node/plugins/css.ts`, `compileCSS` and
//! `compileLightningCSS`; postcss-modules' default scoped-name generator in
//! `vite/dist/node/chunks/build.js`):
//!
//! - Every `.class` in a selector is local unless it sits inside `:global(...)` or after a
//!   bare `:global` (up to the next `,`). `:local(...)` and a bare `:local` switch back.
//!   The `:global`/`:local` markers themselves are removed.
//! - A `@keyframes name` is local too, and so is that name where `animation` or
//!   `animation-name` uses it, anywhere in the file.
//! - `composes: a b;` in a rule adds the local names `a` and `b` to the exported value of
//!   each class of the rule's selector; `from global` adds them as written; `from './x.css'`
//!   adds that file's names for them, and that file's (scoped) CSS goes before this one's.
//!   The declaration itself is removed.
//! - A local name's scoped form depends only on the file's URL and the name, so editing a
//!   rule keeps the class names a page already uses. With postcss-modules (Vite's default)
//!   it is `_<name>_<hash5>`; with `css.transformer: 'lightningcss'` it is
//!   `<hash6>_<name>`, with a leading `_` when the hash starts with a digit. The hashes are
//!   ours (blake3 of the URL), not the tools' own.
//! - The exports are in the order the names first appear.

use ts_r_cache::Key;

/// How local names are scoped: the shape of the class names the page sees.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Naming {
    /// postcss-modules, Vite's default: `_<name>_<hash>`.
    #[default]
    Postcss,
    /// `css.transformer: 'lightningcss'`: `<hash>_<name>`.
    Lightningcss,
}

/// A CSS module's text with its local names scoped, and its exports: local name to the
/// space-separated class names (its own and the ones it composes).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Scoped {
    pub css: String,
    pub exports: Vec<(String, String)>,
}

/// Selector nesting (`:global(:not(.a))`) deeper than this is copied unscoped.
const MAX_SELECTOR_DEPTH: u32 = 32;

/// The scoped form of `name` in the module at `key` (its URL, without a query).
pub fn scoped_name(naming: Naming, key: &str, name: &str) -> String {
    let hash = Key::of(key.as_bytes()).to_hex();
    match naming {
        Naming::Postcss => format!("_{name}_{}", &hash[..5]),
        Naming::Lightningcss => {
            let h = &hash[..6];
            match h.as_bytes()[0].is_ascii_digit() {
                true => format!("_{h}_{name}"),
                false => format!("{h}_{name}"),
            }
        }
    }
}

enum Ctx {
    /// The top level and `@media`-like blocks: rules.
    Group,
    /// A style rule's block, with the local classes of its selector.
    Rule(Vec<String>),
    /// `@keyframes`: the selectors are percentages.
    Keyframes,
    /// Other blocks of declarations (`@font-face`, a keyframe).
    Decls,
}

/// Scopes the module `css` served at `key`. `compose` returns the scoped form of the file a
/// `composes: ... from '<spec>'` names (`None` when it can't be read).
pub fn scope(
    css: &str,
    key: &str,
    naming: Naming,
    compose: &mut dyn FnMut(&str) -> Option<Scoped>,
) -> Scoped {
    let mut s = Scoper {
        key,
        naming,
        keyframes: keyframe_names(css),
        exports: Vec::new(),
        composed: Vec::new(),
    };
    let b = css.as_bytes();
    let mut out = String::with_capacity(css.len() + css.len() / 4);
    let mut stack = vec![Ctx::Group];
    let mut seg = 0;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'*') => i = skip_comment(b, i),
            b'"' | b'\'' => i = skip_string(b, i),
            b'\\' => i += 2,
            b'(' => i = skip_parens(b, i),
            b'{' => {
                let (text, ctx) = s.prelude(&css[seg..i], stack.last());
                out.push_str(&text);
                out.push('{');
                stack.push(ctx);
                i += 1;
                seg = i;
            }
            b';' => {
                if let Some(text) = s.declaration(&css[seg..i], stack.last(), compose) {
                    out.push_str(&text);
                    out.push(';');
                }
                i += 1;
                seg = i;
            }
            b'}' => {
                if let Some(text) = s.declaration(&css[seg..i], stack.last(), compose) {
                    out.push_str(&text);
                }
                out.push('}');
                if stack.len() > 1 {
                    stack.pop();
                }
                i += 1;
                seg = i;
            }
            _ => i += 1,
        }
    }
    out.push_str(&css[seg.min(css.len())..]);
    let mut prefix = String::new();
    for (_, file) in &s.composed {
        if let Some(f) = file {
            prefix.push_str(&f.css);
            if !f.css.ends_with('\n') {
                prefix.push('\n');
            }
        }
    }
    prefix.push_str(&out);
    Scoped {
        css: prefix,
        exports: s.exports,
    }
}

struct Scoper<'a> {
    key: &'a str,
    naming: Naming,
    keyframes: Vec<String>,
    exports: Vec<(String, String)>,
    /// Files named by `composes: ... from`, by specifier, in first-use order.
    composed: Vec<(String, Option<Scoped>)>,
}

impl Scoper<'_> {
    fn local(&mut self, name: &str) -> String {
        let scoped = scoped_name(self.naming, self.key, name);
        if !self.exports.iter().any(|(n, _)| n == name) {
            self.exports.push((name.to_owned(), scoped.clone()));
        }
        scoped
    }

    /// A block's prelude: its replacement text and the block it opens.
    fn prelude(&mut self, text: &str, parent: Option<&Ctx>) -> (String, Ctx) {
        let trimmed = trim_css(text);
        if let Some(at) = trimmed.strip_prefix('@') {
            let name_len = ident_len(at.as_bytes(), 0);
            let name = at[..name_len].to_ascii_lowercase();
            if name.ends_with("keyframes") {
                return (self.keyframes_prelude(text), Ctx::Keyframes);
            }
            let ctx = match name.as_str() {
                "media" | "supports" | "layer" | "container" | "document" | "scope"
                | "starting-style" | "-moz-document" => Ctx::Group,
                _ => Ctx::Decls,
            };
            return (text.to_owned(), ctx);
        }
        if matches!(parent, Some(Ctx::Keyframes)) {
            return (text.to_owned(), Ctx::Decls);
        }
        let mut classes = Vec::new();
        let scoped = self.selector(text, false, 0, &mut classes);
        (scoped, Ctx::Rule(classes))
    }

    /// `@keyframes name` with the name scoped (`:global(name)` unwrapped).
    fn keyframes_prelude(&mut self, text: &str) -> String {
        let b = text.as_bytes();
        let Some(at) = text.find('@') else {
            return text.to_owned();
        };
        let mut i = at + 1 + ident_len(b, at + 1);
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        let rest = &text[i..];
        for (marker, global) in [(":global(", true), (":local(", false)] {
            if starts_with_ci(rest, marker) {
                let start = i + marker.len();
                let len = ident_len(b, start);
                let name = &text[start..start + len];
                let close = text[start + len..]
                    .find(')')
                    .map_or(b.len(), |j| start + len + j + 1);
                let name = match global {
                    true => name.to_owned(),
                    false => self.local(name),
                };
                return format!("{}{name}{}", &text[..i], &text[close..]);
            }
        }
        let len = ident_len(b, i);
        if len == 0 {
            return text.to_owned();
        }
        let name = self.local(&text[i..i + len]);
        format!("{}{name}{}", &text[..i], &text[i + len..])
    }

    /// A selector list with its local classes scoped; their names go to `classes`.
    fn selector(
        &mut self,
        sel: &str,
        global_in: bool,
        depth: u32,
        classes: &mut Vec<String>,
    ) -> String {
        if depth > MAX_SELECTOR_DEPTH {
            return sel.to_owned();
        }
        let b = sel.as_bytes();
        let mut out = String::with_capacity(sel.len() + 16);
        let mut global = global_in;
        let mut i = 0;
        let mut copied = 0;
        while i < b.len() {
            match b[i] {
                b'/' if b.get(i + 1) == Some(&b'*') => i = skip_comment(b, i),
                b'"' | b'\'' => i = skip_string(b, i),
                b'\\' => i += 2,
                b'[' => i = skip_brackets(b, i),
                b',' => {
                    global = global_in;
                    i += 1;
                }
                b'.' if ident_len(b, i + 1) > 0 => {
                    let len = ident_len(b, i + 1);
                    let name = &sel[i + 1..i + 1 + len];
                    if !global {
                        out.push_str(&sel[copied..=i]);
                        out.push_str(&self.local(name));
                        if !classes.iter().any(|c| c == name) {
                            classes.push(name.to_owned());
                        }
                        copied = i + 1 + len;
                    }
                    i += 1 + len;
                }
                b':' if b.get(i + 1) != Some(&b':') => {
                    let len = ident_len(b, i + 1);
                    let word = sel[i + 1..i + 1 + len].to_ascii_lowercase();
                    let is_global = word == "global";
                    if !is_global && word != "local" {
                        i += 1 + len;
                        continue;
                    }
                    out.push_str(&sel[copied..i]);
                    let after = i + 1 + len;
                    if b.get(after) == Some(&b'(') {
                        let close = skip_parens(b, after);
                        // `close` is past a `)`, or the end of an unterminated group.
                        let inner_end = match close <= b.len() && b[close - 1] == b')' {
                            true => close - 1,
                            false => b.len(),
                        };
                        let inner = &sel[after + 1..inner_end.max(after + 1)];
                        out.push_str(&self.selector(inner, is_global, depth + 1, classes));
                        i = close.min(b.len());
                    } else {
                        global = is_global;
                        i = after;
                        while i < b.len() && b[i].is_ascii_whitespace() {
                            i += 1;
                        }
                    }
                    copied = i;
                }
                b':' => i += 2,
                _ => i += 1,
            }
        }
        out.push_str(&sel[copied.min(sel.len())..]);
        out
    }

    /// A declaration (or a statement at-rule): its replacement text, or `None` to drop it.
    fn declaration(
        &mut self,
        text: &str,
        ctx: Option<&Ctx>,
        compose: &mut dyn FnMut(&str) -> Option<Scoped>,
    ) -> Option<String> {
        let Some(colon) = top_level_colon(text) else {
            return Some(text.to_owned());
        };
        let prop = trim_css(&text[..colon]).to_ascii_lowercase();
        match prop.as_str() {
            "composes" | "compose-with" => {
                let Some(Ctx::Rule(classes)) = ctx else {
                    return Some(text.to_owned());
                };
                let classes = classes.clone();
                let names = self.composition(&text[colon + 1..], compose);
                for class in classes {
                    let scoped = self.local(&class);
                    if let Some(e) = self.exports.iter_mut().find(|(n, _)| *n == class) {
                        for n in &names {
                            if *n != scoped && !e.1.split(' ').any(|x| x == n) {
                                e.1.push(' ');
                                e.1.push_str(n);
                            }
                        }
                    }
                }
                None
            }
            "animation" | "animation-name" | "-webkit-animation" | "-webkit-animation-name"
                if !self.keyframes.is_empty() =>
            {
                Some(format!(
                    "{}{}",
                    &text[..=colon],
                    self.animation_value(&text[colon + 1..])
                ))
            }
            _ => Some(text.to_owned()),
        }
    }

    /// The class names a `composes` value adds.
    fn composition(
        &mut self,
        value: &str,
        compose: &mut dyn FnMut(&str) -> Option<Scoped>,
    ) -> Vec<String> {
        let value = trim_css(value);
        let words: Vec<&str> = value.split_ascii_whitespace().collect();
        let from = words.iter().rposition(|w| w.eq_ignore_ascii_case("from"));
        let (names, source) = match from {
            Some(k) => (&words[..k], Some(words[k + 1..].join(" "))),
            None => (&words[..], None),
        };
        let names: Vec<&str> = names.iter().map(|n| n.trim_matches(',')).collect();
        match source.as_deref().map(str::trim) {
            None => names.iter().map(|n| self.local(n)).collect(),
            Some(g) if g.eq_ignore_ascii_case("global") => {
                names.iter().map(|n| (*n).to_owned()).collect()
            }
            Some(spec) => {
                let spec = spec.trim_matches(['"', '\'']).to_owned();
                if !self.composed.iter().any(|(s, _)| *s == spec) {
                    let file = compose(&spec);
                    self.composed.push((spec.clone(), file));
                }
                let file = self
                    .composed
                    .iter()
                    .find(|(s, _)| *s == spec)
                    .and_then(|(_, f)| f.as_ref());
                names
                    .iter()
                    .filter_map(|n| {
                        file?
                            .exports
                            .iter()
                            .find(|(k, _)| k == n)
                            .map(|(_, v)| v.clone())
                    })
                    .collect()
            }
        }
    }

    /// An `animation` value with the file's keyframe names scoped.
    fn animation_value(&mut self, value: &str) -> String {
        let b = value.as_bytes();
        let mut out = String::with_capacity(value.len() + 16);
        let mut copied = 0;
        let mut i = 0;
        while i < b.len() {
            match b[i] {
                b'"' | b'\'' => i = skip_string(b, i),
                b'(' => i = skip_parens(b, i),
                _ => {
                    let len = ident_len(b, i);
                    if len == 0 {
                        i += 1;
                        continue;
                    }
                    let word = &value[i..i + len];
                    if self.keyframes.iter().any(|k| k == word) {
                        out.push_str(&value[copied..i]);
                        out.push_str(&self.local(word));
                        copied = i + len;
                    }
                    i += len;
                }
            }
        }
        out.push_str(&value[copied.min(value.len())..]);
        out
    }
}

/// The local `@keyframes` names of `css` (not `:global(...)` ones).
fn keyframe_names(css: &str) -> Vec<String> {
    let b = css.as_bytes();
    let mut names = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'*') => i = skip_comment(b, i),
            b'"' | b'\'' => i = skip_string(b, i),
            b'@' => {
                let len = ident_len(b, i + 1);
                let word = &css[i + 1..i + 1 + len];
                i += 1 + len;
                if !word.to_ascii_lowercase().ends_with("keyframes") {
                    continue;
                }
                while i < b.len() && b[i].is_ascii_whitespace() {
                    i += 1;
                }
                let rest = &css[i..];
                if starts_with_ci(rest, ":global(") {
                    continue;
                }
                let start = match starts_with_ci(rest, ":local(") {
                    true => i + 7,
                    false => i,
                };
                let len = ident_len(b, start);
                if len > 0 {
                    let name = css[start..start + len].to_owned();
                    if !names.contains(&name) {
                        names.push(name);
                    }
                }
                i = start + len;
            }
            _ => i += 1,
        }
    }
    names
}

/// Whether `s` starts with the ASCII `prefix`, ignoring case.
fn starts_with_ci(s: &str, prefix: &str) -> bool {
    s.as_bytes()
        .get(..prefix.len())
        .is_some_and(|p| p.eq_ignore_ascii_case(prefix.as_bytes()))
}

/// `text` without surrounding whitespace and comments.
fn trim_css(text: &str) -> &str {
    let mut t = text.trim();
    loop {
        if let Some(rest) = t.strip_prefix("/*") {
            t = rest.find("*/").map_or("", |j| &rest[j + 2..]).trim_start();
        } else if t.ends_with("*/")
            && let Some(j) = t.rfind("/*")
        {
            t = t[..j].trim_end();
        } else {
            return t;
        }
    }
}

/// The byte index of the first `:` outside strings, comments and parentheses.
fn top_level_colon(text: &str) -> Option<usize> {
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'*') => i = skip_comment(b, i),
            b'"' | b'\'' => i = skip_string(b, i),
            b'(' => i = skip_parens(b, i),
            b':' => return Some(i),
            _ => i += 1,
        }
    }
    None
}

/// The length of the CSS identifier starting at `i` (0 when none starts there).
fn ident_len(b: &[u8], i: usize) -> usize {
    let starts = |j: usize| match b.get(j) {
        Some(c) => c.is_ascii_alphabetic() || *c == b'_' || *c >= 0x80 || *c == b'\\',
        None => false,
    };
    let ok = match b.get(i) {
        Some(b'-') => starts(i + 1) || b.get(i + 1) == Some(&b'-'),
        _ => starts(i),
    };
    if !ok {
        return 0;
    }
    let mut j = i;
    while j < b.len() {
        match b[j] {
            b'\\' => j = (j + 2).min(b.len()),
            c if c.is_ascii_alphanumeric() || c == b'-' || c == b'_' || c >= 0x80 => j += 1,
            _ => break,
        }
    }
    j - i
}

/// Past the comment starting at `i`.
fn skip_comment(b: &[u8], i: usize) -> usize {
    let mut j = i + 2;
    while j + 1 < b.len() {
        if b[j] == b'*' && b[j + 1] == b'/' {
            return j + 2;
        }
        j += 1;
    }
    b.len()
}

/// Past the string starting at `i` (or the end of its line, unterminated).
fn skip_string(b: &[u8], i: usize) -> usize {
    let q = b[i];
    let mut j = i + 1;
    while j < b.len() {
        match b[j] {
            b'\\' => j += 2,
            b'\n' => return j,
            c if c == q => return j + 1,
            _ => j += 1,
        }
    }
    b.len()
}

/// Past the `)` matching the `(` at `i` (strings and comments skipped).
fn skip_parens(b: &[u8], i: usize) -> usize {
    skip_balanced(b, i, b'(', b')')
}

/// Past the `]` matching the `[` at `i`.
fn skip_brackets(b: &[u8], i: usize) -> usize {
    skip_balanced(b, i, b'[', b']')
}

fn skip_balanced(b: &[u8], i: usize, open: u8, close: u8) -> usize {
    let mut depth = 0u32;
    let mut j = i;
    while j < b.len() {
        match b[j] {
            b'/' if b.get(j + 1) == Some(&b'*') => j = skip_comment(b, j),
            b'"' | b'\'' => j = skip_string(b, j),
            b'\\' => j += 2,
            b'{' | b'}' | b';' if open == b'[' => return j,
            c if c == open => {
                depth += 1;
                j += 1;
            }
            c if c == close => {
                depth = depth.saturating_sub(1);
                j += 1;
                if depth == 0 {
                    return j;
                }
            }
            _ => j += 1,
        }
    }
    b.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(css: &str, naming: Naming) -> Scoped {
        scope(css, "/a.module.css", naming, &mut |_| None)
    }

    fn name(n: &str) -> String {
        scoped_name(Naming::Postcss, "/a.module.css", n)
    }

    #[test]
    fn names_have_the_tools_shapes() {
        let p = scoped_name(Naming::Postcss, "/mod.module.css", "mod");
        assert!(
            p.starts_with("_mod_") && p.len() == "_mod_".len() + 5,
            "{p}"
        );
        let l = scoped_name(Naming::Lightningcss, "/mod.module.css", "apply-color");
        let hash = l.trim_start_matches('_');
        assert!(hash.ends_with("_apply-color"), "{l}");
        assert_eq!(hash.len(), 6 + "_apply-color".len(), "{l}");
        assert!(!l.as_bytes()[0].is_ascii_digit());
        assert_eq!(
            l,
            scoped_name(Naming::Lightningcss, "/mod.module.css", "apply-color")
        );
    }

    #[test]
    fn classes_are_scoped_and_exported_in_order() {
        let s = run(
            ".b, .a:hover > .b { color: red } div.c[data-x=\".d\"] { x: y }",
            Naming::Postcss,
        );
        let (a, b, c) = (name("a"), name("b"), name("c"));
        assert_eq!(
            s.css,
            format!(".{b}, .{a}:hover > .{b} {{ color: red }} div.{c}[data-x=\".d\"] {{ x: y }}")
        );
        let keys: Vec<&str> = s.exports.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["b", "a", "c"]);
    }

    #[test]
    fn global_and_local_markers() {
        let s = run(
            ":global(.g) .a, :global .h .i, .j :local(.k) {}\n:not(.n) {}",
            Naming::Postcss,
        );
        assert_eq!(
            s.css,
            format!(
                ".g .{}, .h .i, .{} .{} {{}}\n:not(.{}) {{}}",
                name("a"),
                name("j"),
                name("k"),
                name("n")
            )
        );
        assert!(!s.exports.iter().any(|(k, _)| k == "g" || k == "h"));
    }

    #[test]
    fn keyframes_and_animations() {
        let s = run(
            ".a { animation: spin 1s linear; color: url(spin.png) }\n@keyframes spin { from { opacity: 0 } 50.5% { opacity: 1 } }\n@keyframes :global(keep) {}",
            Naming::Postcss,
        );
        let spin = name("spin");
        assert_eq!(
            s.css,
            format!(
                ".{} {{ animation: {spin} 1s linear; color: url(spin.png) }}\n@keyframes {spin} {{ from {{ opacity: 0 }} 50.5% {{ opacity: 1 }} }}\n@keyframes keep {{}}",
                name("a")
            )
        );
    }

    #[test]
    fn media_blocks_and_comments_keep_their_text() {
        let s = run(
            "/* .x { } */ @media (min-width: 1.5em) { .a { color: red; } }\n@font-face { font-family: x; }",
            Naming::Postcss,
        );
        assert_eq!(
            s.css,
            format!(
                "/* .x {{ }} */ @media (min-width: 1.5em) {{ .{} {{ color: red; }} }}\n@font-face {{ font-family: x; }}",
                name("a")
            )
        );
    }

    #[test]
    fn composes_local_global_and_from_files() {
        let mut asked = Vec::new();
        let s = scope(
            ".base { color: red }\n.a { composes: base; composes: x y from global; composes: other from './o.module.css'; font: z }",
            "/a.module.css",
            Naming::Postcss,
            &mut |spec| {
                asked.push(spec.to_owned());
                Some(Scoped {
                    css: "._other_1 { color: blue }".to_owned(),
                    exports: vec![("other".to_owned(), "_other_1 _more_2".to_owned())],
                })
            },
        );
        assert_eq!(asked, ["./o.module.css"]);
        assert_eq!(
            s.css,
            format!(
                "._other_1 {{ color: blue }}\n.{} {{ color: red }}\n.{} {{ font: z }}",
                name("base"),
                name("a")
            )
        );
        let a = &s.exports.iter().find(|(k, _)| k == "a").unwrap().1;
        assert_eq!(
            *a,
            format!("{} {} x y _other_1 _more_2", name("a"), name("base"))
        );
    }

    #[test]
    fn malformed_input_does_not_panic() {
        for css in [
            ".a {",
            "}}}} .a",
            ":global(",
            ":global(.a",
            ".a { composes: ; }",
            ".a { composes: x from; }",
            "@keyframes",
            "@keyframes :global(",
            "@keyframes :local(",
            ".a[x=\"",
            "/* open",
            "\\",
            ".a:\\",
            ".é { animation: é }",
            "@keyframes é {} .b { animation: é 1s }",
        ] {
            let _ = run(css, Naming::Postcss);
            let _ = run(css, Naming::Lightningcss);
        }
        let deep = format!("{}.a{}", ":global(".repeat(200), ")".repeat(200));
        let _ = run(&deep, Naming::Postcss);
    }
}
