//! `unicorn/prefer-string-starts-ends-with`: prefix and suffix checks written another way.
//!
//! - `/^abc/.test(x)` and `/abc$/.test(x)` (a regex literal receiver, no `i`/`m` flag, no
//!   `y` flag for `$`, and a remainder free of regex syntax characters) become
//!   `x.startsWith('abc')` / `x.endsWith('abc')`. Skipped when `x` is known not to be a
//!   string. Fixed when `x` is a string (a template, a `String(...)` call or a static
//!   string) or isn't statically known; when it isn't known to be a string, the
//!   suggestions `String(x)`, `x?.` (not on a template tag) and `(x ?? '')` are offered.
//! - `s.indexOf(t) === 0` (either side, `==`/`!=`/`!==` too) on a string `s`:
//!   `s.startsWith(t)`, fixed only when `t` is a string as well.
//! - `s.slice(0, n) === t` (`n` the length of a static `t`, or `t.length`) and
//!   `s.slice(-n) === t` / `s.slice(-t.length) === t` (a non-empty static `t`): fixed to
//!   `startsWith` / `endsWith` when both sides are strings; a suggestion instead when the
//!   slice is on the right and its receiver may have side effects.
//!
//! The comparison fixes keep `s` and `t`, and are withheld when other comments would be
//! lost.

use super::prelude::*;
use super::unicorn_static::{Static, static_value_no_side_effects};
use super::unicorn_text::{
    has_possible_side_effect, is_parenthesized, member_object_needs_parens, member_object_text,
    needs_semicolon, paren_span, paren_text, would_remove_comments,
};
use super::unicorn_types::{is_known_non_string, is_string};
use super::unicorn_util::is_same_reference;
use super::unicorn_util::{space_around_keyword, with_edits};
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-string-starts-ends-with",
        "Prefer `String#startsWith()` & `String#endsWith()` over regexes, `String#indexOf() === 0`, and slice checks.",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<PreferStringStartsEndsWith>,
};

#[derive(Debug, Default)]
struct PreferStringStartsEndsWith;

impl Rule for PreferStringStartsEndsWith {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call, NodeKind::Binary]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        if matches!(cx.node(node), Node::Call { .. }) {
            check_regex_test(cx, node);
        } else {
            check_index_of(cx, node);
            check_slice(cx, node);
        }
    }
}

/// `x.name(args)` with a plain (non-optional, non-computed) member and a plain call, no
/// spread, and an argument count in `min..=max`: the receiver, property and arguments.
fn plain_method_call<'a>(
    cx: &Ctx<'a, '_>,
    call: NodeId,
    name: &str,
    min: usize,
    max: usize,
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
        optional: false,
    } = *ast.node(skip_parens(ast, callee))
    else {
        return None;
    };
    let args = ast.list(args);
    let ok = ident_name(ast, prop) == Some(name)
        && (min..=max).contains(&args.len())
        && !args
            .iter()
            .any(|&a| matches!(ast.node(a), Node::Spread { .. }));
    ok.then_some((skip_parens(ast, obj), prop, args))
}

