//! `unicorn/prefer-uint8array-hex`: `Uint8Array#toHex()` / `Uint8Array.fromHex()` over hand
//! written and `Buffer` hex conversions.
//!
//! Encoding: `<copy>.map(byte => byte.toString(16).padStart(2, '0')).join('')` (or
//! `('0' + byte.toString(16)).slice(-2)`, `` `0${…}`.slice(-2) ``, `'00'` prefixes), with the
//! copy `[...x]` / `Array.from(x)`, or `Array.from(x, cb)` / `Array.prototype.map.call(x, cb)`
//! / `[].map.call(x, cb)`; and `x.toString('hex')` (any case). The callback is one plain
//! parameter, not async or a generator, returning that expression. `x` is guessed as a byte
//! array (`Uint8Array`, `Buffer`) or a `Buffer`: known ones are fixed to `x.toHex()`, unknown
//! ones suggested, others left (a typed array of another kind, an array, a constructor, …).
//! `Buffer.from(<bytes>).toString('hex')` converts the bytes themselves.
//!
//! Decoding: `Buffer.from(x, 'hex')` when `x` may be a string (suggested unless the result is
//! read right away), and `new Uint8Array(<s>.match(<pairs>).map(p => parseInt(p, 16)))` /
//! `Uint8Array.from(…)` (also `(… ?? [])`), where `<pairs>` is `/../g`, `/.{2}/g`, `/.{1,2}/g`,
//! `/[0-9a-f]{2}/g` or `/[\da-f]{2}/g` (flags only from `gisuv`). Both suggest
//! `Uint8Array.fromHex(x)`. Nothing for `super`, an optional chain, or when a comment would
//! go.

use super::prelude::*;
use super::unicorn_no_array_reduce::{is_array_prototype_property, return_expression};
use super::unicorn_prefer_spread::is_buffer_reference;
use super::unicorn_static::static_string_literal;
use super::unicorn_text::paren_span;
use super::unicorn_text::{member_object_text, needs_semicolon, would_remove_comments};
use super::unicorn_types::{Checker, Guess, TYPED_ARRAYS, is_string};
use super::unicorn_util::method_call_of;
use super::unicorn_util::{skip_wrappers, space_around_keyword, with_edits};
use ts_r_ast::{Ast, UnaryOp};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-uint8array-hex",
        "Prefer `Uint8Array#toHex()` and `Uint8Array.fromHex()` over manual and Buffer hex conversions.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create_default::<PreferUint8ArrayHex>,
};

#[derive(Debug, Default)]
struct PreferUint8ArrayHex;

const GLOBAL_OBJECTS: &[&str] = &["globalThis", "window", "self", "global"];
pub(crate) const BUFFER_IMPORTS: &[(&str, &[&str])] =
    &[("buffer", &["Buffer"]), ("node:buffer", &["Buffer"])];
pub(crate) const NON_BUFFER_NAMES: &[&str] = &[
    "Array",
    "ReadonlyArray",
    "ArrayBuffer",
    "SharedArrayBuffer",
    "DataView",
    "Int8Array",
    "Uint8Array",
    "Uint8ClampedArray",
    "Int16Array",
    "Uint16Array",
    "Int32Array",
    "Uint32Array",
    "Float16Array",
    "Float32Array",
    "Float64Array",
    "BigInt64Array",
    "BigUint64Array",
];

/// `<obj>.<method>(…)` with exactly `n` arguments, nothing optional or computed, no spread.
fn plain_call<'a>(
    ast: &'a Ast,
    id: NodeId,
    method: &str,
    n: usize,
) -> Option<(NodeId, &'a [NodeId])> {
    method_call_of(
        ast,
        skip_parens(ast, id),
        &[method],
        n,
        Some(n),
        Some(false),
    )
    .map(|(obj, _, args)| (obj, args))
}

fn named(ast: &Ast, id: NodeId, name: &str) -> bool {
    ident_name(ast, skip_parens(ast, id)) == Some(name)
}

fn is_number(ast: &Ast, id: NodeId, n: f64) -> bool {
    matches!(*ast.node(skip_parens(ast, id)), Node::NumberLit { value } if value.get() == n)
}

fn static_string(ast: &Ast, id: NodeId) -> Option<String> {
    static_string_literal(ast, skip_parens(ast, id))
}

