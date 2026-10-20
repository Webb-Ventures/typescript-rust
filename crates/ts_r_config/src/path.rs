//! String-based path helpers.
//!
//! Every path inside ts-r is a `/`-separated string. Absolute paths start with `/` (POSIX),
//! `X:/` (Windows drive) or `//` (UNC). Backslashes are converted at the edges with
//! [`normalize_slashes`]. Keeping paths as strings (rather than `PathBuf`) makes case-folding,
//! prefix tests and relative-path printing cheap and identical on every platform.

/// Replaces every `\` with `/`.
pub fn normalize_slashes(path: &str) -> String {
    if path.contains('\\') {
        path.replace('\\', "/")
    } else {
        path.to_owned()
    }
}

/// Length of the root of `path` (`/`, `c:/`, `//server/`), or 0 when relative.
pub fn root_length(path: &str) -> usize {
    let b = path.as_bytes();
    if b.is_empty() {
        return 0;
    }
    if b[0] == b'/' {
        if b.len() > 1 && b[1] == b'/' {
            // UNC: //server/share
            if let Some(i) = path[2..].find('/') {
                return i + 3;
            }
            return path.len();
        }
        return 1;
    }
    if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
        if b.len() >= 3 && b[2] == b'/' {
            return 3;
        }
        return 2;
    }
    0
}

/// True when `path` is absolute.
pub fn is_rooted(path: &str) -> bool {
    root_length(path) > 0
}

/// Joins `rel` onto `base` without normalising. Rooted `rel` wins.
pub fn combine(base: &str, rel: &str) -> String {
    if rel.is_empty() {
        return base.to_owned();
    }
    if base.is_empty() || is_rooted(rel) {
        return rel.to_owned();
    }
    if base.ends_with('/') {
        format!("{base}{rel}")
    } else {
        format!("{base}/{rel}")
    }
}

/// Removes `.` and `..` segments, duplicate and trailing separators.
pub fn normalize(path: &str) -> String {
    let path = normalize_slashes(path);
    let root_len = root_length(&path);
    let (root, rest) = path.split_at(root_len);
    let mut parts: Vec<&str> = Vec::new();
    for seg in rest.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if matches!(parts.last(), Some(p) if *p != "..") {
                    parts.pop();
                } else if root.is_empty() {
                    parts.push("..");
                }
            }
            s => parts.push(s),
        }
    }
    let mut out = String::with_capacity(path.len());
    out.push_str(root);
    out.push_str(&parts.join("/"));
    if out.is_empty() && !path.is_empty() {
        out.push('.');
    }
    out
}

/// `normalize(combine(base, rel))`.
pub fn resolve(base: &str, rel: &str) -> String {
    normalize(&combine(base, rel))
}

/// Directory part of `path` (everything before the last separator), keeping the root.
pub fn directory_of(path: &str) -> &str {
    let root = root_length(path);
    let trimmed = path.trim_end_matches('/');
    let trimmed = if trimmed.len() < root {
        &path[..root]
    } else {
        trimmed
    };
    match trimmed.rfind('/') {
        Some(i) if i + 1 > root => &trimmed[..i],
        Some(_) => &path[..root],
        None => "",
    }
}

/// Last path component.
pub fn file_name(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(i) => &trimmed[i + 1..],
        None => trimmed,
    }
}

/// Splits a normalised absolute path into its root and components.
pub fn components(path: &str) -> (&str, Vec<&str>) {
    let root_len = root_length(path);
    let (root, rest) = path.split_at(root_len);
    (root, rest.split('/').filter(|s| !s.is_empty()).collect())
}

/// Compares two path strings, optionally ignoring ASCII and Unicode case.
pub fn eq(a: &str, b: &str, case_sensitive: bool) -> bool {
    if case_sensitive || a == b {
        a == b
    } else if a.eq_ignore_ascii_case(b) {
        true
    } else if a.is_ascii() && b.is_ascii() {
        false
    } else {
        a.to_lowercase() == b.to_lowercase()
    }
}

/// Key used for maps of paths: the path itself, or its lower-case form on case-insensitive
/// file systems.
pub fn key(path: &str, case_sensitive: bool) -> String {
    if case_sensitive {
        path.to_owned()
    } else {
        path.to_lowercase()
    }
}

/// True when `child` equals `parent` or lies inside it.
pub fn contains(parent: &str, child: &str, case_sensitive: bool) -> bool {
    let parent = parent.trim_end_matches('/');
    if child.len() < parent.len() {
        return false;
    }
    // `get`: the cut may fall inside a multi-byte character of `child`, which then differs.
    if !child
        .get(..parent.len())
        .is_some_and(|prefix| eq(prefix, parent, case_sensitive))
    {
        return false;
    }
    child.len() == parent.len()
        || child.as_bytes()[parent.len()] == b'/'
        || parent.ends_with('/')
        || parent.is_empty()
}

