//! `unicorn/no-unnecessary-fetch-options`: options of the global `fetch(input, init)` and
//! `new Request(input, init)` that change nothing.
//!
//! An empty options literal as the last argument is reported. Otherwise, when every key
//! of the literal is static and unique, a request-init key whose value is statically
//! `undefined`, a `body` of `null`, and, when the input is surely not a `Request` (a string
//! literal, a substitution-free template, `new URL(...)`), empty `headers` (`{}`, `[]`,
//! `new Headers()`) and a default value (`method: "GET"` in any case, `mode: "cors"`, ...)
//! are reported. Values are evaluated as unicorn does for these checks (control flow for
//! branches, no side effects when a member could have changed).
//!
//! Fixes: when all properties go (none with side effects, getters considered, or reading a
//! name declared after it, no comments in the literal) the first report removes the whole
//! argument (with its comma; `{}` when more arguments follow); otherwise each property is
//! removed with its line when it is alone on it, inline with its comma, or token by token.

use super::prelude::*;
use super::unicorn_static::{
    Static, has_side_effect_with_getters, has_side_effectful_const_initializer,
    mutable_member_access, static_value, static_value_control_flow, static_value_no_side_effects,
};
use super::unicorn_text::has_comment_in;
use super::unicorn_util::skip_wrappers;
use ts_r_lexer::{Lexer, TokenKind};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-unnecessary-fetch-options",
        "Disallow unnecessary options in `fetch()` and `new Request()`.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<NoUnnecessaryFetchOptions>,
};

#[derive(Debug, Default)]
struct NoUnnecessaryFetchOptions;

const REQUEST_INIT: &[&str] = &[
    "attributionReporting",
    "body",
    "browsingTopics",
    "cache",
    "credentials",
    "duplex",
    "headers",
    "integrity",
    "keepalive",
    "method",
    "mode",
    "priority",
    "redirect",
    "referrer",
    "referrerPolicy",
    "signal",
    "window",
];