/// `Buffer.from(…)`-style factories, `new Buffer(…)` and `Buffer(…)`.
pub(crate) fn is_buffer_expression(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    if let Some((obj, _, _)) = method_call_of(
        ast,
        id,
        &[
            "from",
            "of",
            "alloc",
            "allocUnsafe",
            "allocUnsafeSlow",
            "concat",
            "copyBytesFrom",
        ],
        0,
        None,
        Some(false),
    ) && is_buffer_reference(cx, obj)
    {
        return true;
    }
    match *ast.node(id) {
        Node::New { callee, .. }
        | Node::Call {
            callee,
            optional: false,
            ..
        } => is_buffer_reference(cx, callee),
        _ => false,
    }
}

fn buffer_node(cx: &Ctx<'_, '_>, id: NodeId, _: &dyn Fn(NodeId) -> bool) -> bool {
    is_buffer_expression(cx, id)
}

fn byte_array_node(cx: &Ctx<'_, '_>, id: NodeId, _: &dyn Fn(NodeId) -> bool) -> bool {
    let ast = cx.ast();
    is_buffer_expression(cx, id)
        || method_call_of(
            ast,
            id,
            &["from", "of", "fromHex", "fromBase64"],
            0,
            None,
            Some(false),
        )
        .is_some_and(|(obj, _, _)| named(ast, obj, "Uint8Array"))
}

/// `Buffer`, `Array`, an array buffer or typed array constructor (also `globalThis.X`).
fn is_constructor_reference(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    if is_buffer_reference(cx, id) {
        return true;
    }
    let is_constructor = |n: &str| NON_BUFFER_NAMES.contains(&n) && n != "ReadonlyArray";
    match *ast.node(id) {
        Node::Ident { .. } => ident_name(ast, id).is_some_and(is_constructor),
        Node::Member {
            obj,
            optional: false,
            prop,
        } => {
            ident_name(ast, prop).is_some_and(is_constructor)
                && GLOBAL_OBJECTS.iter().any(|n| cx.is_global_ref(obj, n))
        }
        _ => false,
    }
}

pub(crate) fn is_non_buffer_node(cx: &Ctx<'_, '_>, id: NodeId, plus: bool) -> bool {
    let ast = cx.ast();
    let shape = match *ast.node(id) {
        Node::Binary {
            op: BinaryOp::Add, ..
        } => plus,
        Node::ArrayLit { .. }
        | Node::Arrow { .. }
        | Node::Binary { .. }
        | Node::ClassExpr { .. }
        | Node::FunctionExpr { .. }
        | Node::StringLit { .. }
        | Node::NumberLit { .. }
        | Node::BigIntLit { .. }
        | Node::BoolLit { .. }
        | Node::NullLit {}
        | Node::RegexLit { .. }
        | Node::New { .. }
        | Node::ObjectLit { .. }
        | Node::TemplateLit { .. }
        | Node::Unary { .. }
        | Node::Update { .. } => true,
        _ => false,
    };
    if shape || is_constructor_reference(cx, id) {
        return true;
    }
    if let Node::Call { callee, .. } = *ast.node(id)
        && named(ast, callee, "Array")
    {
        return true;
    }
    method_call_of(
        ast,
        id,
        &["from", "of", "fromHex", "fromBase64"],
        0,
        None,
        None,
    )
    .is_some_and(|(obj, prop, _)| {
        let method = ident_name(ast, prop).unwrap_or("");
        ident_name(ast, skip_parens(ast, obj)).is_some_and(|o| {
            (matches!(method, "from" | "of") && (o == "Array" || TYPED_ARRAYS.contains(&o)))
                || (o == "Uint8Array" && matches!(method, "fromHex" | "fromBase64"))
        })
    })
}

fn non_buffer_node(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    is_non_buffer_node(cx, id, true)
}

fn non_buffer_node_decoding(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    is_non_buffer_node(cx, id, false)
}

fn always_non_target(_: &super::unicorn_static::Static, _: &Ast, _: NodeId) -> Guess {
    Guess::NonTarget
}

static BUFFER: Checker = Checker {
    class_heritage: false,
    prefer_definitions: true,
    target_names: &["Buffer"],
    target_imports: BUFFER_IMPORTS,
    target_namespace_imports: BUFFER_IMPORTS,
    non_target_names: NON_BUFFER_NAMES,
    target_node: Some(buffer_node),
    non_target_node: Some(non_buffer_node),
    ..Checker::DEFAULT
};

