//! `unicorn/prefer-uint8array-base64`: `Uint8Array#toBase64()` / `Uint8Array.fromBase64()`
//! over `atob()` / `btoa()`, `Buffer` base64 conversions, and hand-made base64url.
//!
//! - `x.toBase64().replaceAll('+', '-').replaceAll('/', '_')` (any order, `replace` with
//!   `/\+/g`-style regexes) and `.replaceAll('=', '')` / `.replace(/=+$/, '')` right after a
//!   bare `toBase64()`: fixed to `x.toBase64({alphabet: 'base64url', omitPadding: true})`
//!   (whichever apply; `+` and `/` only together, each at most once), keeping parentheses.
//! - Calls of the global `atob` / `btoa` (also `window.atob`, aliases): reported.
//! - `Buffer.from(s, 'base64' | 'base64url')` (exactly two arguments) when `s` may be a
//!   string: suggests `Uint8Array.fromBase64(s)` (`{alphabet: 'base64url'}` for the url
//!   form), except when optional or when the result is read right away (through `?:`, `||`,
//!   `await`, sequences, assignments and TS wrappers). Known bytes, non-strings, and values
//!   derived from `toBase64()` are skipped.
//! - `x.toString('base64' | 'base64url')` when `x` may be a `Buffer` (not a known string,
//!   non-buffer, or `toBase64()` result): reported on `toString`.

use super::prelude::*;
use super::unicorn_ast::remove_argument;
use super::unicorn_prefer_spread::is_buffer_reference;
use super::unicorn_prefer_uint8array_hex::{
    BUFFER_IMPORTS, NON_BUFFER_NAMES, is_buffer_expression, is_non_buffer_node,
};
use super::unicorn_static::{Static, static_string_literal};
use super::unicorn_text::paren_span;
use super::unicorn_types::{Checker, Guess, is_string};
use super::unicorn_util::{global_path, skip_wrappers, space_around_keyword, with_edits};
use super::unicorn_util_h1::static_property_name;
use super::unicorn_util_h2::remove_method_call;
use super::unicorn_util_h5::{method_call_of, would_remove_comments};
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-uint8array-base64",
        "Prefer `Uint8Array#toBase64()` and `Uint8Array.fromBase64()` over legacy base64 conversions and manual postprocessing.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create_default::<PreferUint8ArrayBase64>,
};

#[derive(Debug, Default)]
struct PreferUint8ArrayBase64;

fn buffer_node(cx: &Ctx<'_, '_>, id: NodeId, _: &dyn Fn(NodeId) -> bool) -> bool {
    is_buffer_expression(cx, id)
}

fn static_nullish_or_not_buffer(v: &Static, _: &Ast, _: NodeId) -> Guess {
    if v.is_nullish() {
        Guess::Nullish
    } else {
        Guess::NonTarget
    }
}

fn non_buffer(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    is_non_buffer_node(cx, id, true)
}

fn non_buffer_input(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    is_non_buffer_node(cx, id, false)
}

const BUFFER_BASE: Checker = Checker {
    class_heritage: false,
    target_names: &["Buffer"],
    target_imports: BUFFER_IMPORTS,
    target_namespace_imports: BUFFER_IMPORTS,
    non_target_names: NON_BUFFER_NAMES,
    target_node: Some(buffer_node),
    non_target_node: Some(non_buffer),
    static_type: Some(static_nullish_or_not_buffer),
    nullish_in_mixed_union: true,
    mixed_union_non_target: true,
    ..Checker::DEFAULT
};

static BUFFER: Checker = BUFFER_BASE;

/// [`BUFFER`] for a `Buffer.from` input, where `a + b` may be a string.
static BUFFER_INPUT: Checker = Checker {
    non_target_node: Some(non_buffer_input),
    ..BUFFER_BASE
};

/// Known (`Target` / `NonTarget`, nullish folded in) rather than `Unknown`.
fn is_known(g: Guess) -> bool {
    g != Guess::Unknown
}

fn base64_encoding(ast: &Ast, id: Option<NodeId>) -> Option<(String, bool)> {
    let raw = static_string_literal(ast, skip_parens(ast, id?))?;
    match raw.to_ascii_lowercase().as_str() {
        "base64" => Some((raw, false)),
        "base64url" => Some((raw, true)),
        _ => None,
    }
}

