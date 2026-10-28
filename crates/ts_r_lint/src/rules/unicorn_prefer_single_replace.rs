//! `unicorn/prefer-single-replace`: a chain of two or more `.replace()`/`.replaceAll()`
//! calls that each replace one character everywhere with the same static string becomes
//! one `.replaceAll(/[…]/g, replacement)`. A step counts when it is `replaceAll` with a
//! one-unit static string, or either method with a `g` regex (only `g`, `m`, `s` flags)
//! matching one literal character, alone or as a one-character class. Only the outermost
//! call is reported. It is left alone when the replacement uses `` $` `` or `$'`, contains
//! a searched character, or uses `$&` while a character repeats. The class escapes `\`,
//! `]`, line terminators and controls, a leading `^` and an inner `-`; a comment outside
//! the receiver and the replacement blocks the fix.

use super::prelude::*;
use super::unicorn_ast::method_call;
use super::unicorn_text::{
    member_object_text, paren_span, paren_text, static_string_value, would_remove_comments,
};
use super::unicorn_util::{space_around_keyword, with_edits};
use crate::regex::{Flags, RKind, parse};
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-single-replace",
        "Enforce combining multiple single-character replacements into a single `String#replaceAll()` with a regular expression.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<PreferSingleReplace>,
};

#[derive(Debug, Default)]
struct PreferSingleReplace;

fn static_string(ast: &Ast, id: NodeId) -> Option<String> {
    static_string_value(ast, skip_parens(ast, id))
}

/// The one character a `g` regex literal matches.
fn regex_character(pattern: &str, flags: &str) -> Option<char> {
    if !flags.contains('g') || !flags.chars().all(|f| matches!(f, 'g' | 'm' | 's')) {
        return None;
    }
    let re = parse(pattern, Flags::new(flags)).ok()?;
    let root = re.node(re.root);
    let [alt] = root.children[..] else {
        return None;
    };
    let [term] = re.node(alt).children[..] else {
        return None;
    };
    let value = match &re.node(term).kind {
        RKind::Char { value } => *value,
        RKind::Class { negate: false } => {
            let [c] = re.node(term).children[..] else {
                return None;
            };
            match re.node(c).kind {
                RKind::Char { value } => value,
                _ => return None,
            }
        }
        _ => return None,
    };
    char::from_u32(value)
}

/// The character a `replace`/`replaceAll` call replaces everywhere.
fn character(cx: &Ctx<'_, '_>, call: NodeId) -> Option<char> {
    let ast = cx.ast();
    let m = method_call(ast, call)?;
    if !m.plain() || !matches!(m.name, "replace" | "replaceAll") || m.args.len() != 2 {
        return None;
    }
    if m.args
        .iter()
        .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
    {
        return None;
    }
    let search = skip_parens(ast, m.args[0]);
    if let Some(s) = static_string(ast, search) {
        if m.name != "replaceAll" || s.encode_utf16().count() != 1 {
            return None;
        }
        return s.chars().next();
    }
    match *ast.node(search) {
        Node::RegexLit { pattern, flags } => regex_character(ast.atom(pattern), ast.atom(flags)),
        _ => None,
    }
}

fn class_escape(c: char, first: bool, last: bool) -> String {
    match c {
        '\\' => "\\\\".into(),
        ']' => "\\]".into(),
        '\n' => "\\n".into(),
        '\r' => "\\r".into(),
        '\t' => "\\t".into(),
        '\u{c}' => "\\f".into(),
        '\u{b}' => "\\v".into(),
        '\u{2028}' => "\\u2028".into(),
        '\u{2029}' => "\\u2029".into(),
        '^' if first => "\\^".into(),
        '-' if !first && !last => "\\-".into(),
        c if (c as u32) < 0x20 || c as u32 == 0x7f => format!("\\x{:02X}", c as u32),
        c => c.to_string(),
    }
}

impl Rule for PreferSingleReplace {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        if character(cx, node).is_none() {
            return;
        }
        let Some(m) = method_call(ast, node) else {
            return;
        };
        let replacement = m.args[1];
        let Some(value) = static_string(ast, replacement) else {
            return;
        };
        if value.contains("$`") || value.contains("$'") {
            return;
        }
        let mergeable = |call: NodeId| {
            character(cx, call).is_some()
                && method_call(ast, call)
                    .and_then(|m| static_string(ast, m.args[1]))
                    .as_deref()
                    == Some(value.as_str())
        };
        // Only the outermost call of the chain.
        if let Some(member) = parent_skip_parens(ast, cx.file.parents, node)
            && matches!(*ast.node(member), Node::Member { obj, .. } if skip_parens(ast, obj) == node)
            && let Some(outer) = parent_skip_parens(ast, cx.file.parents, member)
            && matches!(*ast.node(outer), Node::Call { callee, .. } if skip_parens(ast, callee) == member)
            && mergeable(outer)
        {
            return;
        }
        let mut chars = Vec::new();
        let mut current = node;
        for _ in 0..100_000 {
            if !mergeable(current) {
                break;
            }
            let (Some(c), Some(m)) = (character(cx, current), method_call(ast, current)) else {
                break;
            };
            chars.push(c);
            current = skip_parens(ast, m.obj);
        }
        if chars.len() < 2 {
            return;
        }
        chars.reverse();
        let mut unique: Vec<char> = Vec::new();
        for &c in &chars {
            if !unique.contains(&c) {
                unique.push(c);
            }
        }
        if unique.iter().any(|&c| value.contains(c)) {
            return;
        }
        if unique.len() != chars.len() && value.contains("$&") {
            return;
        }
        let span = cx.span(node);
        let fix = (!would_remove_comments(
            cx,
            span,
            &[paren_span(cx, current), paren_span(cx, replacement)],
        ))
        .then(|| {
            let class: String = unique
                .iter()
                .enumerate()
                .map(|(i, &c)| class_escape(c, i == 0, i + 1 == unique.len()))
                .collect();
            let base = member_object_text(cx, current, node);
            let text = format!(
                "{base}.replaceAll(/[{class}]/g, {})",
                paren_text(cx, replacement)
            );
            with_edits(Fix::replace(span, text), space_around_keyword(cx, node))
        });
        cx.report(
            span,
            "Combine these single-character replacements into a single `replaceAll()` with a regular expression.",
        )
        .id("prefer-single-replace")
        .fix_opt(fix);
    }
}