const BYTE_ARRAY_BASE: Checker = Checker {
    class_heritage: false,
    prefer_definitions: true,
    target_names: &["Buffer", "Uint8Array"],
    target_imports: BUFFER_IMPORTS,
    target_namespace_imports: BUFFER_IMPORTS,
    non_target_names: NON_BUFFER_NAMES,
    target_ctors: &["Uint8Array"],
    target_node: Some(byte_array_node),
    non_target_node: Some(non_buffer_node),
    ..Checker::DEFAULT
};

static BYTE_ARRAY: Checker = BYTE_ARRAY_BASE;

/// [`BYTE_ARRAY`] for a decoding input: any static value and `+` are not bytes.
static BYTE_ARRAY_DECODING: Checker = Checker {
    static_type: Some(always_non_target),
    non_target_node: Some(non_buffer_node_decoding),
    ..BYTE_ARRAY_BASE
};

/// A one-parameter, non-async, non-generator function returning one expression:
/// (parameter name, returned expression without TS wrappers).
fn callback(ast: &Ast, id: NodeId) -> Option<(&str, NodeId)> {
    let f = skip_wrappers(ast, id);
    let (flags, params) = match *ast.node(f) {
        Node::Arrow { flags, params, .. } | Node::FunctionExpr { flags, params, .. } => {
            (flags, ast.list(params))
        }
        _ => return None,
    };
    if flags.intersects(FnFlags::ASYNC | FnFlags::GENERATOR) {
        return None;
    }
    let &[param] = params else {
        return None;
    };
    let Node::Param {
        rest: false,
        pattern,
        init: None,
        ..
    } = *ast.node(param)
    else {
        return None;
    };
    let name = match ast.node(pattern) {
        Node::Ident { .. } => ident_name(ast, pattern)?,
        _ => return None,
    };
    Some((name, skip_wrappers(ast, return_expression(ast, f)?)))
}

/// `'0' + x` / `'00' + x` / `` `0${x}` ``: `x`.
fn zero_prefixed(ast: &Ast, id: NodeId) -> Option<NodeId> {
    let is_zero = |s: &str| s == "0" || s == "00";
    match *ast.node(skip_parens(ast, id)) {
        Node::Binary {
            left,
            op: BinaryOp::Add,
            right,
        } if static_string(ast, left).is_some_and(|s| is_zero(&s)) => Some(right),
        Node::TemplateLit { parts } => {
            let &[head, expr, tail] = ast.list(parts) else {
                return None;
            };
            let cooked = |p: NodeId| match *ast.node(p) {
                Node::TemplateElement {
                    cooked,
                    invalid_escape: false,
                } => Some(ast.atom(cooked)),
                _ => None,
            };
            (cooked(head).is_some_and(is_zero) && cooked(tail) == Some("")).then_some(expr)
        }
        _ => None,
    }
}

fn is_encoding_callback(ast: &Ast, id: NodeId) -> bool {
    let Some((param, expr)) = callback(ast, id) else {
        return false;
    };
    let number_to_string = if let Some((obj, args)) = plain_call(ast, expr, "padStart", 2) {
        if !is_number(ast, args[0], 2.0) || static_string(ast, args[1]).as_deref() != Some("0") {
            return false;
        }
        obj
    } else if let Some((obj, &[index])) = plain_call(ast, expr, "slice", 1) {
        let Node::Unary {
            op: UnaryOp::Minus,
            arg,
        } = *ast.node(skip_parens(ast, index))
        else {
            return false;
        };
        if !is_number(ast, arg, 2.0) {
            return false;
        }
        let Some(inner) = zero_prefixed(ast, obj) else {
            return false;
        };
        inner
    } else {
        return false;
    };
    plain_call(ast, number_to_string, "toString", 1)
        .is_some_and(|(obj, args)| is_number(ast, args[0], 16.0) && named(ast, obj, param))
}

fn is_decoding_callback(ast: &Ast, id: NodeId) -> bool {
    let Some((param, expr)) = callback(ast, id) else {
        return false;
    };
    let args = match *ast.node(expr) {
        Node::Call {
            callee,
            optional: false,
            args,
            ..
        } if named(ast, callee, "parseInt") => ast.list(args),
        _ => match plain_call(ast, expr, "parseInt", 2) {
            Some((obj, args)) if named(ast, obj, "Number") => args,
            _ => return false,
        },
    };
    let &[value, radix] = args else {
        return false;
    };
    !matches!(ast.node(value), Node::Spread { .. })
        && named(ast, value, param)
        && matches!(ast.node(skip_parens(ast, value)), Node::Ident { .. })
        && is_number(ast, radix, 16.0)
}

