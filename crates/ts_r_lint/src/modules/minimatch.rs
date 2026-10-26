//! minimatch 3's `makeRe` (default options), which eslint-plugin-import uses for path
//! globs: braces expand to alternatives, each `/`-segment becomes a regex piece and the
//! pieces are joined by `/`. Unlike minimatch's own `match`, the regex form treats `**`
//! as one segment (`a/**/**` needs two slashes after `a`). `*` and `?` never match `/`;
//! a segment that does not start with `.` cannot match a leading dot; `**` never matches
//! a segment starting with `.`. `#` starts a comment pattern (matches nothing); `!`
//! negations are not supported (they match nothing either).

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
}
