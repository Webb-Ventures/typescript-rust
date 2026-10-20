//! `no-regex-spaces`: disallow multiple spaces in regular expressions.

use super::prelude::*;
use crate::regex::{self, Flags};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-regex-spaces",
        "Disallow multiple spaces in regular expressions",
        ESLINT_REC,
        Category::Suspicious,
    )
    .fixable(),
    create: create_default::<NoRegexSpaces>,
};

#[derive(Debug, Default)]
struct NoRegexSpaces;

/// The first run of two or more literal spaces outside character classes: (byte offset,
/// length). A space before a quantifier is not counted.
fn first_run(pattern: &str, v: bool) -> Option<(usize, usize)> {
    let b = pattern.as_bytes();
    let mut i = 0;
    let mut depth = 0u32;
    while i < b.len() {
        let c = b[i];
        if c == b'\\' {
            i += if b.get(i + 1) == Some(&b' ') { 1 } else { 2 };
            continue;
        }
        if depth > 0 {
            if c == b']' {
                depth -= 1;
            } else if c == b'[' && v {
                depth += 1;
            }
            i += 1;
            continue;
        }
        if c == b'[' {
            depth = 1;
            i += 1;
            continue;
        }
        if c == b' ' {
            let s = i;
            while b.get(i) == Some(&b' ') {
                i += 1;
            }
            let mut n = i - s;
            if matches!(b.get(i), Some(b'*' | b'+' | b'?' | b'{')) {
                n -= 1;
            }
            if n >= 2 {
                return Some((s, n));
            }
            continue;
        }
        i += 1;
    }
    None
}

impl Rule for NoRegexSpaces {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::RegexLit, NodeKind::Call, NodeKind::New]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        // (pattern, flags, start of the pattern text in the source when it can be fixed)
        let (pattern, flags, fix_base) = if let Some((p, f)) = regex_parts(ast, node) {
            (p, f.to_owned(), Some(cx.span(node).start + 1))
        } else {
            let Some(args) = regexp_call_args(cx, node, false, false) else {
                return;
            };
            let Some(&p) = args.first() else { return };
            let p = skip_parens(ast, p);
            let Node::StringLit { value } = *ast.node(p) else {
                return;
            };
            let flags = match args.get(1) {
                None => String::new(),
                Some(&f) => match *ast.node(skip_parens(ast, f)) {
                    Node::StringLit { value } => ast.atom(value).to_owned(),
                    _ => return,
                },
            };
            let value = ast.atom(value);
            let raw = cx.text(p);
            let base =
                (raw.len() >= 2 && &raw[1..raw.len() - 1] == value).then(|| cx.span(p).start + 1);
            (value, flags, base)
        };
        if regex::parse(pattern, Flags::new(&flags)).is_err() {
            return;
        }
        let Some((start, n)) = first_run(pattern, flags.contains('v')) else {
            return;
        };
        if regex_parts(ast, node).is_none() && !cx.text(node).contains(&" ".repeat(n)) {
            // The spaces come from escapes in the string, not literal spaces.
            return;
        }
        let fix = fix_base.map(|base| {
            let s = base + start as u32;
            Fix::replace(Span::new(s, s + n as u32), format!(" {{{n}}}"))
        });
        cx.report_node(node, format!("Spaces are hard to count. Use {{{n}}}."))
            .id("multipleSpaces")
            .fix_opt(fix);
    }
}
