//! A small part of the WHATWG URL parser, for unicorn rules that ask whether two relative
//! URLs resolve to the same place: special-scheme bases (`http`, `https`, `ws`, `wss`, `ftp`,
//! `file`) and relative inputs.
//!
//! What is modelled: input clean-up (leading and trailing C0 controls and spaces trimmed, tabs
//! and line breaks removed), scheme detection, `\` as a path separator, dot segments (also
//! `%2e`), the relative forms (empty, `?query`, `#fragment`, a path), and the `file` quirks
//! (a Windows drive letter as the first segment, and never shortening a path that is just a
//! drive letter). Percent-encoding is left out: both sides of a comparison encode the same
//! characters the same way. Anything else (non-special bases, inputs with a scheme or an
//! authority) is "can't say", which callers treat as different.

/// A parsed base URL.
pub(crate) struct Base {
    scheme: String,
    authority: String,
    path: Vec<String>,
    query: Option<String>,
}

const SPECIAL: &[&str] = &["http", "https", "ws", "wss", "ftp", "file"];

/// Leading and trailing C0 controls and spaces trimmed, then tabs and line breaks removed.
fn clean(input: &str) -> String {
    input
        .trim_matches(|c: char| c <= ' ')
        .chars()
        .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
        .collect()
}

/// The scheme of `s` (lowercased) and the rest after its `:`.
pub(crate) fn split_scheme(s: &str) -> Option<(String, &str)> {
    let mut chars = s.char_indices();
    let (_, first) = chars.next()?;
    if !first.is_ascii_alphabetic() {
        return None;
    }
    for (i, c) in chars {
        if c == ':' {
            return Some((s[..i].to_ascii_lowercase(), &s[i + 1..]));
        }
        if !(c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')) {
            return None;
        }
    }
    None
}

fn is_sep(c: char) -> bool {
    c == '/' || c == '\\'
}

fn is_single_dot(s: &str) -> bool {
    s == "." || s.eq_ignore_ascii_case("%2e")
}

fn is_double_dot(s: &str) -> bool {
    let l = s.to_ascii_lowercase();
    matches!(l.as_str(), ".." | ".%2e" | "%2e." | "%2e%2e")
}

/// `C:` or `C|`.
fn is_drive_letter(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 2 && b[0].is_ascii_alphabetic() && matches!(b[1], b':' | b'|')
}

/// `s` starts with a Windows drive letter followed by the end, a separator, `?` or `#`.
fn starts_with_drive_letter(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 2
        && b[0].is_ascii_alphabetic()
        && matches!(b[1], b':' | b'|')
        && (b.len() == 2 || matches!(b[2], b'/' | b'\\' | b'?' | b'#'))
}

impl Base {
    pub(crate) fn parse(input: &str) -> Option<Base> {
        let s = clean(input);
        let (scheme, rest) = split_scheme(&s)?;
        if !SPECIAL.contains(&scheme.as_str()) {
            return None;
        }
        let file = scheme == "file";
        let (authority, rest) = if file {
            let after = rest
                .strip_prefix("//")
                .or_else(|| rest.strip_prefix("\\\\"))
                .or_else(|| rest.strip_prefix("/\\"))
                .or_else(|| rest.strip_prefix("\\/"));
            match after {
                Some(after) => {
                    let end = after
                        .find(|c: char| is_sep(c) || c == '?' || c == '#')
                        .unwrap_or(after.len());
                    (after[..end].to_ascii_lowercase(), &after[end..])
                }
                None => (String::new(), rest),
            }
        } else {
            let after = rest.trim_start_matches(is_sep);
            let end = after
                .find(|c: char| is_sep(c) || c == '?' || c == '#')
                .unwrap_or(after.len());
            if end == 0 {
                return None;
            }
            (after[..end].to_ascii_lowercase(), &after[end..])
        };
        let rest = rest.strip_prefix(is_sep).unwrap_or(rest);
        let mut path = Vec::new();
        let query = parse_path(rest, &mut path, file);
        Some(Base {
            scheme,
            authority,
            path,
            query: query.map(|(q, _)| q),
        })
    }