fn is_simple_string(s: &str) -> bool {
    !s.contains(['^', '$', '+', '[', '{', '(', '\\', '.', '?', '*', '|'])
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

#[derive(Clone, Copy, PartialEq)]
enum Variant {
    Fix,
    StringCasting,
    OptionalChaining,
    NullishCoalescing,
}

/// `shouldAddParenthesesToLogicalExpressionChild` for a `??` left operand.
fn coalesce_child_needs_parens(ast: &Ast, id: NodeId) -> bool {
    match *ast.node(id) {
        Node::Binary {
            op: BinaryOp::Coalesce,
            ..
        } => false,
        Node::Binary { .. }
        | Node::Await { .. }
        | Node::Conditional { .. }
        | Node::Assign { .. }
        | Node::Arrow { .. }
        | Node::Yield { .. }
        | Node::Seq { .. } => true,
        _ => false,
    }
}

fn check_regex_test(cx: &mut Ctx<'_, '_>, node: NodeId) {
    let ast = cx.ast();
    let Some((regex, prop, &[target])) = plain_method_call(cx, node, "test", 1, 1) else {
        return;
    };
    let Node::RegexLit { pattern, flags } = *ast.node(regex) else {
        return;
    };
    let (pattern, flags) = (ast.atom(pattern), ast.atom(flags));
    if flags.contains(['i', 'm']) {
        return;
    }
    let (starts, string) = if let Some(rest) = pattern.strip_prefix('^')
        && is_simple_string(rest)
    {
        (true, rest)
    } else if let Some(rest) = pattern.strip_suffix('$')
        && !flags.contains('y')
        && is_simple_string(rest)
    {
        (false, rest)
    } else {
        return;
    };
    if is_known_non_string(cx, target) {
        return;
    }
    let inner = skip_parens(ast, target);
    let mut is_target_string = match *ast.node(inner) {
        Node::TemplateLit { .. } => true,
        Node::Call { callee, .. } => {
            ident_name(ast, callee) == Some("String")
                && matches!(ast.node(callee), Node::Ident { .. })
        }
        _ => false,
    };
    let mut is_non_string = false;
    if !is_target_string && let Some(v) = static_value_no_side_effects(cx, target) {
        is_target_string = matches!(v, Static::Str(_));
        is_non_string = !is_target_string;
    }
    let method = if starts { "startsWith" } else { "endsWith" };
    let replacement = quote(string);
    let build = |variant: Variant| -> Fix {
        let mut text = paren_text(cx, inner).to_owned();
        let regex_parens = is_parenthesized(cx, regex);
        let target_parens = is_parenthesized(cx, inner);
        let mut edits = Vec::new();
        match variant {
            Variant::NullishCoalescing => {
                if !target_parens && coalesce_child_needs_parens(ast, inner) {
                    text = format!("({text})");
                }
                text.push_str(" ?? ''");
                if !regex_parens {
                    text = format!("({text})");
                }
            }
            Variant::StringCasting => text = format!("String({text})"),
            Variant::OptionalChaining | Variant::Fix => {
                if variant == Variant::OptionalChaining {
                    let dot = skip_trivia_backward(ast, cx.src(), cx.span(prop).start as usize);
                    let dot = dot.saturating_sub(1) as u32;
                    edits.push((Span::new(dot, dot + 1), "?.".to_owned()));
                }
                if !regex_parens && !target_parens && member_object_needs_parens(cx, inner, regex) {
                    text = format!("({text})");
                }
            }
        }
        edits.push((cx.span(regex), text));
        edits.extend(space_around_keyword(cx, regex));
        edits.push((cx.span(prop), method.to_owned()));
        edits.push((paren_span(cx, inner), replacement.clone()));
        with_edits(Fix { edits: Vec::new() }, edits)
    };
    let fix = (is_target_string || !is_non_string).then(|| build(Variant::Fix));
    let mut suggestions = Vec::new();
    if !is_target_string {
        let template_tag = !is_parenthesized(cx, node)
            && cx.parent(node).is_some_and(
                |p| matches!(*ast.node(p), Node::TaggedTemplate { tag, .. } if tag == node),
            );
        suggestions.push((
            "useStringCasting",
            format!("Convert to string `String(…).{method}()`."),
            build(Variant::StringCasting),
        ));
        if !template_tag {
            suggestions.push((
                "useOptionalChaining",
                format!("Use optional chaining `…?.{method}()`."),
                build(Variant::OptionalChaining),
            ));
        }
        suggestions.push((
            "useNullishCoalescing",
            format!("Use nullish coalescing `(… ?? '').{method}()`."),
            build(Variant::NullishCoalescing),
        ));
    }
    let (id, msg) = if starts {
        (
            "prefer-starts-with",
            "Prefer `String#startsWith()` over a regex with `^`.",
        )
    } else {
        (
            "prefer-ends-with",
            "Prefer `String#endsWith()` over a regex with `$`.",
        )
    };
    let mut report = cx.report_node(node, msg).id(id).fix_opt(fix);
    for (id, desc, fix) in suggestions {
        report = report.suggest(id, desc, fix);
    }
}

fn is_equality(op: BinaryOp) -> bool {
    matches!(
        op,
        BinaryOp::StrictEq | BinaryOp::StrictNotEq | BinaryOp::Eq | BinaryOp::NotEq
    )
}

fn is_negated(op: BinaryOp) -> bool {
    matches!(op, BinaryOp::StrictNotEq | BinaryOp::NotEq)
}

fn is_zero(ast: &Ast, id: NodeId) -> bool {
    matches!(*ast.node(skip_parens(ast, id)), Node::NumberLit { value } if value.get() == 0.0)
}

/// `[!]target.method(search)` in place of `node`, or `None` when comments would be lost.
fn comparison_fix(
    cx: &Ctx<'_, '_>,
    node: NodeId,
    op: BinaryOp,
    target: NodeId,
    search: NodeId,
    method: &str,
) -> Option<Fix> {
    let kept = [paren_span(cx, target), paren_span(cx, search)];
    if would_remove_comments(cx, cx.span(node), &kept) {
        return None;
    }
    let negated = is_negated(op);
    let target_text = member_object_text(cx, target, if negated { target } else { node });
    let mut replacement = format!(
        "{}{target_text}.{method}({})",
        if negated { "!" } else { "" },
        paren_text(cx, skip_parens(cx.ast(), search))
    );
    if needs_semicolon(cx, cx.span(node).start, &replacement) {
        replacement.insert(0, ';');
    }
    Some(with_edits(
        Fix::replace(cx.span(node), replacement),
        space_around_keyword(cx, node),
    ))
}

fn check_index_of(cx: &mut Ctx<'_, '_>, node: NodeId) {
    let ast = cx.ast();
    let Node::Binary { left, op, right } = *ast.node(node) else {
        return;
    };
    if !is_equality(op) {
        return;
    }
    let call = match (
        plain_method_call(cx, left, "indexOf", 1, 1),
        plain_method_call(cx, right, "indexOf", 1, 1),
    ) {
        (Some(c), _) if is_zero(ast, right) => c,
        (_, Some(c)) if is_zero(ast, left) => c,
        _ => return,
    };
    let (target, _, &[search]) = call else {
        return;
    };
    if !is_string(cx, target) {
        return;
    }
    let fix = if is_string(cx, search) {
        comparison_fix(cx, node, op, target, search, "startsWith")
    } else {
        None
    };
    cx.report_node(
        node,
        "Prefer `String#startsWith()` over `String#indexOf() === 0`.",
    )
    .id("prefer-starts-with-indexOf")
    .fix_opt(fix);
}

/// A number literal, or a negated one.
fn numeric_value(ast: &Ast, id: NodeId) -> Option<f64> {
    match *ast.node(skip_parens(ast, id)) {
        Node::NumberLit { value } => Some(value.get()),
        Node::Unary {
            op: UnaryOp::Minus,
            arg,
        } => match *ast.node(skip_parens(ast, arg)) {
            Node::NumberLit { value } => Some(-value.get()),
            _ => None,
        },
        _ => None,
    }
}

/// `search.length` (non-computed `length` on the same reference).
fn is_length_of(cx: &Ctx<'_, '_>, id: NodeId, search: NodeId) -> bool {
    let ast = cx.ast();
    matches!(*ast.node(skip_parens(ast, id)), Node::Member { obj, prop, .. }
        if ident_name(ast, prop) == Some("length") && is_same_reference(cx, obj, search))
}

fn check_slice(cx: &mut Ctx<'_, '_>, node: NodeId) {
    let ast = cx.ast();
    let Node::Binary { left, op, right } = *ast.node(node) else {
        return;
    };
    if !is_equality(op) {
        return;
    }
    let (call, slice_on_right, search) = if let Some(c) = plain_method_call(cx, left, "slice", 1, 2)
    {
        (c, false, right)
    } else if let Some(c) = plain_method_call(cx, right, "slice", 1, 2) {
        (c, true, left)
    } else {
        return;
    };
    let (target, _, args) = call;
    if !is_string(cx, target) || !is_string(cx, search) {
        return;
    }
    let search_len = match static_value_no_side_effects(cx, search) {
        Some(Static::Str(s)) => Some(s.encode_utf16().count() as f64),
        _ => None,
    };
    let starts = match *args {
        [first, second]
            if is_zero(ast, first)
                && matches!(*ast.node(skip_parens(ast, first)), Node::NumberLit { .. })
                && ((search_len.is_some() && numeric_value(ast, second) == search_len)
                    || is_length_of(cx, second, search)) =>
        {
            true
        }
        [first]
            if search_len.is_some_and(|n| n > 0.0)
                && (numeric_value(ast, first) == search_len.map(|n| -n)
                    || matches!(*ast.node(skip_parens(ast, first)), Node::Unary { op: UnaryOp::Minus, arg }
                        if is_length_of(cx, arg, search))) =>
        {
            false
        }
        _ => return,
    };
    let method = if starts { "startsWith" } else { "endsWith" };
    let fix = comparison_fix(cx, node, op, target, search, method);
    let suggest = slice_on_right && has_possible_side_effect(cx, target);
    let (id, msg) = if starts {
        (
            "prefer-starts-with-slice",
            "Prefer `String#startsWith()` over comparing a string slice.",
        )
    } else {
        (
            "prefer-ends-with-slice",
            "Prefer `String#endsWith()` over comparing a string slice.",
        )
    };
    let report = cx.report_node(node, msg).id(id);
    match fix {
        Some(fix) if suggest => {
            report.suggest(
                "prefer-string-starts-ends-with/slice-suggestion",
                format!("Replace with `String#{method}()`."),
                fix,
            );
        }
        fix => {
            report.fix_opt(fix);
        }
    }
}
