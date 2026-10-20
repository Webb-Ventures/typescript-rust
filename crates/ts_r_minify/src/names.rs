//! Short identifier generation and reserved words.

/// First characters, roughly ordered by how often they appear in minified JavaScript so
/// that gzip finds more repeats. Every name is `FIRST[i]` followed by `REST` digits.
const FIRST: &[u8] = b"etnrisoaluchdfpmgbvywkxqjzETNRISOALUCHDFPMGBVYWKXQJZ_$";
const REST: &[u8] = b"etnrisoaluchdfpmgbvywkxqjzETNRISOALUCHDFPMGBVYWKXQJZ_$0123456789";

/// The `n`th short name in the sequence `e, t, n, ..., $, ee, te, ...`, skipping nothing.
/// Callers skip reserved words with [`is_reserved`].
pub fn nth_name(mut n: usize) -> String {
    let mut s = String::with_capacity(4);
    s.push(FIRST[n % FIRST.len()] as char);
    n /= FIRST.len();
    while n > 0 {
        n -= 1;
        s.push(REST[n % REST.len()] as char);
        n /= REST.len();
    }
    s
}

/// Words that may not be used as binding names (reserved words in any mode, plus names
/// whose rebinding would change meaning: `arguments`, `eval`, `undefined`, ...).
pub fn is_reserved(name: &str) -> bool {
    matches!(
        name,
        "do" | "if"
            | "in"
            | "as"
            | "of"
            | "for"
            | "let"
            | "new"
            | "try"
            | "var"
            | "case"
            | "else"
            | "enum"
            | "eval"
            | "null"
            | "this"
            | "true"
            | "void"
            | "with"
            | "await"
            | "break"
            | "catch"
            | "class"
            | "const"
            | "false"
            | "super"
            | "throw"
            | "while"
            | "yield"
            | "delete"
            | "export"
            | "import"
            | "public"
            | "return"
            | "static"
            | "switch"
            | "typeof"
            | "default"
            | "extends"
            | "finally"
            | "package"
            | "private"
            | "continue"
            | "debugger"
            | "function"
            | "arguments"
            | "interface"
            | "protected"
            | "implements"
            | "instanceof"
            | "undefined"
            | "NaN"
            | "Infinity"
            | "async"
            | "get"
            | "set"
    )
}

/// True when `s` can be written as an identifier (ASCII subset; anything else is quoted).
pub fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' || c == '$' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

/// True when `s` is a valid identifier that is not a reserved word (usable as a binding).
pub fn is_binding_name(s: &str) -> bool {
    is_identifier(s) && !is_reserved(s)
}

/// A legal identifier derived from arbitrary text (`react-dom/client` -> `react_dom_client`).
pub fn sanitize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_us = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '$' {
            out.push(c);
            last_us = c == '_';
        } else if !last_us && !out.is_empty() {
            out.push('_');
            last_us = true;
        }
    }
    while out.ends_with('_') && out.len() > 1 {
        out.pop();
    }
    if out.is_empty() || out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    if is_reserved(&out) {
        out.push('_');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequence_is_unique_and_valid() {
        let mut seen = std::collections::HashSet::new();
        for i in 0..20_000 {
            let n = nth_name(i);
            assert!(is_identifier(&n), "{n}");
            assert!(seen.insert(n));
        }
        assert_eq!(nth_name(0), "e");
        assert_eq!(nth_name(FIRST.len()).len(), 2);
    }

    #[test]
    fn sanitizing() {
        assert_eq!(sanitize("react-dom/client"), "react_dom_client");
        assert_eq!(sanitize("3d"), "_3d");
        assert_eq!(sanitize("class"), "class_");
        assert_eq!(sanitize("@scope/pkg"), "scope_pkg");
        assert_eq!(sanitize(""), "_");
    }
}