/// TS wrappers, parentheses and `await` looked through.
fn unwrap_transparent(ast: &Ast, mut id: NodeId) -> NodeId {
    for _ in 0..10_000 {
        id = skip_wrappers(ast, id);
        match *ast.node(id) {
            Node::Await { arg } => id = arg,
            _ => return id,
        }
    }
    id
}

/// `(expression, TS wrapper?)` one level in: `as`/`satisfies`/`!`/`<T>` or `await`/parens.
enum Step {
    Wrapper(NodeId),
    Transparent(NodeId),
}

fn step(ast: &Ast, id: NodeId) -> Option<Step> {
    match *ast.node(id) {
        Node::As { expr, .. }
        | Node::Satisfies { expr, .. }
        | Node::NonNull { expr }
        | Node::TypeAssertion { expr, .. } => Some(Step::Wrapper(expr)),
        Node::Paren { expr } => Some(Step::Transparent(expr)),
        Node::Await { arg } => Some(Step::Transparent(arg)),
        _ => None,
    }
}

/// A member call on the way down to a `toBase64()` (`x.toBase64().trim()`).
fn is_derived_from_base64(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let mut node = id;
    for _ in 0..10_000 {
        node = unwrap_transparent(ast, node);
        let Node::Call { callee, .. } = *ast.node(node) else {
            return false;
        };
        let callee = skip_parens(ast, callee);
        let obj = match *ast.node(callee) {
            Node::Member { obj, .. } | Node::Index { obj, .. } => obj,
            _ => return false,
        };
        if static_property_name(cx, callee).as_deref() == Some("toBase64") {
            return true;
        }
        node = obj;
    }
    false
}

/// The character a `replace`/`replaceAll` call turns into its base64url form.
fn transformation(ast: &Ast, call: NodeId) -> Option<char> {
    let (_, prop, &[search, replacement]) = method_call_of(
        ast,
        call,
        &["replace", "replaceAll"],
        2,
        Some(2),
        Some(false),
    )?
    else {
        return None;
    };
    let replacement = static_string_literal(ast, skip_parens(ast, replacement))?;
    let replace_all = ident_name(ast, prop) == Some("replaceAll");
    let search = skip_parens(ast, search);
    let mut character = None;
    if replace_all && let Some(s) = static_string_literal(ast, search) {
        character = match s.as_str() {
            "+" => Some('+'),
            "/" => Some('/'),
            "=" => Some('='),
            _ => None,
        };
    } else if let Node::RegexLit { pattern, flags } = *ast.node(search) {
        let (pattern, flags) = (ast.atom(pattern), ast.atom(flags));
        let (global, sticky) = (flags.contains('g'), flags.contains('y'));
        if global && !sticky {
            character = match pattern {
                "\\+" => Some('+'),
                "\\/" => Some('/'),
                _ => None,
            };
        }
        if pattern == "=+$" && !sticky && (global || !replace_all) {
            character = Some('=');
        }
    }
    match (character?, replacement.as_str()) {
        (c @ '+', "-") | (c @ '/', "_") | (c @ '=', "") => Some(c),
        _ => None,
    }
}

fn is_base64_string(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let mut node = unwrap_transparent(ast, id);
    for _ in 0..10_000 {
        if transformation(ast, node).is_none() {
            break;
        }
        let Some((obj, _, _)) = method_call_of(
            ast,
            node,
            &["replace", "replaceAll"],
            2,
            Some(2),
            Some(false),
        ) else {
            break;
        };
        node = unwrap_transparent(ast, obj);
    }
    method_call_of(ast, node, &["toBase64"], 0, None, Some(false)).is_some()
}

