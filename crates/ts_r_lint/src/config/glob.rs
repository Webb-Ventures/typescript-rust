//! Glob matching with minimatch semantics as used by ESLint flat config (`dot: true`) and
//! `.gitignore`: `*`, `**`, `?`, `[...]`, `{a,b}`. Paths are `/`-separated and relative.

/// A compiled glob (brace alternatives expanded).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Glob {
    alts: Vec<Vec<Seg>>,
    /// The pattern ended with `/`: it matches directories only.
    pub dir_only: bool,
    pub source: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Seg {
    /// `**`
    Globstar,
    /// A segment pattern (may contain `*`, `?`, classes).
    Pat(Vec<Tok>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Tok {
    Lit(char),
    Any,                            // ?
    Star,                           // *
    Class(bool, Vec<(char, char)>), // negated, ranges
}

const MAX_ALTS: usize = 256;

impl Glob {
    pub fn new(pattern: &str) -> Glob {
        let mut p = pattern.trim();
        let source = p.to_owned();
        while let Some(rest) = p.strip_prefix("./") {
            p = rest;
        }
        let p = p.strip_prefix('/').unwrap_or(p);
        let dir_only = p.ends_with('/') && p.len() > 1;
        let p = p.trim_end_matches('/');
        let mut alts = Vec::new();
        for expanded in expand_braces(p) {
            let segs = expanded
                .split('/')
                .filter(|s| !s.is_empty())
                .map(|s| {
                    if s == "**" {
                        Seg::Globstar
                    } else {
                        Seg::Pat(parse_seg(s))
                    }
                })
                .collect();
            alts.push(segs);
            if alts.len() >= MAX_ALTS {
                break;
            }
        }
        Glob {
            alts,
            dir_only,
            source,
        }
    }

    /// Matches a relative file path.
    pub fn matches(&self, path: &str) -> bool {
        let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        self.alts.iter().any(|segs| match_segs(segs, &parts))
    }

    /// True when the pattern matches the directory `dir` itself (for `dir/` and `dir/**`
    /// style ignores, which make everything inside the directory ignored).
    pub fn matches_dir(&self, dir: &str) -> bool {
        let parts: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
        self.alts.iter().any(|segs| {
            if match_segs(segs, &parts) {
                return true;
            }
            // `dir/**` matches the directory's contents: treat as matching the dir.
            if let Some((Seg::Globstar, head)) = segs.split_last()
                && !head.is_empty()
            {
                return match_segs(head, &parts);
            }
            false
        })
    }

    /// True for patterns ESLint treats as "universal" (`*`, `**`, `**/*`, `dir/**`): they do
    /// not by themselves make a file lintable.
    pub fn is_universal(&self) -> bool {
        let s = self.source.trim_end_matches('/');
        s == "*" || s == "**" || s.ends_with("/*") || s.ends_with("/**")
    }
}

pub(crate) fn expand_braces(p: &str) -> Vec<String> {
    // Find the first top-level `{...}` with a comma.
    let b: Vec<char> = p.chars().collect();
    let mut i = 0;
    while i < b.len() {
        if b[i] == '\\' {
            i += 2;
            continue;
        }
        if b[i] == '{' {
            let mut depth = 0;
            let mut j = i;
            let mut commas = Vec::new();
            while j < b.len() {
                match b[j] {
                    '\\' => {
                        j += 1;
                    }
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    ',' if depth == 1 => commas.push(j),
                    _ => {}
                }
                j += 1;
            }
            if j < b.len() && !commas.is_empty() {
                let pre: String = b[..i].iter().collect();
                let post: String = b[j + 1..].iter().collect();
                let mut out = Vec::new();
                let mut start = i + 1;
                for &c in commas.iter().chain(std::iter::once(&j)) {
                    let mid: String = b[start..c].iter().collect();
                    for tail in expand_braces(&format!("{mid}{post}")) {
                        out.push(format!("{pre}{tail}"));
                        if out.len() >= MAX_ALTS {
                            return out;
                        }
                    }
                    start = c + 1;
                }
                return out;
            }
        }
        i += 1;
    }
    vec![p.to_owned()]
}

fn parse_seg(s: &str) -> Vec<Tok> {
    let c: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < c.len() {
        match c[i] {
            '\\' if i + 1 < c.len() => {
                out.push(Tok::Lit(c[i + 1]));
                i += 2;
                continue;
            }
            '*' => {
                if out.last() != Some(&Tok::Star) {
                    out.push(Tok::Star);
                }
            }
            '?' => out.push(Tok::Any),
            '[' => {
                if let Some(end) = c[i + 1..].iter().position(|&x| x == ']').map(|p| p + i + 1)
                    && end > i + 1
                {
                    let mut j = i + 1;
                    let neg = c[j] == '!' || c[j] == '^';
                    if neg {
                        j += 1;
                    }
                    let mut ranges = Vec::new();
                    while j < end {
                        if j + 2 < end && c[j + 1] == '-' {
                            ranges.push((c[j], c[j + 2]));
                            j += 3;
                        } else {
                            ranges.push((c[j], c[j]));
                            j += 1;
                        }
                    }
                    out.push(Tok::Class(neg, ranges));
                    i = end + 1;
                    continue;
                }
                out.push(Tok::Lit('['));
            }
            ch => out.push(Tok::Lit(ch)),
        }
        i += 1;
    }
    out
}

fn match_segs(segs: &[Seg], parts: &[&str]) -> bool {
    match segs.first() {
        None => parts.is_empty(),
        Some(Seg::Globstar) => {
            // Zero or more segments.
            (0..=parts.len()).any(|k| match_segs(&segs[1..], &parts[k..]))
        }
        Some(Seg::Pat(toks)) => {
            !parts.is_empty() && match_toks(toks, parts[0]) && match_segs(&segs[1..], &parts[1..])
        }
    }
}

fn match_toks(toks: &[Tok], s: &str) -> bool {
    let chars: Vec<char> = s.chars().collect();
    // Iterative wildcard matching with backtracking on the last star.
    let (mut ti, mut ci) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while ci < chars.len() {
        if ti < toks.len() {
            match &toks[ti] {
                Tok::Star => {
                    star = Some((ti, ci));
                    ti += 1;
                    continue;
                }
                Tok::Any => {
                    ti += 1;
                    ci += 1;
                    continue;
                }
                Tok::Lit(l) if *l == chars[ci] => {
                    ti += 1;
                    ci += 1;
                    continue;
                }
                Tok::Class(neg, ranges) => {
                    let hit = ranges
                        .iter()
                        .any(|&(a, b)| a <= chars[ci] && chars[ci] <= b);
                    if hit != *neg {
                        ti += 1;
                        ci += 1;
                        continue;
                    }
                }
                _ => {}
            }
        }
        match star {
            Some((st, sc)) => {
                ti = st + 1;
                ci = sc + 1;
                star = Some((st, sc + 1));
            }
            None => return false,
        }
    }
    while ti < toks.len() && toks[ti] == Tok::Star {
        ti += 1;
    }
    ti == toks.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basics() {
        assert!(Glob::new("**/*.ts").matches("a/b/c.ts"));
        assert!(Glob::new("**/*.ts").matches("c.ts"));
        assert!(!Glob::new("**/*.ts").matches("c.tsx"));
        assert!(Glob::new("**/*.{ts,tsx}").matches("x/c.tsx"));
        assert!(Glob::new("dist/**").matches("dist/a/b.js"));
        assert!(!Glob::new("dist/**").matches("src/dist/a.js"));
        assert!(Glob::new("**/generated/**").matches("a/generated/x.ts"));
        assert!(Glob::new("**/*.gen.ts").matches("a/x.gen.ts"));
        assert!(Glob::new("**/.hidden/*.js").matches(".hidden/a.js"));
        assert!(Glob::new("*.config.{js,ts,mjs}").matches("vite.config.ts"));
        assert!(!Glob::new("*.config.{js,ts,mjs}").matches("a/vite.config.ts"));
        assert!(Glob::new("**/*.test.{ts,tsx}").matches("src/a.test.tsx"));
        assert!(Glob::new("**/test/**").matches("src/test/a.ts"));
        assert!(Glob::new("file[0-9].js").matches("file3.js"));
        assert!(!Glob::new("file[!0-9].js").matches("file3.js"));
        assert!(Glob::new("a/**/b").matches("a/b"));
        assert!(Glob::new("a/**/b").matches("a/x/y/b"));
    }

    #[test]
    fn dirs_and_universal() {
        assert!(Glob::new("dist/").matches_dir("dist"));
        assert!(Glob::new("dist/").dir_only);
        assert!(Glob::new("dist/**").matches_dir("dist"));
        assert!(Glob::new("**/node_modules/").matches_dir("a/node_modules"));
        assert!(Glob::new("**/*").is_universal());
        assert!(Glob::new("src/**").is_universal());
        assert!(!Glob::new("**/*.js").is_universal());
    }
}
