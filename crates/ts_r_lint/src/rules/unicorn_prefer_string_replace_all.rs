//! `unicorn/prefer-string-replace-all`: global-regex replacements and `split().join()`
//! written with `String#replaceAll()`.
//!
//! - `s.replace(re, x)` (two arguments, not an optional call) where `re` is statically a
//!   regex with the `g` flag (a literal, a `new RegExp(...)` of static arguments, or a
//!   variable holding either and never reassigned) is reported on `replace` and fixed by
//!   appending `All`. Skipped on receivers known not to be strings and on the
//!   `replace(/[\^$.*+?()[\]{}|/-]/g, "\\$&")` regex-escape idiom (8+ escapable characters).
//! - A regex literal that matches one fixed string (characters, escapes, single-character
//!   classes, `{1}` quantifiers and non-capturing groups; no `i`/`y` flag, no lone
//!   surrogate under `u`/`v`) is also replaced with that string; on a `replaceAll` call it
//!   is the only report ("This pattern can be replaced with 'x'", or "a string literal"
//!   for 20+ characters).
//! - `s.split(sep).join(rep)` with a static string `rep` and a non-empty static string
//!   `sep`, or a regex literal `sep` that can't match empty and has no capture group (no
//!   `y` flag), becomes `s.replaceAll(sep, rep)` (with `$` doubled in `rep`); no fix when
//!   the call holds a comment.

use super::prelude::*;
use super::unicorn_static::static_regexp;
use super::unicorn_text::{has_comment_in, member_object_text, static_string_value};
use super::unicorn_types::is_known_non_string;
use crate::regex::{self, Flags, RId, RKind, Regex};
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-string-replace-all",
        "Prefer `String#replaceAll()` over regex searches with the global flag and `String#split().join()`.",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<PreferStringReplaceAll>,
};

#[derive(Debug, Default)]
struct PreferStringReplaceAll;

impl Rule for PreferStringReplaceAll {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        if let Some((prop, replacement)) = split_join_replacement(cx, node) {
            let fix = (!has_comment_in(cx, cx.span(node)))
                .then(|| Fix::replace(cx.span(node), replacement));
            cx.report_node(
                prop,
                "Prefer `String#replaceAll()` over `String#split().join()`.",
            )
            .id("split-join")
            .fix_opt(fix);
            return;
        }
        check_replace(cx, node);
    }
}

/// `x.name(args)`: a member callee (not computed) and a non-optional call with `count`
/// non-spread arguments; `plain` also rules out `x?.name`. Receiver, property, arguments.
fn method_call<'a>(
    cx: &Ctx<'a, '_>,
    call: NodeId,
    names: &[&str],
    count: usize,
    plain: bool,
) -> Option<(NodeId, NodeId, &'a [NodeId])> {
    let ast = cx.ast();
    let Node::Call {
        callee,
        args,
        optional: false,
        ..
    } = *ast.node(skip_parens(ast, call))
    else {
        return None;
    };
    let Node::Member {
        obj,
        prop,
        optional,
    } = *ast.node(skip_parens(ast, callee))
    else {
        return None;
    };
    let args = ast.list(args);
    let ok = ident_name(ast, prop).is_some_and(|n| names.contains(&n))
        && !(plain && optional)
        && args.len() == count
        && !args
            .iter()
            .any(|&a| matches!(ast.node(a), Node::Spread { .. }));
    ok.then_some((obj, prop, args))
}

/// A single-quoted JS string literal for `s`.
fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        match c {
            '\'' => out.push_str("\\'"),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

fn regex_literal(ast: &Ast, id: NodeId) -> Option<(&str, &str)> {
    match *ast.node(skip_parens(ast, id)) {
        Node::RegexLit { pattern, flags } => Some((ast.atom(pattern), ast.atom(flags))),
        _ => None,
    }
}

fn parse_regex<'s>(pattern: &'s str, flags: &str) -> Option<Regex<'s>> {
    regex::parse(pattern, Flags::new(flags)).ok()
}

// ---- The regex tree --------------------------------------------------------------------

/// The terms of a node that holds one alternative (`Pattern`, a group), or `None` for a
/// disjunction.
fn single_alternative(re: &Regex<'_>, id: RId) -> Option<Vec<RId>> {
    match re.node(id).children.as_slice() {
        [alt] if matches!(re.kind(*alt), RKind::Alternative) => {
            Some(re.node(*alt).children.clone())
        }
        [] => Some(Vec::new()),
        _ => None,
    }
}