fn is_known_string(cx: &Ctx<'_, '_>, id: NodeId, depth: usize) -> bool {
    let ast = cx.ast();
    if depth > 64 {
        return false;
    }
    if is_string(cx, id) {
        return true;
    }
    match step(ast, id) {
        Some(Step::Wrapper(expr)) => {
            return !is_known(BUFFER.guess(cx, id)) && is_known_string(cx, expr, depth + 1);
        }
        Some(Step::Transparent(expr)) => return is_known_string(cx, expr, depth + 1),
        None => {}
    }
    if is_base64_string(cx, id) {
        return true;
    }
    matches!(*ast.node(id), Node::Binary {
        left,
        op: BinaryOp::LogicalOr | BinaryOp::LogicalAnd | BinaryOp::Coalesce,
        right,
    } if is_known_string(cx, left, depth + 1) && is_known_string(cx, right, depth + 1))
}

fn should_ignore_input(cx: &Ctx<'_, '_>, id: NodeId, depth: usize) -> bool {
    let ast = cx.ast();
    if depth > 64 || is_known_string(cx, id, 0) {
        return false;
    }
    if is_derived_from_base64(cx, id) {
        return true;
    }
    match step(ast, id) {
        Some(Step::Wrapper(expr)) => {
            return is_known(BUFFER_INPUT.guess(cx, id))
                || should_ignore_input(cx, expr, depth + 1);
        }
        Some(Step::Transparent(expr)) => return should_ignore_input(cx, expr, depth + 1),
        None => {}
    }
    match *ast.node(id) {
        Node::Binary {
            left,
            op: BinaryOp::LogicalOr | BinaryOp::LogicalAnd | BinaryOp::Coalesce,
            right,
        } => {
            return should_ignore_input(cx, left, depth + 1)
                || should_ignore_input(cx, right, depth + 1);
        }
        Node::Conditional { cons, alt, .. } => {
            return should_ignore_input(cx, cons, depth + 1)
                || should_ignore_input(cx, alt, depth + 1);
        }
        _ => {}
    }
    is_known(BUFFER_INPUT.guess(cx, id))
}

/// The value is read right away, after passing through what keeps it the result.
fn is_chained(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let mut expr = node;
    for _ in 0..10_000 {
        let Some(p) = cx.parent(expr) else {
            return false;
        };
        let passes = match *ast.node(p) {
            Node::Paren { .. }
            | Node::As { .. }
            | Node::Satisfies { .. }
            | Node::NonNull { .. }
            | Node::TypeAssertion { .. }
            | Node::Await { .. } => true,
            Node::Conditional { cons, alt, .. } => cons == expr || alt == expr,
            Node::Binary {
                op: BinaryOp::LogicalOr | BinaryOp::LogicalAnd | BinaryOp::Coalesce,
                ..
            } => true,
            Node::Seq { exprs } => ast.list(exprs).last() == Some(&expr),
            Node::Assign { right, .. } => right == expr,
            Node::Member { obj, .. } | Node::Index { obj, .. } => return obj == expr,
            _ => false,
        };
        if !passes {
            return false;
        }
        expr = p;
    }
    false
}

fn check_options(cx: &mut Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    if method_call_of(ast, node, &["toBase64"], 0, Some(0), Some(false)).is_none()
        || !matches!(*ast.node(node), Node::Call { args, .. } if ast.list(args).is_empty())
    {
        return false;
    }
    let mut found: Vec<char> = Vec::new();
    let mut calls = Vec::new();
    let mut outer = node;
    for _ in 0..10_000 {
        let Some(member) = parent_skip_parens(ast, cx.file.parents, outer) else {
            break;
        };
        let (Node::Member { obj, .. } | Node::Index { obj, .. }) = *ast.node(member) else {
            break;
        };
        if skip_parens(ast, obj) != outer {
            break;
        }
        let Some(call) = parent_skip_parens(ast, cx.file.parents, member) else {
            break;
        };
        if !matches!(*ast.node(call), Node::Call { callee, .. } if skip_parens(ast, callee) == member)
        {
            break;
        }
        let Some(c) = transformation(ast, call) else {
            break;
        };
        if found.contains(&c) {
            return false;
        }
        found.push(c);
        calls.push(call);
        outer = call;
    }
    if calls.is_empty() || found.contains(&'+') != found.contains(&'/') {
        return false;
    }
    let mut options = Vec::new();
    if found.contains(&'+') {
        options.push("alphabet: 'base64url'");
    }
    if found.contains(&'=') {
        options.push("omitPadding: true");
    }
    let span = cx.span(outer);
    let fix = (!would_remove_comments(cx, span, &[paren_span(cx, node)])).then(|| {
        let close = cx.span(node).end - 1;
        let mut fix = Fix::insert(close, format!("{{{}}}", options.join(", ")));
        for &call in &calls {
            fix = remove_method_call(cx, call, fix);
        }
        fix
    });
    cx.report(
        span,
        "Prefer native `toBase64()` options over manual base64 postprocessing.",
    )
    .id("prefer-uint8array-base64/options")
    .fix_opt(fix);
    true
}

