//! minimatch 3's `makeRe` (default options), which eslint-plugin-import uses for path
//! globs: braces expand to alternatives, each `/`-segment becomes a regex piece and the
//! pieces are joined by `/`. Unlike minimatch's own `match`, the regex form treats `**`
//! as one segment (`a/**/**` needs two slashes after `a`). `*` and `?` never match `/`;
//! a segment that does not start with `.` cannot match a leading dot; `**` never matches
//! a segment starting with `.`. `#` starts a comment pattern (matches nothing); `!`
//! negations are not supported (they match nothing either).
//!
//! [`glob_match`] is minimatch's own `minimatch(path, pattern, options)` (used by
//! `import/order` path groups): the path and the pattern are split on runs of `/` and
//! compared segment by segment, and `**` stands for zero or more whole segments (none of
//! them `.`, `..` or dot-led). A pattern without the trailing segment still matches a path
//! that ends in `/`.

use crate::config::glob::expand_braces;

const TWO_STAR: &str = r"(?:(?!(?:\/|^)\.).)*?";
const STAR: &str = r"[^/]*?";
const QMARK: &str = "[^/]";

/// The JS regex source for `pattern`, or `None` for a pattern that matches nothing.
pub(crate) fn make_re(pattern: &str) -> Option<String> {
    if pattern.starts_with('#') || pattern.starts_with('!') {
        return None;
    }
    let alts: Vec<String> = expand_braces(pattern)
        .iter()
        .map(|p| {
            p.split('/')
                .map(|seg| {
                    if seg == "**" {
                        TWO_STAR.to_owned()
                    } else {
                        segment(seg)
                    }
                })
                .collect::<Vec<_>>()
                .join(r"\/")
        })
        .collect();
    Some(format!("^(?:{})$", alts.join("|")))
}

/// Whether `text` matches `pattern` (`makeRe(pattern).test(text)`).
pub(crate) fn matches(pattern: &str, text: &str) -> bool {
    make_re(pattern)
        .as_deref()
        .and_then(crate::regex::compile)
        .is_some_and(|r| r.is_match(text, false))
}

/// The options of [`glob_match`] that the plugin passes (`nocomment`, `nonegate`).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct MatchOptions {
    pub nocomment: bool,
    pub nonegate: bool,
}

/// minimatch's `minimatch(text, pattern, options)` with the default `dot: false`.
pub(crate) fn glob_match(text: &str, pattern: &str, opts: MatchOptions) -> bool {
    if !opts.nocomment && pattern.starts_with('#') {
        return false;
    }
    if pattern.trim().is_empty() {
        return text.is_empty();
    }
    let mut pattern = pattern;
    let mut negate = false;
    if !opts.nonegate {
        while let Some(rest) = pattern.strip_prefix('!') {
            negate = !negate;
            pattern = rest;
        }
    }
    let file: Vec<&str> = split_slashes(text);
    let mut budget = 10_000u32;
    let hit = expand_braces(pattern).iter().any(|alt| {
        let segs: Vec<Seg> = split_slashes(alt)
            .into_iter()
            .map(|s| {
                if s == "**" {
                    Seg::GlobStar
                } else {
                    Seg::Part(format!("^(?:{})$", segment(s)))
                }
            })
            .collect();
        match_segments(&file, &segs, &mut budget)
    });
    hit != negate
}

enum Seg {
    GlobStar,
    /// The regex source of one segment.
    Part(String),
}

/// `s.split(/\/+/)`.
fn split_slashes(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = s;
    loop {
        match rest.find('/') {
            Some(i) => {
                out.push(&rest[..i]);
                rest = rest[i..].trim_start_matches('/');
            }
            None => {
                out.push(rest);
                return out;
            }
        }
    }
}

fn dot_led(seg: &str) -> bool {
    seg.starts_with('.')
}

/// minimatch's `matchOne` (not partial). `budget` bounds the globstar backtracking.
fn match_segments(file: &[&str], pat: &[Seg], budget: &mut u32) -> bool {
    if *budget == 0 {
        return false;
    }
    *budget -= 1;
    let (mut fi, mut pi) = (0, 0);
    while fi < file.len() && pi < pat.len() {
        match &pat[pi] {
            Seg::GlobStar => {
                if pi + 1 == pat.len() {
                    return file[fi..].iter().all(|f| !dot_led(f));
                }
                let mut fr = fi;
                while fr < file.len() {
                    if match_segments(&file[fr..], &pat[pi + 1..], budget) {
                        return true;
                    }
                    if dot_led(file[fr]) || *budget == 0 {
                        break;
                    }
                    fr += 1;
                }
                return false;
            }
            Seg::Part(re) => {
                if !crate::regex::compile(re).is_some_and(|r| r.is_match(file[fi], false)) {
                    return false;
                }
            }
        }
        fi += 1;
        pi += 1;
    }
    if fi == file.len() && pi == pat.len() {
        true
    } else if fi == file.len() {
        false
    } else {
        // Pattern used up: `a` still matches `a/`.
        fi + 1 == file.len() && file[fi].is_empty()
    }
}

