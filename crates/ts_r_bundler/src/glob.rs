//! Glob patterns of `require("./src/" + x)` and `` import(`./src/${x}`) `` (`scan.rs` builds
//! them): `*` matches within one path segment, `**/` any number of whole directories.

use ts_r_config::fs::{EntryKind, FileSystem};
use ts_r_config::path;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Lit(String),
    /// `*`: any text without `/`.
    Star,
    /// `**/`: zero or more directories, each with its `/`.
    Dirs,
    /// `**` not followed by `/`: any text.
    Any,
}

/// A parsed pattern: the directory the walk starts from, and the tokens the path below it
/// must match.
#[derive(Debug, Clone)]
pub struct Pattern {
    dir: String,
    tokens: Vec<Token>,
    /// The match can be below more directory levels than the pattern has `/` (it has `**`).
    deep: bool,
    slashes: usize,
}

impl Pattern {
    pub fn parse(pattern: &str) -> Pattern {
        let first = pattern.find('*').unwrap_or(pattern.len());
        let split = pattern[..first].rfind('/').map_or(0, |i| i + 1);
        let (dir, rest) = pattern.split_at(split);
        let mut tokens = Vec::new();
        let mut lit = String::new();
        let mut chars = rest.chars().peekable();
        while let Some(c) = chars.next() {
            if c != '*' {
                lit.push(c);
                continue;
            }
            if !lit.is_empty() {
                tokens.push(Token::Lit(std::mem::take(&mut lit)));
            }
            if chars.peek() == Some(&'*') {
                chars.next();
                if chars.peek() == Some(&'/') {
                    chars.next();
                    tokens.push(Token::Dirs);
                } else {
                    tokens.push(Token::Any);
                }
            } else {
                tokens.push(Token::Star);
            }
        }
        if !lit.is_empty() {
            tokens.push(Token::Lit(lit));
        }
        let deep = tokens.iter().any(|t| matches!(t, Token::Dirs | Token::Any));
        let slashes = rest.matches('/').count();
        Pattern {
            dir: dir.to_owned(),
            tokens,
            deep,
            slashes,
        }
    }

    /// The literal directory part, with its trailing `/` (`./src/` of `./src/**/*`).
    pub fn dir(&self) -> &str {
        &self.dir
    }

    /// Whether `rel` (a `/`-separated path below [`Pattern::dir`]) matches.
    pub fn matches(&self, rel: &str) -> bool {
        let s = rel.as_bytes();
        let m = s.len();
        // `next[j]`: the tokens after the current one match `s[j..]`. Rows are filled
        // right to left, so each is O(len): O(tokens * len) in all.
        let mut next = vec![false; m + 1];
        next[m] = true;
        let mut cur = vec![false; m + 1];
        for t in self.tokens.iter().rev() {
            match t {
                Token::Lit(l) => {
                    let l = l.as_bytes();
                    for j in 0..=m {
                        cur[j] = s[j..].starts_with(l) && next[j + l.len()];
                    }
                }
                Token::Star => {
                    cur[m] = next[m];
                    for j in (0..m).rev() {
                        cur[j] = next[j] || (s[j] != b'/' && cur[j + 1]);
                    }
                }
                Token::Any => {
                    cur[m] = next[m];
                    for j in (0..m).rev() {
                        cur[j] = next[j] || cur[j + 1];
                    }
                }
                Token::Dirs => {
                    // Either no directory here, or one segment and its `/`, then again.
                    let mut after_slash = false;
                    cur[m] = next[m];
                    for j in (0..m).rev() {
                        if s[j] == b'/' {
                            after_slash = cur[j + 1];
                            cur[j] = next[j];
                        } else {
                            cur[j] = next[j] || after_slash;
                        }
                    }
                }
            }
            std::mem::swap(&mut cur, &mut next);
        }
        next[0]
    }
}

/// The virtual directory of `ts_r_guard::vfs::GuardFs` (`ts-r-build` wraps the bundler's
/// file system in it; this crate does not depend on that one).
const GUARD_DIR: &str = ".ts-r-guard";

