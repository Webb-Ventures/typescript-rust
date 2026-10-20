//! `no-control-regex`: disallow control characters in regular expressions.

use super::prelude::*;
use crate::regex::{self, Flags, RKind};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-control-regex",
        "Disallow control characters in regular expressions",
        ESLINT_REC,
        Category::Correctness,
    ),
    create: create_default::<NoControlRegex>,
};

#[derive(Debug, Default)]
struct NoControlRegex;

/// Flags that are well-formed (known letters, no duplicates, not both `u` and `v`).
pub(crate) fn valid_flags(flags: &str) -> bool {
    flags
        .char_indices()
        .all(|(i, c)| "dgimsuvy".contains(c) && !flags[..i].contains(c))
        && !(flags.contains('u') && flags.contains('v'))
}

fn control_chars(pattern: &str, flags: &str) -> Vec<String> {
    let Ok(r) = regex::parse(pattern, Flags::new(flags)) else {
        return Vec::new();
    };
    let mut found: Vec<(u32, String)> = Vec::new();
    for (i, n) in r.nodes.iter().enumerate() {
        let RKind::Char { value } = n.kind else {
            continue;
        };
        if value >= 0x20 {
            continue;
        }
        let raw = r.text(i as u32);
        let literal =
            raw.chars().count() == 1 && raw.chars().next().map(|c| c as u32) == Some(value);
        if raw.starts_with("\\x") || raw.starts_with("\\u") || literal {
            found.push((n.start, format!("\\x{value:02x}")));
        }
    }
    found.sort_by_key(|f| f.0);
    found.into_iter().map(|f| f.1).collect()
}

impl Rule for NoControlRegex {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::RegexLit, NodeKind::Call, NodeKind::New]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let (target, found) = if let Some((pattern, flags)) = regex_parts(ast, node) {
            (node, control_chars(pattern, flags))
        } else {
            let Some(args) = regexp_call_args(cx, node, false, false) else {
                return;
            };
            let Some(&p) = args.first() else { return };
            let Node::StringLit { value } = *ast.node(skip_parens(ast, p)) else {
                return;
            };
            let flags = match args.get(1).map(|&f| static_value_string(ast, cx.src(), f)) {
                None => String::new(),
                Some(Some(f)) => f.into_owned(),
                Some(None) => String::new(),
            };
            if !valid_flags(&flags) {
                return;
            }
            (p, control_chars(ast.atom(value), &flags))
        };
        if found.is_empty() {
            return;
        }
        cx.report_node(
            target,
            format!(
                "Unexpected control character(s) in regular expression: {}.",
                found.join(", ")
            ),
        )
        .id("unexpected");
    }
}