    /// The `href` of `input` resolved against this base, or `None` when it can't say.
    pub(crate) fn resolve(&self, input: &str) -> Option<String> {
        let s = clean(input);
        if split_scheme(&s).is_some() {
            return None;
        }
        let file = self.scheme == "file";
        if s.starts_with(is_sep) || (file && starts_with_drive_letter(&s)) {
            return None;
        }
        let (path, query, fragment) = if s.is_empty() {
            (self.path.clone(), self.query.clone(), None)
        } else if let Some(rest) = s.strip_prefix('?') {
            let (q, f) = match rest.split_once('#') {
                Some((q, f)) => (q.to_owned(), Some(f.to_owned())),
                None => (rest.to_owned(), None),
            };
            (self.path.clone(), Some(q), f)
        } else if let Some(f) = s.strip_prefix('#') {
            (self.path.clone(), self.query.clone(), Some(f.to_owned()))
        } else {
            let mut path = self.path.clone();
            shorten(&mut path, file);
            match parse_path(&s, &mut path, file) {
                Some((q, f)) => (path, Some(q), f),
                None => {
                    let f = s.split_once('#').map(|(_, f)| f.to_owned());
                    (path, None, f)
                }
            }
        };
        let mut href = format!("{}://{}/{}", self.scheme, self.authority, path.join("/"));
        if let Some(q) = query {
            href.push('?');
            href.push_str(&q);
        }
        if let Some(f) = fragment {
            href.push('#');
            href.push_str(&f);
        }
        Some(href)
    }
}

fn shorten(path: &mut Vec<String>, file: bool) {
    if file && path.len() == 1 && is_drive_letter(&path[0]) && path[0].as_bytes()[1] == b':' {
        return;
    }
    path.pop();
}

/// The path state: appends `input`'s segments (up to `?` or `#`) to `path`. Returns the
/// query and the fragment after it when there is a `?` before any `#`.
fn parse_path(input: &str, path: &mut Vec<String>, file: bool) -> Option<(String, Option<String>)> {
    let mut buffer = String::new();
    let mut chars = input.char_indices();
    loop {
        let next = chars.next();
        let c = next.map(|(_, c)| c);
        if c.is_none_or(|c| is_sep(c) || c == '?' || c == '#') {
            let at_sep = c.is_some_and(is_sep);
            if is_double_dot(&buffer) {
                shorten(path, file);
                if !at_sep {
                    path.push(String::new());
                }
            } else if is_single_dot(&buffer) {
                if !at_sep {
                    path.push(String::new());
                }
            } else {
                if file && path.is_empty() && is_drive_letter(&buffer) {
                    buffer.replace_range(1..2, ":");
                }
                path.push(std::mem::take(&mut buffer));
            }
            buffer.clear();
            match (next, c) {
                (Some((i, '?')), _) => {
                    let rest = &input[i + 1..];
                    return Some(match rest.split_once('#') {
                        Some((q, f)) => (q.to_owned(), Some(f.to_owned())),
                        None => (rest.to_owned(), None),
                    });
                }
                (_, None | Some('#')) => return None,
                _ => {}
            }
        } else if let Some(c) = c {
            buffer.push(c);
        }
    }
}

/// `new URL(url, base).href === new URL("./" + url, base).href` for every base (a base that
/// doesn't parse counts as different).
pub(crate) fn dot_slash_is_noop(url: &str, bases: &[&str]) -> bool {
    let with = format!("./{url}");
    bases.iter().all(|b| {
        Base::parse(b).is_some_and(|base| match (base.resolve(url), base.resolve(&with)) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_like_whatwg() {
        let b = Base::parse("https://example.com/a/b.html").unwrap();
        assert_eq!(b.resolve("").unwrap(), "https://example.com/a/b.html");
        assert_eq!(b.resolve("./").unwrap(), "https://example.com/a/");
        assert_eq!(b.resolve("?q").unwrap(), "https://example.com/a/b.html?q");
        assert_eq!(b.resolve("./?q").unwrap(), "https://example.com/a/?q");
        assert_eq!(b.resolve("../x/./y").unwrap(), "https://example.com/x/y");
        let f = Base::parse("file:///D:/project/index.js").unwrap();
        assert_eq!(f.resolve("../../..").unwrap(), "file:///D:/");
        assert_eq!(f.resolve("./C|/a").unwrap(), "file:///D:/project/C|/a");
        assert!(dot_slash_is_noop("foo", &["https://example.com/a/b.html"]));
        assert!(!dot_slash_is_noop(" foo", &["https://example.com/a/b/"]));
        assert!(dot_slash_is_noop("", &["https://example.com/a/b/"]));
        assert!(!dot_slash_is_noop("", &["https://example.com/a/b.html"]));
        assert!(!dot_slash_is_noop(
            "http:foo",
            &["https://example.com/a/b/"]
        ));
    }
}