/// Relative path from directory `from` to `to` (both absolute and normalised), without a
/// `./` prefix. Returns `to` unchanged when the roots differ.
pub fn relative(from: &str, to: &str, case_sensitive: bool) -> String {
    let (fr, fc) = components(from);
    let (tr, tc) = components(to);
    if !eq(fr, tr, case_sensitive) {
        return to.to_owned();
    }
    let mut i = 0;
    while i < fc.len() && i < tc.len() && eq(fc[i], tc[i], case_sensitive) {
        i += 1;
    }
    let mut parts: Vec<&str> = vec![".."; fc.len() - i];
    parts.extend_from_slice(&tc[i..]);
    parts.join("/")
}

/// Relative path in the style tsc prints in `--showConfig`: `./x`, `../x` or `./`.
pub fn relative_dotted(from: &str, to: &str, case_sensitive: bool) -> String {
    let rel = relative(from, to, case_sensitive);
    if is_rooted(&rel) {
        rel
    } else if rel.is_empty() {
        "./".to_owned()
    } else if rel == ".." || rel.starts_with("../") {
        rel
    } else {
        format!("./{rel}")
    }
}

/// True for `./x`, `../x`, `.` and `..`.
pub fn is_explicitly_relative(spec: &str) -> bool {
    spec == "."
        || spec == ".."
        || spec.starts_with("./")
        || spec.starts_with("../")
        || spec.starts_with(".\\")
        || spec.starts_with("..\\")
}

/// Extension helpers shared by config and resolver.
pub mod ext {
    /// Extensions that hold TypeScript or declaration code, longest first where it matters.
    pub const DTS_LIKE: &[&str] = &[".d.ts", ".d.mts", ".d.cts"];

    /// True when `name` ends with `ext`, comparing ASCII case-sensitively like tsc.
    pub fn has(name: &str, ext: &str) -> bool {
        name.len() > ext.len() && name.ends_with(ext)
    }

    /// True for `.d.ts`, `.d.mts`, `.d.cts` and arbitrary-extension declarations
    /// (`foo.d.css.ts`).
    pub fn is_declaration(name: &str) -> bool {
        if DTS_LIKE.iter().any(|e| has(name, e)) {
            return true;
        }
        // `x.d.<anything>.ts`
        if let Some(stem) = name.strip_suffix(".ts")
            && let Some(i) = stem.rfind(".d.")
        {
            let after = &stem[i + 3..];
            return !after.is_empty() && !after.contains('/');
        }
        false
    }

    /// The tsc extension of a file name (`.d.ts`, `.ts`, `.json`, ...), or `""`.
    pub fn of(name: &str) -> &'static str {
        const ALL: &[&str] = &[
            ".d.ts", ".d.mts", ".d.cts", ".ts", ".tsx", ".mts", ".cts", ".js", ".jsx", ".mjs",
            ".cjs", ".json",
        ];
        for e in ALL {
            if has(name, e) {
                return e;
            }
        }
        ""
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_paths() {
        assert_eq!(normalize("/a/b/../c/./d/"), "/a/c/d");
        assert_eq!(normalize("/a/../../b"), "/b");
        assert_eq!(normalize("a/../../b"), "../b");
        assert_eq!(normalize("c:\\x\\y"), "c:/x/y");
        assert_eq!(normalize("/"), "/");
        assert_eq!(normalize("./"), ".");
    }

    #[test]
    fn directories() {
        assert_eq!(directory_of("/a/b/c.ts"), "/a/b");
        assert_eq!(directory_of("/a"), "/");
        assert_eq!(directory_of("/"), "/");
        assert_eq!(directory_of("c:/a"), "c:/");
        assert_eq!(file_name("/a/b/c.ts"), "c.ts");
    }

    #[test]
    fn relative_paths() {
        assert_eq!(relative("/a/b", "/a/c/d", true), "../c/d");
        assert_eq!(relative_dotted("/a/b", "/a/b/c", true), "./c");
        assert_eq!(relative_dotted("/a/b", "/a/b", true), "./");
        assert_eq!(relative_dotted("/a/b", "/a", true), "..");
        assert_eq!(relative_dotted("/a/B", "/a/b/x", false), "./x");
        assert_eq!(
            relative_dotted("/tmp/tsx/g", "/abs/x", true),
            "../../../abs/x"
        );
    }

    #[test]
    fn containment() {
        assert!(contains("/a/b", "/a/b/c", true));
        assert!(contains("/a/b", "/a/b", true));
        assert!(!contains("/a/b", "/a/bc", true));
        assert!(contains("/A/b", "/a/B/c", false));
        assert!(contains("/", "/x", true));
        // Byte 4 is inside `é`.
        assert!(!contains("/p/x", "/p/é.ts", true));
        assert!(!contains("/p/x", "/p/é.ts", false));
        assert!(contains("/p/é", "/p/é/a.ts", true));
    }

    #[test]
    fn extensions() {
        assert!(ext::is_declaration("a.d.ts"));
        assert!(ext::is_declaration("a.d.css.ts"));
        assert!(!ext::is_declaration("a.ts"));
        assert_eq!(ext::of("x.d.mts"), ".d.mts");
        assert_eq!(ext::of("x.mts"), ".mts");
    }
}