fn encoding_input(ast: &Ast, node: NodeId) -> Option<NodeId> {
    let (joined, args) = plain_call(ast, node, "join", 1)?;
    if static_string_literal(ast, args[0]).as_deref() != Some("") {
        return None;
    }
    let mapped = skip_wrappers(ast, joined);
    if let Some((obj, args)) = plain_call(ast, mapped, "from", 2)
        && named(ast, obj, "Array")
    {
        return is_encoding_callback(ast, args[1]).then_some(args[0]);
    }
    if let Some((obj, args)) = plain_call(ast, mapped, "call", 2)
        && is_array_prototype_property(ast, skip_parens(ast, obj), &["map"])
    {
        return is_encoding_callback(ast, args[1]).then_some(args[0]);
    }
    let (copied, args) = plain_call(ast, mapped, "map", 1)?;
    if !is_encoding_callback(ast, args[0]) {
        return None;
    }
    let copied = skip_wrappers(ast, copied);
    if let Node::ArrayLit { elements } = *ast.node(copied)
        && let &[only] = ast.list(elements)
        && let Node::Spread { arg } = *ast.node(only)
    {
        return Some(arg);
    }
    plain_call(ast, copied, "from", 1)
        .filter(|&(obj, _)| named(ast, obj, "Array"))
        .map(|(_, args)| args[0])
}

const HEX_PAIRS: &[&str] = &["..", ".{2}", ".{1,2}", "[0-9a-f]{2}", "[\\da-f]{2}"];

fn decoding_input(ast: &Ast, node: NodeId) -> Option<NodeId> {
    let (pairs, cb) = if let Some((obj, args)) = plain_call(ast, node, "from", 2)
        && named(ast, obj, "Uint8Array")
    {
        (args[0], args[1])
    } else {
        let new_arg = match *ast.node(node) {
            Node::New { callee, args, .. } if named(ast, callee, "Uint8Array") => {
                match *ast.list(args) {
                    [arg] if !matches!(ast.node(arg), Node::Spread { .. }) => Some(arg),
                    _ => None,
                }
            }
            _ => None,
        };
        let arg = match new_arg {
            Some(arg) => arg,
            None => match plain_call(ast, node, "from", 1) {
                Some((obj, args)) if named(ast, obj, "Uint8Array") => args[0],
                _ => return None,
            },
        };
        let (pairs, args) = plain_call(ast, skip_wrappers(ast, arg), "map", 1)?;
        (pairs, args[0])
    };
    if !is_decoding_callback(ast, cb) {
        return None;
    }
    let mut pairs = skip_wrappers(ast, pairs);
    if let Node::Binary {
        left,
        op: BinaryOp::Coalesce | BinaryOp::LogicalOr,
        right,
    } = *ast.node(pairs)
        && matches!(*ast.node(skip_parens(ast, right)), Node::ArrayLit { elements } if ast.list(elements).is_empty())
    {
        pairs = skip_wrappers(ast, left);
    }
    let (text, args) = plain_call(ast, pairs, "match", 1)?;
    let Node::RegexLit { pattern, flags } = *ast.node(skip_parens(ast, args[0])) else {
        return None;
    };
    let flags = ast.atom(flags);
    (HEX_PAIRS.contains(&ast.atom(pattern))
        && flags.contains('g')
        && flags.chars().all(|c| "gisuv".contains(c)))
    .then_some(text)
}

/// An optional link on the member/call spine (unicorn's `ChainExpression`).
fn is_chain(ast: &Ast, id: NodeId) -> bool {
    let mut id = skip_wrappers(ast, id);
    for _ in 0..10_000 {
        id = match *ast.node(id) {
            Node::Member { optional: true, .. }
            | Node::Index { optional: true, .. }
            | Node::Call { optional: true, .. } => return true,
            Node::Member { obj, .. } | Node::Index { obj, .. } => obj,
            Node::Call { callee, .. } => callee,
            _ => return false,
        };
    }
    false
}