const DEFAULTS: &[(&str, &str)] = &[
    ("cache", "default"),
    ("credentials", "same-origin"),
    ("integrity", ""),
    ("method", "GET"),
    ("mode", "cors"),
    ("redirect", "follow"),
    ("referrer", "about:client"),
    ("referrerPolicy", ""),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Input {
    Request,
    NonRequest,
    Unknown,
}

fn is_branch(ast: &ts_r_ast::Ast, id: NodeId) -> bool {
    let mut node = skip_wrappers(ast, id);
    for _ in 0..1000 {
        match *ast.node(node) {
            Node::Unary { arg, .. } => node = skip_wrappers(ast, arg),
            Node::Conditional { .. }
            | Node::Binary {
                op: BinaryOp::LogicalAnd | BinaryOp::LogicalOr | BinaryOp::Coalesce,
                ..
            } => return true,
            _ => return false,
        }
    }
    false
}

fn static_of(cx: &Ctx<'_, '_>, id: NodeId) -> Option<Static> {
    let ast = cx.ast();
    let node = skip_wrappers(ast, id);
    let branch = is_branch(ast, node);
    if branch {
        static_value_control_flow(cx, node)
    } else if mutable_member_access(cx, node, &mut Vec::new())
        || has_side_effectful_const_initializer(cx, node)
    {
        static_value_no_side_effects(cx, node)
    } else {
        static_value(cx, node)
    }
}

fn is_global_named(cx: &Ctx<'_, '_>, id: NodeId, name: &str) -> bool {
    let id = skip_parens(cx.ast(), id);
    matches!(cx.node(id), Node::Ident { .. })
        && ident_name(cx.ast(), id) == Some(name)
        && cx.is_global_ref(id, name)
}

fn input_state(cx: &Ctx<'_, '_>, input: NodeId) -> Input {
    let ast = cx.ast();
    let input = skip_wrappers(ast, input);
    match *ast.node(input) {
        Node::StringLit { .. } => Input::NonRequest,
        Node::TemplateLit { parts } if ast.list(parts).len() == 1 => Input::NonRequest,
        Node::New { callee, args, .. } if !ast.list(args).is_empty() => {
            if is_global_named(cx, callee, "URL") {
                Input::NonRequest
            } else if is_global_named(cx, callee, "Request") {
                Input::Request
            } else {
                Input::Unknown
            }
        }
        _ => Input::Unknown,
    }
}

/// A property of the options literal: (key node to report, value, computed key expression).
struct Prop {
    node: NodeId,
    key: NodeId,
    value: Option<NodeId>,
    computed: Option<NodeId>,
}

fn prop_parts(cx: &Ctx<'_, '_>, p: NodeId) -> Option<Prop> {
    let ast = cx.ast();
    let (key, value) = match *ast.node(p) {
        Node::Property { key, value, .. } => (key, Some(value)),
        Node::ShorthandProp { name, .. } => (name, Some(name)),
        Node::MethodDef { key, value, .. } => (key, Some(value)),
        _ => return None,
    };
    let computed = match *ast.node(key) {
        Node::ComputedKey { expr } => Some(expr),
        _ => None,
    };
    Some(Prop {
        node: p,
        key: computed.unwrap_or(key),
        value,
        computed,
    })
}

fn prop_name(cx: &Ctx<'_, '_>, prop: &Prop) -> Option<String> {
    let ast = cx.ast();
    if let Some(expr) = prop.computed {
        return static_value(cx, expr)?.to_js_string();
    }
    match *ast.node(prop.key) {
        Node::NumberLit { .. } => static_value(cx, prop.key)?.to_js_string(),
        _ => key_name(ast, cx.src(), prop.key).map(|k| k.into_owned()),
    }
}

fn is_empty_headers(cx: &Ctx<'_, '_>, value: NodeId) -> bool {
    let ast = cx.ast();
    match *ast.node(skip_wrappers(ast, value)) {
        Node::ObjectLit { props } => ast.list(props).is_empty(),
        Node::ArrayLit { elements } => ast.list(elements).is_empty(),
        Node::New { callee, args, .. } => {
            ast.list(args).is_empty() && is_global_named(cx, callee, "Headers")
        }
        _ => false,
    }
}

fn is_unnecessary(cx: &Ctx<'_, '_>, prop: &Prop, name: &str, input: Input) -> bool {
    let Some(value) = prop.value else {
        return false;
    };
    if matches!(cx.node(prop.node), Node::MethodDef { .. }) {
        return false;
    }
    let v = static_of(cx, value);
    if REQUEST_INIT.contains(&name) && matches!(v, Some(Static::Undefined)) {
        return true;
    }
    if name == "body" && matches!(v, Some(Static::Null)) {
        return true;
    }
    if input != Input::NonRequest {
        return false;
    }
    if name == "headers" && is_empty_headers(cx, value) {
        return true;
    }
    if name == "keepalive" {
        return matches!(v, Some(Static::Bool(false)));
    }
    match (DEFAULTS.iter().find(|(n, _)| *n == name), v) {
        (Some((_, d)), Some(Static::Str(s))) => {
            if name == "method" {
                s.to_uppercase() == *d
            } else {
                s == *d
            }
        }
        _ => false,
    }
}

/// A name inside `id` that reads a binding declared after it.
fn reads_later_declaration(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let span = cx.span(skip_wrappers(cx.ast(), id));
    let sem = cx.semantic();
    sem.references().iter().any(|r| {
        let at = cx.span(r.node);
        if at.start < span.start || at.end > span.end {
            return false;
        }
        r.symbol()
            .and_then(|s| sem.symbol(s).decls.first().copied())
            .is_some_and(|d| cx.span(d.name).start > at.start)
    })
}

fn is_unsafe_to_remove(cx: &Ctx<'_, '_>, prop: &Prop) -> bool {
    let ast = cx.ast();
    let side_effect = |id: NodeId| has_side_effect_with_getters(ast, skip_wrappers(ast, id));
    prop.value
        .is_some_and(|v| side_effect(v) || reads_later_declaration(cx, v))
        || prop
            .computed
            .is_some_and(|k| side_effect(k) || reads_later_declaration(cx, k))
}

fn is_line_break(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

fn line_start(src: &str, pos: usize) -> usize {
    src[..pos]
        .char_indices()
        .rev()
        .find(|&(_, c)| is_line_break(c))
        .map_or(0, |(i, c)| i + c.len_utf8())
}

fn line_end(src: &str, pos: usize) -> usize {
    src[pos..]
        .char_indices()
        .find(|&(_, c)| is_line_break(c))
        .map_or(src.len(), |(i, _)| pos + i)
}

fn same_line(src: &str, a: usize, b: usize) -> bool {
    let (a, b) = if a <= b { (a, b) } else { (b, a) };
    !src[a..b].chars().any(is_line_break)
}

fn is_blank(s: &str) -> bool {
    s.chars()
        .all(|c| super::unicorn_ast::is_js_space(c) || c == '\u{feff}')
}

/// The next token's start at or after `pos` (comments skipped).
fn next_token(src: &str, pos: usize) -> usize {
    skip_trivia_forward(src, pos)
}

/// Removing the property with its whole line.
fn line_removal(cx: &Ctx<'_, '_>, prop: NodeId, is_last: bool) -> Option<Span> {
    let src = cx.src();
    let span = cx.span(prop);
    let (start, end) = (span.start as usize, span.end as usize);
    if !same_line(src, start, end) {
        return None;
    }
    let ls = line_start(src, start);
    if !is_blank(&src[ls..start]) {
        return None;
    }
    let after = next_token(src, end);
    let comma_on_line = src[after..].starts_with(',') && same_line(src, end, after);
    if !is_last && !comma_on_line {
        return None;
    }
    let end_col = if comma_on_line { after + 1 } else { end };
    let le = line_end(src, end_col);
    if !is_blank(&src[end_col..le]) {
        return None;
    }
    let next = if src[le..].starts_with("\r\n") {
        le + 2
    } else {
        le + src[le..].chars().next().map_or(0, char::len_utf8)
    };
    Some(Span::new(ls as u32, next as u32))
}

/// Removing the property inline, with one comma.
fn inline_removal(cx: &Ctx<'_, '_>, prop: NodeId, is_last: bool) -> Option<Span> {
    let src = cx.src();
    let span = cx.span(prop);
    let (start, end) = (span.start as usize, span.end as usize);
    if !same_line(src, start, end) {
        return None;
    }
    let next = next_token(src, end);
    if is_last {
        let prev_end = skip_trivia_backward(cx.ast(), src, start);
        if !same_line(src, prev_end, start) {
            return None;
        }
        let prev_start = prev_end.checked_sub(1)?;
        let end_token = if src[next..].starts_with(',') {
            next_token(src, next + 1)
        } else {
            next
        };
        let stop = if same_line(src, end, end_token) {
            end_token
        } else {
            end
        };
        return Some(Span::new(prev_start as u32, stop as u32));
    }
    if !same_line(src, end, next) || !src[next..].starts_with(',') {
        return None;
    }
    let after_comma = next_token(src, next + 1);
    if after_comma < src.len() && same_line(src, end, after_comma) {
        return Some(Span::new(start as u32, after_comma as u32));
    }
    let mut s = start;
    while s > 0 {
        let Some(c) = src[..s].chars().next_back() else {
            break;
        };
        if c.is_whitespace() && !matches!(c, '\n' | '\r') {
            s -= c.len_utf8();
        } else {
            break;
        }
    }
    Some(Span::new(s as u32, (next + 1) as u32))
}

/// Removing the property's tokens (the spaces between them stay) and one comma.
fn token_removal(cx: &Ctx<'_, '_>, prop: NodeId, is_last: bool, count: usize) -> Fix {
    let src = cx.src();
    let span = cx.span(prop);
    let mut runs: Vec<Span> = Vec::new();
    let mut lexer = Lexer::new(src);
    lexer.seek(span.start);
    let mut ok = true;
    for _ in 0..100_000 {
        let kind = lexer.next_token();
        let tok = lexer.token();
        if kind == TokenKind::Eof || tok.start >= span.end {
            break;
        }
        if matches!(
            kind,
            TokenKind::TemplateHead | TokenKind::Slash | TokenKind::SlashEq
        ) || tok.end > span.end
        {
            ok = false;
            break;
        }
        match runs.last_mut() {
            Some(last) if last.end == tok.start => last.end = tok.end,
            _ => runs.push(tok.span()),
        }
    }
    if !ok || runs.is_empty() {
        runs = vec![span];
    }
    let mut edits: Vec<Span> = runs;
    let after = next_token(src, span.end as usize);
    if src[after..].starts_with(',') {
        edits.push(Span::new(after as u32, after as u32 + 1));
    } else if count > 1 && is_last {
        let before = skip_trivia_backward(cx.ast(), src, span.start as usize);
        if before > 0 && src[..before].ends_with(',') {
            edits.insert(0, Span::new(before as u32 - 1, before as u32));
        }
    }
    let mut it = edits.into_iter();
    let first = it.next().unwrap_or(span);
    let mut fix = Fix::delete(first);
    for s in it {
        fix = fix.and(s, "");
    }
    fix
}

impl NoUnnecessaryFetchOptions {
    fn check(&self, cx: &mut Ctx<'_, '_>, args: &[NodeId]) {
        let ast = cx.ast();
        let (input, argument) = (args[0], args[1]);
        let options = skip_wrappers(ast, argument);
        let Node::ObjectLit { props } = *ast.node(options) else {
            return;
        };
        let props = ast.list(props);
        let is_last_argument = args.len() == 2;
        let src = cx.src();

        // The argument with its comma before, and a trailing comma after.
        let arg_span = super::unicorn_text::paren_span(cx, argument);
        let comma_before = skip_trivia_backward(ast, src, arg_span.start as usize);
        let mut removal = Span::new(comma_before.saturating_sub(1) as u32, arg_span.end);
        let after_arg = next_token(src, arg_span.end as usize);
        let trailing_comma = src[after_arg..].starts_with(',');
        if trailing_comma {
            removal.end = after_arg as u32 + 1;
        }
        let comment_right_after = |pos: usize| {
            let rest =
                src[pos..].trim_start_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
            rest.starts_with("//") || rest.starts_with("/*")
        };
        let can_remove_argument = !(has_comment_in(cx, removal)
            || comment_right_after(arg_span.end as usize)
            || trailing_comma && comment_right_after(after_arg + 1));
        let remove_argument = Fix::delete(removal);

        if props.is_empty() {
            if !is_last_argument {
                return;
            }
            cx.report(cx.span(options), "Remove unnecessary empty fetch options.")
                .id("empty-options")
                .fix_opt(can_remove_argument.then_some(remove_argument));
            return;
        }

        let mut parts = Vec::with_capacity(props.len());
        let mut names: Vec<String> = Vec::with_capacity(props.len());
        for &p in props {
            let Some(part) = prop_parts(cx, p) else {
                return;
            };
            let Some(name) = prop_name(cx, &part) else {
                return;
            };
            if names.contains(&name) {
                return;
            }
            names.push(name);
            parts.push(part);
        }
        let input = input_state(cx, input);
        let unnecessary: Vec<usize> = (0..parts.len())
            .filter(|&i| is_unnecessary(cx, &parts[i], &names[i], input))
            .collect();
        let comments_inside = has_comment_in(cx, cx.span(options));
        let whole = (unnecessary.len() == parts.len()
            && !comments_inside
            && !unnecessary
                .iter()
                .any(|&i| is_unsafe_to_remove(cx, &parts[i])))
        .then(|| {
            if !is_last_argument {
                Some(Fix::replace(cx.span(options), "{}"))
            } else {
                can_remove_argument.then_some(remove_argument)
            }
        })
        .flatten();

        let mut whole = whole;
        for (n, &i) in unnecessary.iter().enumerate() {
            let part = &parts[i];
            let fix = match (n, whole.take()) {
                (0, Some(f)) => Some(f),
                _ => {
                    if comments_inside || is_unsafe_to_remove(cx, part) || parts.len() == 1 {
                        None
                    } else {
                        let is_last = i == parts.len() - 1;
                        Some(
                            line_removal(cx, part.node, is_last)
                                .or_else(|| inline_removal(cx, part.node, is_last))
                                .map(Fix::delete)
                                .unwrap_or_else(|| {
                                    token_removal(cx, part.node, is_last, parts.len())
                                }),
                        )
                    }
                }
            };
            let message = format!("Remove unnecessary `{}` fetch option.", names[i]);
            cx.report(cx.span(skip_parens(ast, part.key)), message)
                .id("property")
                .fix_opt(fix);
        }
    }
}

impl Rule for NoUnnecessaryFetchOptions {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call, NodeKind::New]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let (callee, args, name) = match *ast.node(node) {
            Node::Call {
                callee,
                optional: false,
                args,
                ..
            } => (callee, args, "fetch"),
            Node::New { callee, args, .. } => (callee, args, "Request"),
            _ => return,
        };
        let args = ast.list(args);
        if args.len() < 2 || !is_global_named(cx, callee, name) {
            return;
        }
        self.check(cx, args);
    }
}
