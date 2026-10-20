//! The runtime (`runtime.js`) as top-level items, so inline output carries only the items
//! its generated code reaches (the same tree shaking a bundler does for shared output), and
//! shared modules import only the names they use.

use std::sync::OnceLock;

use crate::RUNTIME;

/// One top-level declaration of the runtime.
struct Item {
    names: Vec<&'static str>,
    text: &'static str,
    /// Runtime names it references (filled in after splitting).
    refs: Vec<usize>,
    /// Whether it reads `$MAX_DEPTH` (emitted after the runtime, per output).
    max_depth: bool,
}

const MAX_DEPTH: &str = "$MAX_DEPTH";

fn is_ident_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'$'
}

/// Every identifier in `s` that starts with `$` (whole identifiers: `$f$parse$e0` is one).
fn dollar_idents(s: &str) -> impl Iterator<Item = &str> {
    let b = s.as_bytes();
    let mut i = 0;
    std::iter::from_fn(move || {
        while i < b.len() {
            if b[i] == b'$' && (i == 0 || !is_ident_char(b[i - 1])) {
                let start = i;
                i += 1;
                while i < b.len() && is_ident_char(b[i]) {
                    i += 1;
                }
                return Some(&s[start..i]);
            }
            i += 1;
        }
        None
    })
}

/// The names a top-level line declares: `const $a = ..`, `let $a, $b;`, `function $a(..`.
fn declared(line: &'static str) -> Vec<&'static str> {
    let rest = ["const ", "let ", "async function ", "function "]
        .iter()
        .find_map(|k| line.strip_prefix(k));
    let Some(rest) = rest else {
        return Vec::new();
    };
    if line.starts_with("let ") {
        let decl = rest.split(';').next().unwrap_or("");
        return decl
            .split(',')
            .map(|n| n.split('=').next().unwrap_or("").trim())
            .filter(|n| n.starts_with('$'))
            .collect();
    }
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '$'))
        .unwrap_or(rest.len());
    vec![&rest[..end]]
}

fn items() -> &'static [Item] {
    static ITEMS: OnceLock<Vec<Item>> = OnceLock::new();
    ITEMS.get_or_init(|| {
        let src: &'static str = RUNTIME;
        let mut items: Vec<Item> = Vec::new();
        // Byte offset of each item's first line; comments before an item belong to it.
        let mut starts: Vec<(usize, Vec<&'static str>)> = Vec::new();
        let mut comment_start: Option<usize> = None;
        let mut off = 0;
        for line in src.split_inclusive('\n') {
            let l = line.trim_end();
            if l.starts_with("//") {
                comment_start.get_or_insert(off);
            } else {
                let names = declared(l);
                if !names.is_empty() {
                    starts.push((comment_start.unwrap_or(off), names));
                }
                comment_start = None;
            }
            off += line.len();
        }
        for (i, (start, names)) in starts.iter().enumerate() {
            let end = starts.get(i + 1).map_or(src.len(), |(s, _)| *s);
            items.push(Item {
                names: names.clone(),
                text: &src[*start..end],
                refs: Vec::new(),
                max_depth: false,
            });
        }
        let index = |n: &str, items: &[Item]| items.iter().position(|it| it.names.contains(&n));
        for i in 0..items.len() {
            let mut refs = Vec::new();
            let mut max_depth = false;
            for id in dollar_idents(items[i].text) {
                if id == MAX_DEPTH {
                    max_depth = true;
                } else if let Some(j) = index(id, &items)
                    && j != i
                    && !refs.contains(&j)
                {
                    refs.push(j);
                }
            }
            items[i].refs = refs;
            items[i].max_depth = max_depth;
        }
        items
    })
}

/// Every name the runtime declares, in declaration order.
pub fn names() -> Vec<&'static str> {
    items()
        .iter()
        .flat_map(|it| it.names.iter().copied())
        .collect()
}

/// The runtime names `code` references directly, in declaration order.
pub fn used_names(code: &str) -> Vec<&'static str> {
    let ids: Vec<&str> = dollar_idents(code).collect();
    names().into_iter().filter(|n| ids.contains(n)).collect()
}

/// The runtime items `code` reaches, in their original order, then `$MAX_DEPTH` when one of
/// them reads it.
pub fn runtime_for(code: &str, max_depth: u32) -> String {
    let items = items();
    let mut keep = vec![false; items.len()];
    let mut stack: Vec<usize> = Vec::new();
    for id in dollar_idents(code) {
        if let Some(i) = items.iter().position(|it| it.names.contains(&id))
            && !keep[i]
        {
            keep[i] = true;
            stack.push(i);
        }
    }
    while let Some(i) = stack.pop() {
        for &j in &items[i].refs {
            if !keep[j] {
                keep[j] = true;
                stack.push(j);
            }
        }
    }
    let mut out = String::new();
    for (it, _) in items.iter().zip(&keep).filter(|(_, k)| **k) {
        out.push_str(it.text);
    }
    if items.iter().zip(&keep).any(|(it, k)| *k && it.max_depth) {
        out.push_str(&format!("const {MAX_DEPTH} = {max_depth};\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_the_runtime_into_items() {
        let n = names();
        for x in [
            "$parse",
            "$parse0",
            "$ParseError",
            "$P",
            "$I",
            "$K",
            "$O",
            "$U",
            "$panic",
        ] {
            assert!(n.contains(&x), "{x} in {n:?}");
        }
        // Each name once.
        let mut sorted = n.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), n.len());
    }

    #[test]
    fn keeps_only_reachable_items() {
        let rt = runtime_for("$parse(v, $is_x, $d_x, \"x\")", 512);
        assert!(rt.contains("function $parse("));
        assert!(rt.contains("const $w ="));
        assert!(rt.contains("const $ParseError ="));
        assert!(!rt.contains("const $U ="), "{rt}");
        assert!(!rt.contains("$ExpectError"));
        assert!(!rt.contains("$MAX_DEPTH"));
        let rt = runtime_for("[$V, 1, 6]", 7);
        assert!(rt.contains("const $V =") && rt.contains("const $R ="));
        assert!(rt.ends_with("const $MAX_DEPTH = 7;\n"), "{rt}");
        assert_eq!(runtime_for("x", 512), "");
        assert_eq!(
            used_names("$f$parse$e0 = $parse0($obj)"),
            ["$obj", "$parse0"]
        );
    }
}