fn push_escaped(re: &mut String, c: char) {
    if "\\^$.*+?()[]{}|/".contains(c) {
        re.push('\\');
    }
    re.push(c);
}

fn segment(seg: &str) -> String {
    let mut re = String::new();
    let mut magic = false;
    let chars: Vec<char> = seg.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\\' if i + 1 < chars.len() => {
                push_escaped(&mut re, chars[i + 1]);
                i += 2;
                continue;
            }
            '*' => {
                // Runs of stars are one star.
                while i + 1 < chars.len() && chars[i + 1] == '*' {
                    i += 1;
                }
                re.push_str(STAR);
                magic = true;
            }
            '?' => {
                re.push_str(QMARK);
                magic = true;
            }
            '[' => match class_end(&chars, i) {
                Some(end) => {
                    re.push('[');
                    let mut j = i + 1;
                    if matches!(chars.get(j), Some('!' | '^')) {
                        re.push('^');
                        j += 1;
                    }
                    while j < end {
                        let k = chars[j];
                        if k == '\\' || (k == ']' && j > i + 1) || k == '[' {
                            re.push('\\');
                        }
                        re.push(k);
                        j += 1;
                    }
                    re.push(']');
                    magic = true;
                    i = end;
                }
                None => push_escaped(&mut re, c),
            },
            _ => push_escaped(&mut re, c),
        }
        i += 1;
    }
    if !magic {
        return re;
    }
    let start = if seg.starts_with('.') { "" } else { r"(?!\.)" };
    let lead = if re.starts_with('[') { start } else { "" };
    format!("{lead}(?=.){re}")
}

/// The index of the `]` closing the class opened at `open` (a `]` right after the `[` or
/// `[!` is literal).
fn class_end(chars: &[char], open: usize) -> Option<usize> {
    let mut j = open + 1;
    if matches!(chars.get(j), Some('!' | '^')) {
        j += 1;
    }
    if chars.get(j) == Some(&']') {
        j += 1;
    }
    while j < chars.len() {
        match chars[j] {
            '\\' => j += 1,
            ']' => return Some(j),
            _ => {}
        }
        j += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regex_sources_match_minimatch() {
        assert_eq!(
            make_re("app/**/**").unwrap(),
            r"^(?:app\/(?:(?!(?:\/|^)\.).)*?\/(?:(?!(?:\/|^)\.).)*?)$"
        );
        assert_eq!(
            make_re("*/app/*").unwrap(),
            r"^(?:(?!\.)(?=.)[^/]*?\/app\/(?!\.)(?=.)[^/]*?)$"
        );
        assert_eq!(make_re("lodash.*").unwrap(), r"^(?:(?=.)lodash\.[^/]*?)$");
    }

    #[test]
    fn matching() {
        assert!(!matches("app/**/**", "app/a"));
        assert!(matches("app/**/**", "app/a/b"));
        assert!(matches("*/app/*", "/app/index.js"));
        assert!(matches("**/index{.js,}", "/x/index"));
        assert!(!matches("**", ".hidden"));
        assert!(!matches("#x", "#x"));
    }

    #[test]
    fn glob_matching() {
        let o = MatchOptions {
            nocomment: true,
            nonegate: false,
        };
        assert!(glob_match("Internal/lib", "Internal/**/*", o));
        assert!(glob_match("~/components/Input", "~/**", o));
        assert!(glob_match("#/x", "#/**", o));
        assert!(!glob_match("#/x", "#/**", MatchOptions::default()));
        assert!(glob_match("~/x", "!/**", o));
        let nonegate = MatchOptions {
            nocomment: false,
            nonegate: true,
        };
        assert!(glob_match("!/x", "!/**", nonegate));
        assert!(glob_match("a/", "a", o));
        assert!(glob_match("./", "./", o));
        assert!(!glob_match("../a", "./[a-z]*", o));
        assert!(glob_match("../a", "../[a-z]*", o));
        assert!(!glob_match("a/.b/c", "a/**/c", o));
        assert!(glob_match("a/c", "a/**/c", o));
        assert!(glob_match("@namespace", "@namespace", o));
        assert!(!glob_match("@namespace/a", "@namespace", o));
    }
}