/// Collects the files below `base` (the absolute form of [`Pattern::dir`]) that match, as
/// `prefix` + their relative path (`./src/a.js`). Reads at most `max_depth` levels and
/// `max_entries` directories and files; returns `false` when a limit cut the walk short
/// (then `out` is incomplete and must not be used).
pub fn walk(
    fs: &dyn FileSystem,
    base: &str,
    prefix: &str,
    pattern: &Pattern,
    max_depth: usize,
    max_entries: usize,
    out: &mut Vec<String>,
) -> bool {
    let depth_limit = if pattern.deep {
        max_depth
    } else {
        pattern.slashes.min(max_depth)
    };
    let mut seen = 0usize;
    // (absolute directory, its path below `base` with a trailing `/`, depth)
    let mut stack = vec![(base.to_owned(), String::new(), 0usize)];
    while let Some((dir, rel, depth)) = stack.pop() {
        for e in fs.read_dir(&dir) {
            // `ts-r-build`'s `parse<T>` validators are virtual modules in a directory that
            // mirrors the project (`ts_r_guard::vfs::VDIR`): not files a pattern names.
            if e.name == GUARD_DIR {
                continue;
            }
            seen += 1;
            if seen > max_entries {
                return false;
            }
            let child_rel = format!("{rel}{}", e.name);
            match e.kind {
                EntryKind::File => {
                    if pattern.matches(&child_rel) {
                        out.push(format!("{prefix}{child_rel}"));
                    }
                }
                EntryKind::Directory if depth < depth_limit => {
                    stack.push((
                        path::combine(&dir, &e.name),
                        format!("{child_rel}/"),
                        depth + 1,
                    ));
                }
                // `**` could reach below the limit.
                EntryKind::Directory if pattern.deep => return false,
                EntryKind::Directory => {}
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(p: &str, rel: &str) -> bool {
        Pattern::parse(p).matches(rel)
    }

    #[test]
    fn patterns_split_at_the_last_literal_directory() {
        assert_eq!(Pattern::parse("./src/**/*").dir(), "./src/");
        assert_eq!(Pattern::parse("./src/file-*.js").dir(), "./src/");
        assert_eq!(Pattern::parse("./sr*").dir(), "./");
        assert_eq!(Pattern::parse("../a/b/*/x").dir(), "../a/b/");
    }

    #[test]
    fn stars_stay_in_a_segment_and_double_stars_cross_directories() {
        assert!(m("./src/**/*", "a.js"));
        assert!(m("./src/**/*", "nested/dir/n.js"));
        assert!(m("./src/**/*.js", "nested/dir/file-a.js"));
        assert!(!m("./src/**/*.js", "file-a.js.map"));
        assert!(m("./src/file-*.js", "file-a.js"));
        assert!(!m("./src/file-*.js", "nested/dir/file-a.js"));
        assert!(m("./src/**/*/index.js", "nested/index.js"));
        assert!(!m("./src/**/*/index.js", "index.js"));
        assert!(m("./src/**/*/**/*", "dir/n.js"));
        assert!(!m("./src/**/*/**/*", "n.js"));
        assert!(m("./sr*", "src"));
        assert!(m("./é/*é", "aé"));
    }

    #[test]
    fn walks_stop_with_false_at_their_limits() {
        let mut fs = ts_r_config::fs::MemoryFs::new();
        fs.add_file("/p/src/a/b/c/d.js", "");
        fs.add_file("/p/src/x.js", "");
        let deep = Pattern::parse("./src/**/*.js");
        let mut out = Vec::new();
        assert!(walk(&fs, "/p/src", "./src/", &deep, 64, 100, &mut out));
        out.sort();
        assert_eq!(out, ["./src/a/b/c/d.js", "./src/x.js"]);
        assert!(!walk(
            &fs,
            "/p/src",
            "./src/",
            &deep,
            2,
            100,
            &mut Vec::new()
        ));
        assert!(!walk(
            &fs,
            "/p/src",
            "./src/",
            &deep,
            64,
            3,
            &mut Vec::new()
        ));
        // A `parse<T>` build's virtual validator directory is not walked.
        fs.add_file("/p/src/.ts-r-guard/x.js/T.js", "");
        let mut out = Vec::new();
        assert!(walk(&fs, "/p/src", "./src/", &deep, 64, 100, &mut out));
        assert_eq!(out.len(), 2, "{out:?}");
        // Without `**` the pattern's own depth bounds the walk: not an error.
        let flat = Pattern::parse("./src/*.js");
        let mut out = Vec::new();
        assert!(walk(&fs, "/p/src", "./src/", &flat, 64, 100, &mut out));
        assert_eq!(out, ["./src/x.js"]);
    }
}