fn check_buffer_from(cx: &mut Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let Node::Call {
        callee,
        optional,
        args,
        ..
    } = *ast.node(node)
    else {
        return false;
    };
    let args = ast.list(args);
    let member = skip_parens(ast, callee);
    let Node::Member {
        obj,
        optional: optional_member,
        prop,
    } = *ast.node(member)
    else {
        return false;
    };
    let &[input, encoding] = args else {
        return false;
    };
    if ident_name(ast, prop) != Some("from")
        || args
            .iter()
            .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
    {
        return false;
    }
    let Some((raw, url)) = base64_encoding(ast, Some(encoding)) else {
        return false;
    };
    if !is_buffer_reference(cx, obj) || should_ignore_input(cx, input, 0) {
        return false;
    }
    let suggestion = (!optional && !optional_member && !is_chained(cx, node))
        .then(|| {
            if would_remove_comments(cx, cx.span(node), &[paren_span(cx, input)]) {
                return None;
            }
            let fix = with_edits(
                Fix::replace(cx.span(member), "Uint8Array.fromBase64"),
                space_around_keyword(cx, member),
            );
            Some(if url {
                fix.and(cx.span(encoding), "{alphabet: 'base64url'}")
            } else {
                let removal = remove_argument(cx, node, 1)?;
                let mut fix = fix;
                for e in removal.edits {
                    fix = fix.and(e.span, e.text);
                }
                fix
            })
        })
        .flatten();
    let r = cx
        .report(
            cx.span(member),
            format!("Prefer `Uint8Array.fromBase64()` over `Buffer.from(…, '{raw}')`."),
        )
        .id("prefer-uint8array-base64/error");
    if let Some(fix) = suggestion {
        r.suggest(
            "prefer-uint8array-base64/suggestion",
            "Replace `Buffer.from()` with `Uint8Array.fromBase64()`.",
            fix,
        );
    }
    true
}

fn check_to_string(cx: &mut Ctx<'_, '_>, node: NodeId) {
    let ast = cx.ast();
    let Some((obj, prop, &[encoding])) = method_call_of(ast, node, &["toString"], 1, Some(1), None)
    else {
        return;
    };
    let Some((raw, url)) = base64_encoding(ast, Some(encoding)) else {
        return;
    };
    if matches!(BUFFER.guess(cx, obj), Guess::NonTarget | Guess::Nullish)
        || is_known_string(cx, obj, 0)
        || is_derived_from_base64(cx, obj)
    {
        return;
    }
    let replacement = if url {
        "Uint8Array#toBase64({alphabet: 'base64url', omitPadding: true})"
    } else {
        "Uint8Array#toBase64()"
    };
    cx.report(
        cx.span(prop),
        format!("Prefer `{replacement}` over `toString('{raw}')`."),
    )
    .id("prefer-uint8array-base64/error");
}

impl Rule for PreferUint8ArrayBase64 {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Node::Call { callee, .. } = *cx.node(node) else {
            return;
        };
        if let Some(path) = global_path(cx, callee)
            && let Some((what, replacement)) = match path.as_str() {
                "atob" => Some(("atob()", "Uint8Array.fromBase64()")),
                "btoa" => Some(("btoa()", "Uint8Array#toBase64()")),
                _ => None,
            }
        {
            let span = cx.span(skip_parens(cx.ast(), callee));
            cx.report(span, format!("Prefer `{replacement}` over `{what}`."))
                .id("prefer-uint8array-base64/error");
            return;
        }
        if check_options(cx, node) || check_buffer_from(cx, node) {
            return;
        }
        check_to_string(cx, node);
    }
}
