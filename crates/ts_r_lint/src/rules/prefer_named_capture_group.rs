//! `prefer-named-capture-group`: unnamed capture groups in regex literals and `RegExp` calls.

use super::prefer_regex_literals::is_global_regexp;
use super::prelude::*;
use super::unicorn_static::static_value;
use crate::regex;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "prefer-named-capture-group",
        "Enforce using named capture group in regular expression",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<PreferNamedCaptureGroup>,
};

#[derive(Debug, Default)]
struct PreferNamedCaptureGroup;

/// `getStringIfConstant` without a scope: identifiers are unknown.
fn string_if_constant(cx: &Ctx<'_, '_>, n: NodeId) -> Option<String> {
    let ast = cx.ast();
    let mut stack = vec![n];
    while let Some(x) = stack.pop() {
        if matches!(cx.node(x), Node::Ident { .. }) {
            return None;
        }
        ast.for_each_child(x, |c| stack.push(c));
    }
    static_value(cx, n)?.to_js_string()
}

fn check(cx: &mut Ctx<'_, '_>, pattern: &str, flags: &str, node: NodeId, regex_node: NodeId) {
    let ast = cx.ast();
    let Ok(re) = regex::parse(pattern, regex::Flags::new(flags)) else {
        return;
    };
    let regex_node = skip_parens(ast, regex_node);
    let raw = cx.text(regex_node);
    let suggestible = match *cx.node(regex_node) {
        Node::RegexLit { .. } => true,
        Node::StringLit { .. } => !raw.contains('\\'),
        Node::TemplateLit { parts } => {
            ast.list(parts).len() == 1 && raw.get(1..raw.len().saturating_sub(1)) == Some(pattern)
        }
        _ => false,
    };
    let span = ast.span(node);
    let base = ast.span(regex_node).start;
    for &g in &re.captures {
        if matches!(
            &re.node(g).kind,
            regex::RKind::Capture { name: Some(_), .. }
        ) {
            continue;
        }
        let text = re.text(g).to_owned();
        let start = base + re.byte_range(g).0 + 2;
        let r = cx
            .report(
                span,
                format!(
                    "Capture group '{text}' should be converted to a named or non-capturing group."
                ),
            )
            .id("required");
        if suggestible {
            let highest = pattern
                .match_indices("temp")
                .filter_map(|(i, _)| {
                    let digits: String = pattern[i + 4..]
                        .chars()
                        .take_while(char::is_ascii_digit)
                        .collect();
                    digits.parse::<u64>().ok()
                })
                .max()
                .unwrap_or(0);
            r.suggest(
                "addGroupName",
                "Add name to capture group.",
                Fix::insert(start, format!("?<temp{}>", highest + 1)),
            )
            .suggest(
                "addNonCapture",
                "Convert group to non-capturing.",
                Fix::insert(start, "?:"),
            );
        }
    }
}

impl Rule for PreferNamedCaptureGroup {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::RegexLit, NodeKind::New, NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        match *cx.node(node) {
            Node::RegexLit { pattern, flags } => {
                check(cx, ast.atom(pattern), ast.atom(flags), node, node);
            }
            Node::New { callee, args, .. } | Node::Call { callee, args, .. } => {
                if !is_global_regexp(cx, callee) {
                    return;
                }
                let args = ast.list(args);
                let Some(&first) = args.first() else { return };
                let Some(pattern) = string_if_constant(cx, first).filter(|p| !p.is_empty()) else {
                    return;
                };
                let flags = args
                    .get(1)
                    .and_then(|&f| string_if_constant(cx, f))
                    .unwrap_or_default();
                check(cx, &pattern, &flags, node, first);
            }
            _ => {}
        }
    }
}