/// The value is read right away: `<node>.x`, `(<node> as T)[i]`.
fn is_immediately_accessed(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let mut outer = node;
    for _ in 0..10_000 {
        let Some(p) = cx.parent(outer) else {
            return false;
        };
        match *ast.node(p) {
            Node::Paren { .. }
            | Node::As { .. }
            | Node::Satisfies { .. }
            | Node::NonNull { .. }
            | Node::TypeAssertion { .. } => outer = p,
            Node::Member { obj, .. } | Node::Index { obj, .. } => return obj == outer,
            _ => return false,
        }
    }
    false
}

enum Mode {
    Fix,
    Suggest,
    Report,
}

fn report(cx: &mut Ctx<'_, '_>, node: NodeId, input: NodeId, decoding: bool, mode: Mode) {
    let ast = cx.ast();
    let input = skip_parens(ast, input);
    if matches!(ast.node(input), Node::Super {}) {
        return;
    }
    let replacement = if decoding {
        "Uint8Array.fromHex()"
    } else {
        "Uint8Array#toHex()"
    };
    let what = if decoding {
        "this hex decoding"
    } else {
        "this hex encoding"
    };
    let span = cx.span(node);
    let input_span = paren_span(cx, input);
    let fix = (!matches!(mode, Mode::Report) && !would_remove_comments(cx, span, &[input_span]))
        .then(|| {
            let mut text = if decoding {
                format!("Uint8Array.fromHex({})", input_span.text(cx.src()))
            } else {
                format!("{}.toHex()", member_object_text(cx, input, node))
            };
            if needs_semicolon(cx, span.start, &text) {
                text.insert(0, ';');
            }
            with_edits(Fix::replace(span, text), space_around_keyword(cx, node))
        });
    let r = cx
        .report(span, format!("Prefer `{replacement}` over {what}."))
        .id("prefer-uint8array-hex/error");
    match (mode, fix) {
        (Mode::Fix, Some(fix)) => {
            r.fix(fix);
        }
        (Mode::Suggest, Some(fix)) => {
            r.suggest(
                "prefer-uint8array-hex/suggestion",
                format!("Replace with `{replacement}`."),
                fix,
            );
        }
        _ => {}
    }
}

fn is_hex(ast: &Ast, id: NodeId) -> bool {
    static_string_literal(ast, id).is_some_and(|s| s.eq_ignore_ascii_case("hex"))
}

impl Rule for PreferUint8ArrayHex {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call, NodeKind::New]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        if let Some(input) = encoding_input(ast, node)
            && !is_chain(ast, input)
        {
            match BYTE_ARRAY.guess(cx, input) {
                Guess::NonTarget | Guess::Nullish => {}
                g => {
                    let mode = if g == Guess::Target {
                        Mode::Fix
                    } else {
                        Mode::Suggest
                    };
                    return report(cx, node, input, false, mode);
                }
            }
        }
        if let Some((obj, args)) = plain_call(ast, node, "toString", 1)
            && is_hex(ast, args[0])
        {
            let mut input = skip_parens(ast, obj);
            let g = BUFFER.guess(cx, input);
            if matches!(g, Guess::NonTarget | Guess::Nullish) || is_chain(ast, input) {
                return;
            }
            if let Some((buffer, &[bytes])) = plain_call(ast, input, "from", 1)
                && is_buffer_reference(cx, buffer)
                && BYTE_ARRAY.guess(cx, bytes) == Guess::Target
            {
                input = bytes;
            }
            let mode = if g == Guess::Target {
                Mode::Fix
            } else {
                Mode::Suggest
            };
            return report(cx, node, input, false, mode);
        }
        if let Some((buffer, &[text, encoding])) = plain_call(ast, node, "from", 2)
            && is_hex(ast, encoding)
            && is_buffer_reference(cx, buffer)
        {
            let known_non_string = match BYTE_ARRAY_DECODING.guess(cx, text) {
                Guess::Target => true,
                Guess::NonTarget | Guess::Nullish => !is_string(cx, text),
                Guess::Unknown => false,
            };
            if !known_non_string {
                let mode = if is_immediately_accessed(cx, node) {
                    Mode::Report
                } else {
                    Mode::Suggest
                };
                return report(cx, node, text, true, mode);
            }
        }
        if let Some(input) = decoding_input(ast, node)
            && !is_chain(ast, input)
        {
            report(cx, node, input, true, Mode::Suggest);
        }
    }
}