/// The string-literal text for one character as written.
fn char_replacement(re: &Regex<'_>, id: RId, value: u32) -> String {
    let raw = re.text(id);
    let unicode_escape = |v: u32| format!("\\u{{{v:X}}}");
    if let Some(rest) = raw.strip_prefix('\\') {
        let first = rest.chars().next().unwrap_or(' ');
        // `\c` not followed by a letter: the backslash is a literal character (Annex B).
        if rest.is_empty() {
            return "\\\\".to_owned();
        }
        if first == 'c' {
            return match value {
                13 => "\\r".to_owned(),
                10 => "\\n".to_owned(),
                9 => "\\t".to_owned(),
                v => unicode_escape(v),
            };
        }
        if ('0'..='7').contains(&first) {
            return unicode_escape(value);
        }
        if matches!(first, 'x' | 'u' | 'n' | 'r' | 't' | 'f' | 'v' | 'b') {
            return raw.to_owned();
        }
        // An identity escape: the character itself.
        return match rest {
            "'" | "\\" => format!("\\{rest}"),
            _ => rest.to_owned(),
        };
    }
    match raw {
        "'" | "\\" => format!("\\{raw}"),
        _ => raw.to_owned(),
    }
}

fn node_replacement(re: &Regex<'_>, id: RId, depth: u32) -> Option<String> {
    if depth > 200 {
        return None;
    }
    match re.kind(id) {
        RKind::Char { value } => Some(char_replacement(re, id, *value)),
        RKind::Class { negate: false } => match re.node(id).children.as_slice() {
            [c] => match re.kind(*c) {
                RKind::Char { value } => Some(char_replacement(re, *c, *value)),
                _ => None,
            },
            _ => None,
        },
        RKind::Quantifier {
            min: 1,
            max: Some(1),
            ..
        } => body_replacement(re, &re.node(id).children, depth + 1),
        RKind::Group if re.text(id).starts_with("(?:") => {
            body_replacement(re, &single_alternative(re, id)?, depth + 1)
        }
        _ => None,
    }
}

fn body_replacement(re: &Regex<'_>, body: &[RId], depth: u32) -> Option<String> {
    let mut out = String::new();
    for &n in body {
        out.push_str(&node_replacement(re, n, depth)?);
    }
    Some(out)
}

fn has_surrogate(re: &Regex<'_>) -> bool {
    re.nodes
        .iter()
        .any(|n| matches!(n.kind, RKind::Char { value } if (0xD800..=0xDFFF).contains(&value)))
}

/// The fixed string a global regex literal matches, quoted.
fn pattern_replacement(ast: &Ast, id: NodeId) -> Option<String> {
    let (pattern, flags) = regex_literal(ast, id)?;
    let re = parse_regex(pattern, flags)?;
    if flags.contains(['i', 'y']) || (flags.contains(['u', 'v']) && has_surrogate(&re)) {
        return None;
    }
    let replacement = body_replacement(&re, &single_alternative(&re, re.root)?, 0)?;
    (!replacement.is_empty()).then(|| format!("'{replacement}'"))
}

fn minimum_length(re: &Regex<'_>, id: RId, depth: u32) -> u64 {
    if depth > 200 {
        return 0;
    }
    let children = &re.node(id).children;
    let sum = |ids: &[RId]| -> u64 { ids.iter().map(|&c| minimum_length(re, c, depth + 1)).sum() };
    match re.kind(id) {
        RKind::Char { .. } | RKind::Dot | RKind::Class { .. } | RKind::CharSet { .. } => 1,
        RKind::Alternative => sum(children),
        RKind::Pattern | RKind::Group => children
            .iter()
            .map(|&c| minimum_length(re, c, depth + 1))
            .min()
            .unwrap_or(0),
        RKind::Quantifier { min, .. } => u64::from(*min) * sum(children),
        _ => 0,
    }
}

