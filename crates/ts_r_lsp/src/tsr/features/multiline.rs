//! Hover shows object types one member per line, as tsserver's quick info does (it prints
//! with multi-line object literals: `{`, each member indented four spaces per level and ended
//! by `;`, then `}`). The checker prints types on one line (`{ a: string; b: number; }`);
//! this pass reshapes that text. Mapped types (`{ [K in keyof T]: T[K]; }`) stay on one
//! line, as in tsc. Observed in tsc 7's quick info output (`tsc: tsc/internal/ls/hover.go`
//! asks the printer for multi-line object literals).

/// `s` with every object type literal spread over lines.
pub fn multiline_objects(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 16);
    write(s, 0, &mut out);
    out
}

const INDENT: &str = "    ";

/// The end (exclusive) of the quoted text starting at `i` (`"`, `'` or a backtick).
fn skip_quoted(b: &[u8], i: usize) -> usize {
    let q = b[i];
    let mut j = i + 1;
    while j < b.len() {
        match b[j] {
            b'\\' => j += 2,
            c if c == q => return j + 1,
            _ => j += 1,
        }
    }
    b.len()
}

/// The index of the `}` that closes the `{` at `open`, if any.
fn matching_brace(b: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0u32;
    let mut j = open;
    while j < b.len() {
        match b[j] {
            b'"' | b'\'' | b'`' => {
                j = skip_quoted(b, j);
                continue;
            }
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(j);
                }
            }
            _ => {}
        }
        j += 1;
    }
    None
}

/// The members of an object type body (the text between `{ ` and ` }`), split at the `;`s
/// outside nested braces, brackets, parentheses and strings.
fn members(body: &str) -> Vec<&str> {
    let b = body.as_bytes();
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    let mut j = 0;
    while j < b.len() {
        match b[j] {
            b'"' | b'\'' | b'`' => {
                j = skip_quoted(b, j);
                continue;
            }
            b'{' | b'(' | b'[' => depth += 1,
            b'}' | b')' | b']' => depth -= 1,
            b';' if depth == 0 => {
                out.push(body[start..j].trim());
                start = j + 1;
            }
            _ => {}
        }
        j += 1;
    }
    let rest = body[start..].trim();
    if !rest.is_empty() {
        out.push(rest);
    }
    out
}

/// Whether an object body is a mapped type's (`[K in ...]: X;`, maybe with `readonly`).
fn is_mapped(body: &str) -> bool {
    let body = body.trim_start();
    let body = body
        .strip_prefix("+readonly ")
        .or_else(|| body.strip_prefix("-readonly "))
        .or_else(|| body.strip_prefix("readonly "))
        .unwrap_or(body);
    let Some(rest) = body.strip_prefix('[') else {
        return false;
    };
    let mut chars = rest.char_indices();
    // `[K in ...`: an identifier, then ` in `.
    let end = chars
        .find(|&(_, ch)| !(ch.is_alphanumeric() || ch == '_' || ch == '$'))
        .map_or(rest.len(), |(i, _)| i);
    end > 0 && rest[end..].starts_with(" in ")
}

fn write(s: &str, depth: usize, out: &mut String) {
    let b = s.as_bytes();
    let mut i = 0;
    let mut copied = 0;
    while i < b.len() {
        match b[i] {
            b'"' | b'\'' | b'`' => {
                i = skip_quoted(b, i);
                continue;
            }
            b'{' if b.get(i + 1) == Some(&b' ') => {
                let Some(close) = matching_brace(b, i) else {
                    break;
                };
                let body = &s[i + 1..close];
                // Object binding patterns in printed parameters (`{ a, b }: T`) end without
                // a `;` and stay as they are.
                if body.trim().is_empty() || is_mapped(body) || !body.trim_end().ends_with(';') {
                    i = close + 1;
                    continue;
                }
                out.push_str(&s[copied..i]);
                out.push_str("{\n");
                for m in members(body) {
                    for _ in 0..=depth {
                        out.push_str(INDENT);
                    }
                    write(m, depth + 1, out);
                    out.push_str(";\n");
                }
                for _ in 0..depth {
                    out.push_str(INDENT);
                }
                out.push('}');
                i = close + 1;
                copied = i;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    out.push_str(&s[copied..]);
}

#[cfg(test)]
mod tests {
    use super::multiline_objects as m;

    #[test]
    fn spreads_object_types_over_lines() {
        assert_eq!(
            m("var c: { name: string; age: number; }[]"),
            "var c: {\n    name: string;\n    age: number;\n}[]"
        );
        assert_eq!(
            m("var x: { a: Foo<{ x: number; }>; }"),
            "var x: {\n    a: Foo<{\n        x: number;\n    }>;\n}"
        );
        // Strings, empty objects and mapped types are left alone.
        assert_eq!(m("var s: \"{ a; }\" | {}"), "var s: \"{ a; }\" | {}");
        assert_eq!(
            m("type F<T> = { [K in keyof T]: T[K]; }"),
            "type F<T> = { [K in keyof T]: T[K]; }"
        );
        assert_eq!(
            m("var r: { [x: number]: Date; }"),
            "var r: {\n    [x: number]: Date;\n}"
        );
        // A destructured parameter is not a type.
        assert_eq!(
            m("const f: ({ a: { b, c } }: P) => any"),
            "const f: ({ a: { b, c } }: P) => any"
        );
        // Unbalanced text is copied as it is.
        assert_eq!(m("var a: { b: string;"), "var a: { b: string;");
    }
}
