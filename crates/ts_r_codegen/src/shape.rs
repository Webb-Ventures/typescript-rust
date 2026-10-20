//! AST shape for round-trip comparisons: the tree dump with spans removed.

use ts_r_ast::Ast;

/// The dump of `ast` without byte spans, so trees parsed from different texts compare equal
/// when they have the same structure and values.
pub fn shape(ast: &Ast) -> String {
    strip_spans(&ast.dump())
}

/// Removes `N..M` span tokens from an `Ast::dump` string.
pub fn strip_spans(dump: &str) -> String {
    let mut out = String::with_capacity(dump.len());
    for line in dump.lines() {
        let mut first = true;
        for word in line.split(' ') {
            if is_span_token(word) {
                continue;
            }
            if !first {
                out.push(' ');
            }
            out.push_str(word);
            first = false;
        }
        out.push('\n');
    }
    out
}

fn is_span_token(w: &str) -> bool {
    match w.split_once("..") {
        Some((a, b)) => {
            !a.is_empty()
                && !b.is_empty()
                && a.bytes().all(|c| c.is_ascii_digit())
                && b.bytes().all(|c| c.is_ascii_digit())
        }
        None => false,
    }
}

/// The first differing line of two shapes, with context, or `None` if equal.
pub fn first_difference(a: &str, b: &str) -> Option<String> {
    if a == b {
        return None;
    }
    let al: Vec<&str> = a.lines().collect();
    let bl: Vec<&str> = b.lines().collect();
    let i = al.iter().zip(&bl).take_while(|(x, y)| x == y).count();
    let ctx = i.saturating_sub(3);
    let mut s = String::new();
    for l in &al[ctx..i] {
        s.push_str("   ");
        s.push_str(l);
        s.push('\n');
    }
    s.push_str(&format!(
        " - {}\n + {}\n",
        al.get(i).unwrap_or(&"<end>"),
        bl.get(i).unwrap_or(&"<end>")
    ));
    Some(s)
}