/// `/pattern/g...` for a split separator regex that can't match empty and captures
/// nothing.
fn global_split_regex(ast: &Ast, id: NodeId) -> Option<String> {
    let (pattern, flags) = regex_literal(ast, id)?;
    if flags.contains('y') {
        return None;
    }
    let re = parse_regex(pattern, flags)?;
    if !re.captures.is_empty() || minimum_length(&re, re.root, 0) == 0 {
        return None;
    }
    let flags = if flags.contains('g') {
        flags.to_owned()
    } else {
        format!("g{flags}")
    };
    Some(format!("/{pattern}/{flags}"))
}

// ---- split().join() -------------------------------------------------------------------

fn split_join_replacement(cx: &Ctx<'_, '_>, node: NodeId) -> Option<(NodeId, String)> {
    let ast = cx.ast();
    let (split, prop, &[replacement]) = method_call(cx, node, &["join"], 1, true)? else {
        return None;
    };
    let (receiver, _, &[separator]) = method_call(cx, split, &["split"], 1, true)? else {
        return None;
    };
    if is_known_non_string(cx, receiver) {
        return None;
    }
    let search = match static_string_value(ast, skip_parens(ast, separator)) {
        Some(s) if s.is_empty() => return None,
        Some(s) => quote(&s),
        None => global_split_regex(ast, separator)?,
    };
    let value = static_string_value(ast, skip_parens(ast, replacement))?;
    let text = format!(
        "{}.replaceAll({search}, {})",
        member_object_text(cx, receiver, node),
        quote(&value.replace('$', "$$"))
    );
    Some((prop, text))
}

// ---- replace / replaceAll --------------------------------------------------------------

const ESCAPABLE: &str = "\\^$.*+?()[]{}|/-";

/// The characters of a `[...]` class made only of regex syntax characters.
fn escape_class_size(pattern: &str) -> Option<usize> {
    let inner = pattern.strip_prefix('[')?.strip_suffix(']')?;
    if inner.starts_with('^') {
        return None;
    }
    let chars: Vec<char> = inner.chars().collect();
    let mut seen = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let mut c = chars[i];
        let mut escaped = false;
        if c == '\\' {
            i += 1;
            c = *chars.get(i)?;
            escaped = true;
        }
        if !escaped && ((c == '-' && i != 0 && i != chars.len() - 1) || (c == ']' && i != 0)) {
            return None;
        }
        if !ESCAPABLE.contains(c) {
            return None;
        }
        if !seen.contains(&c) {
            seen.push(c);
        }
        i += 1;
    }
    Some(seen.len())
}

fn is_regexp_escape_call(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let Some((_, _, &[pattern, replacement])) = method_call(cx, node, &["replace"], 2, true) else {
        return false;
    };
    let Some((source, flags)) = regex_literal(ast, pattern) else {
        return false;
    };
    flags.contains('g')
        && !flags.contains('y')
        && escape_class_size(source).is_some_and(|n| n >= 8)
        && static_string_value(ast, skip_parens(ast, replacement)).as_deref() == Some("\\$&")
}

fn check_replace(cx: &mut Ctx<'_, '_>, node: NodeId) {
    let ast = cx.ast();
    let Some((receiver, prop, &[pattern, _])) =
        method_call(cx, node, &["replace", "replaceAll"], 2, false)
    else {
        return;
    };
    if is_regexp_escape_call(cx, node)
        || is_known_non_string(cx, receiver)
        || !static_regexp(cx, pattern).is_some_and(|(_, flags)| flags.contains('g'))
    {
        return;
    }
    let replacement = pattern_replacement(ast, pattern);
    let pattern_span = cx.span(skip_parens(ast, pattern));
    if ident_name(ast, prop) == Some("replaceAll") {
        let Some(replacement) = replacement else {
            return;
        };
        let shown = if replacement.encode_utf16().count() < 20 {
            replacement.clone()
        } else {
            "a string literal".to_owned()
        };
        cx.report_node(
            skip_parens(ast, pattern),
            format!("This pattern can be replaced with {shown}."),
        )
        .id("pattern")
        .fix(Fix::replace(pattern_span, replacement));
        return;
    }
    let mut fix = Fix::insert(cx.span(prop).end, "All");
    if let Some(replacement) = replacement {
        fix = fix.and(pattern_span, replacement);
    }
    cx.report_node(
        prop,
        "Prefer `String#replaceAll()` over `String#replace()`.",
    )
    .id("method")
    .fix(fix);
}
