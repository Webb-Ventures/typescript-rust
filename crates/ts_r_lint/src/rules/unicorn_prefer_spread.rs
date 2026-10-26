//! `unicorn/prefer-spread`: array copies and joins written with spread syntax.
//!
//! - `Array.from(x)` (the global, one argument) becomes `[...x]` (an array literal without
//!   holes is kept as is). Skipped for `{length}` objects, `Array(n).keys()` ranges,
//!   non-string literals, `new ArrayBuffer`/`SharedArrayBuffer`/`DataView`, and literals of
//!   two or more spreads.
//! - `a.concat(b, ...)` becomes `[...a, ...b]`, unless the receiver is known not to be an
//!   array (literals, templates, binary expressions, class names, `.join()` calls,
//!   `String()`-style calls, `Buffer` concatenation, `new X()` other than `Array`, the
//!   non-array branch of an `Array.isArray` test, type annotations) or, when it isn't
//!   known to be one, every argument is a static string; also not an `x = x.concat(...)`
//!   accumulator in a loop. Leading arguments whose spreadability is known (array
//!   literals, static values) are merged; when the first isn't known, suggestions offer
//!   spreading it, not spreading it, an `Array.isArray` test, and spreading everything.
//! - `a.slice()` / `a.slice(0)` / `a.toSpliced()` become `[...a]` unless the receiver is
//!   not an array (as above, plus `arrayBuffer`, `blob`, `buffer`, `file`, `this`, typed
//!   array and buffer constructions, array literals and optional chains).
//! - `const a = []; for (const x of y) a.push(x);` (or the `entries()` index form on a known
//!   array) becomes `const a = [...y];`.
//!
//! Fixes keep the moved code's comments or are withheld. In TypeScript files they are
//! suggestions (dropping the call can change inferred types, and there is no type checker
//! here to prove otherwise); an argument with possible side effects also makes the concat
//! fix a suggestion, as does a loop array read by a function that can run during the loop.

use super::prelude::*;
use super::unicorn_static::{
    Static, const_definition, has_side_effect, mutable_member_access, static_value_no_side_effects,
};
use super::unicorn_text::{
    has_comment_in, has_possible_side_effect, has_unparenthesized_optional_chain, needs_semicolon,
    paren_span, paren_text, would_remove_comments,
};
use super::unicorn_types::{binding_annotation, type_definition};
use super::unicorn_util::is_same_reference;
use crate::semantic::Decl;
use ts_r_ast::{Ast, KeywordTypeKind, TypeOperatorKind, VarKind};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-spread",
        "Prefer the spread operator over `Array.from(…)`, `Array#concat(…)`, `Array#{slice,toSpliced}()`, and trivial `for…of` copies.",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<PreferSpread>,
};

#[derive(Debug, Default)]
struct PreferSpread;

impl Rule for PreferSpread {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call, NodeKind::ForOf]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        if matches!(cx.node(node), Node::ForOf { .. }) {
            check_for_of(cx, node);
            return;
        }
        check_array_from(cx, node);
        check_concat(cx, node);
        check_slice(cx, node, "slice");
        check_slice(cx, node, "toSpliced");
    }
}

const TYPED_ARRAYS: &[&str] = &[
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

fn unwrap_ts(ast: &Ast, mut id: NodeId) -> NodeId {
    loop {
        match *ast.node(id) {
            Node::Paren { expr }
            | Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::NonNull { expr }
            | Node::TypeAssertion { expr, .. }
            | Node::Instantiation { expr, .. } => id = expr,
            _ => return id,
        }
    }
}

pub(crate) fn is_ts_file(cx: &Ctx<'_, '_>) -> bool {
    let p = cx.file.path;
    [".ts", ".tsx", ".mts", ".cts"]
        .iter()
        .any(|e| p.ends_with(e))
}

fn is_spread(ast: &Ast, id: NodeId) -> bool {
    matches!(ast.node(id), Node::Spread { .. })
}

/// `recv.name(args)` (non-computed member, plain call): receiver (no parens), property,
/// member and arguments. Spread arguments are rejected below index `max`.
struct MethodCall<'a> {
    obj: NodeId,
    prop: NodeId,
    member: NodeId,
    args: &'a [NodeId],
}

fn method_call<'a>(
    cx: &Ctx<'a, '_>,
    call: NodeId,
    name: &str,
    max: Option<usize>,
) -> Option<MethodCall<'a>> {
    let ast = cx.ast();
    let Node::Call {
        callee,
        args,
        optional: false,
        ..
    } = *ast.node(call)
    else {
        return None;
    };
    let member = skip_parens(ast, callee);
    let Node::Member {
        obj,
        prop,
        optional: false,
    } = *ast.node(member)
    else {
        return None;
    };
    let args = ast.list(args);
    if ident_name(ast, prop) != Some(name) {
        return None;
    }
    if let Some(max) = max
        && (args.len() > max || args.iter().take(max).any(|&a| is_spread(ast, a)))
    {
        return None;
    }
    Some(MethodCall {
        obj: skip_parens(ast, obj),
        prop,
        member,
        args,
    })
}

fn is_global(cx: &Ctx<'_, '_>, id: NodeId, name: &str) -> bool {
    matches!(cx.node(id), Node::Ident { .. }) && cx.is_global_ref(id, name)
}

fn has_holes(ast: &Ast, id: NodeId) -> bool {
    matches!(*ast.node(id), Node::ArrayLit { elements }
        if ast.list(elements).iter().any(|&e| matches!(ast.node(e), Node::Hole {})))
}

fn is_empty_array(ast: &Ast, id: NodeId) -> bool {
    matches!(*ast.node(id), Node::ArrayLit { elements } if ast.list(elements).is_empty())
}

/// The array literal ends with `,` before `]`: the comma's start.
fn trailing_comma(cx: &Ctx<'_, '_>, id: NodeId) -> Option<u32> {
    let ast = cx.ast();
    if !matches!(ast.node(id), Node::ArrayLit { .. }) || is_empty_array(ast, id) {
        return None;
    }
    let close = cx.span(id).end as usize - 1;
    let before = skip_trivia_backward(ast, cx.src(), close);
    (before > 0 && cx.src().as_bytes()[before - 1] == b',').then(|| before as u32 - 1)
}

/// `removeMethodCall`: drops `.name(...)` after the receiver.
fn remove_method_call(cx: &Ctx<'_, '_>, call: NodeId, m: &MethodCall<'_>) -> Vec<(Span, String)> {
    let obj_end = paren_span(cx, m.obj).end;
    let member_end = paren_span(cx, m.member).end;
    vec![
        (Span::new(obj_end, cx.span(m.member).end), String::new()),
        (Span::new(member_end, cx.span(call).end), String::new()),
    ]
}

fn build(edits: Vec<(Span, String)>) -> Fix {
    let mut fix = Fix { edits: Vec::new() };
    for (span, text) in edits {
        fix = fix.and(span, text);
    }
    fix
}

fn report_spread(
    cx: &mut Ctx<'_, '_>,
    at: NodeId,
    id: &'static str,
    msg: &str,
    fix: Option<Fix>,
    suggest: bool,
) {
    let report = cx.report_node(at, msg.to_owned()).id(id);
    match fix {
        Some(fix) if suggest => {
            report.suggest("spread", "Use the spread operator.", fix);
        }
        fix => {
            report.fix_opt(fix);
        }
    }
}

// ---- Array.from ----------------------------------------------------------------------------

fn is_array_ctor_with_one_arg_global(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    match *ast.node(id) {
        Node::Call {
            callee,
            args,
            optional: false,
            ..
        }
        | Node::New { callee, args, .. } => {
            matches!(ast.list(args), [a] if !is_spread(ast, *a)) && is_global(cx, callee, "Array")
        }
        _ => false,
    }
}

/// `getArrayRangeLength`: `Array(n).keys()` with a valid static or immutable length.
fn is_array_range(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    array_range_length(cx, id).is_some()
}

/// The length `n` of `Array(n).keys()` / `new Array(n).keys()` (the global `Array`), when
/// it is a valid array length or not statically known and not read from a mutable member.
pub(crate) fn array_range_length(cx: &Ctx<'_, '_>, id: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let m = method_call(cx, unwrap_ts(ast, id), "keys", Some(0))?;
    if !m.args.is_empty() {
        return None;
    }
    let receiver = unwrap_ts(ast, m.obj);
    if !is_array_ctor_with_one_arg_global(cx, receiver) {
        return None;
    }
    let (Node::Call { args, .. } | Node::New { args, .. }) = *ast.node(receiver) else {
        return None;
    };
    let length = ast.list(args)[0];
    let invalid = match static_value_no_side_effects(cx, length) {
        Some(Static::Num(n)) => !(n.fract() == 0.0 && (0.0..=4_294_967_295.0).contains(&n)),
        Some(_) => true,
        None => mutable_member_access(cx, length, &mut Vec::new()),
    };
    (!invalid).then_some(length)
}

fn is_to_array_call(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    method_call(cx, skip_parens(cx.ast(), id), "toArray", Some(0))
        .is_some_and(|m| m.args.is_empty())
}

fn check_array_from(cx: &mut Ctx<'_, '_>, node: NodeId) {
    let ast = cx.ast();
    let Some(m) = method_call(cx, node, "from", Some(1)) else {
        return;
    };
    let &[arg] = m.args else {
        return;
    };
    if !is_global(cx, m.obj, "Array") {
        return;
    }
    let arg = skip_parens(ast, arg);
    let skip = match *ast.node(arg) {
        Node::ObjectLit { .. }
        | Node::NumberLit { .. }
        | Node::BoolLit { .. }
        | Node::NullLit {}
        | Node::RegexLit { .. }
        | Node::BigIntLit { .. } => true,
        Node::New { callee, .. } => {
            matches!(
                ident_name(ast, callee),
                Some("ArrayBuffer" | "SharedArrayBuffer" | "DataView")
            ) && matches!(ast.node(callee), Node::Ident { .. })
        }
        Node::ArrayLit { elements } => {
            let els = ast.list(elements);
            els.len() >= 2
                && els.iter().all(|&e| match *ast.node(e) {
                    Node::Spread { arg } => !is_to_array_call(cx, arg),
                    _ => false,
                })
        }
        _ => false,
    };
    if skip || is_array_range(cx, arg) {
        return;
    }
    let is_literal = matches!(ast.node(arg), Node::ArrayLit { .. });
    let preserved = if is_literal {
        cx.span(arg)
    } else {
        paren_span(cx, arg)
    };
    let fix = (!would_remove_comments(cx, cx.span(node), &[preserved])).then(|| {
        let mut text = if is_literal && !has_holes(ast, arg) {
            cx.text(arg).to_owned()
        } else {
            format!("[...{}]", paren_text(cx, arg))
        };
        if needs_semicolon(cx, cx.span(node).start, "[") {
            text.insert(0, ';');
        }
        Fix::replace(cx.span(node), text)
    });
    let suggest = is_ts_file(cx);
    report_spread(
        cx,
        node,
        "array-from",
        "Prefer the spread operator over `Array.from(…)`.",
        fix,
        suggest,
    );
}

// ---- Receivers that are (not) arrays -------------------------------------------------------

fn is_class_name(ast: &Ast, id: NodeId) -> bool {
    let id = match *ast.node(id) {
        Node::Member { prop, .. } => prop,
        Node::Index { index, .. } => skip_parens(ast, index),
        _ => id,
    };
    let Some(name) = (match ast.node(id) {
        Node::Ident { .. } => ident_name(ast, id),
        _ => None,
    }) else {
        return false;
    };
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_uppercase())
        && chars.next().is_some()
        && name.to_uppercase() != name
}

fn is_method_named(ast: &Ast, id: NodeId, name: &str) -> bool {
    matches!(*ast.node(id), Node::Call { callee, .. }
        if matches!(*ast.node(skip_parens(ast, callee)), Node::Member { prop, .. } if ident_name(ast, prop) == Some(name)))
}

/// `isNotArray`.
fn is_not_array(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let id = skip_parens(ast, id);
    let syntactic = match *ast.node(id) {
        Node::TemplateLit { .. }
        | Node::StringLit { .. }
        | Node::NumberLit { .. }
        | Node::BoolLit { .. }
        | Node::NullLit {}
        | Node::RegexLit { .. }
        | Node::BigIntLit { .. }
        | Node::Super {} => true,
        Node::Binary { op, .. } => !matches!(
            op,
            BinaryOp::LogicalAnd | BinaryOp::LogicalOr | BinaryOp::Coalesce
        ),
        _ => false,
    };
    if syntactic || is_class_name(ast, id) || is_method_named(ast, id, "join") {
        return true;
    }
    static_value_no_side_effects(cx, id).is_some_and(|v| !matches!(v, Static::Array(_)))
}

/// `isKnownNonArrayConstruction`: `new X()` (directly or as a `const` initializer) with `X`
/// not the global `Array`.
fn is_known_non_array_construction(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let id = skip_parens(ast, id);
    let node = const_definition(cx, id).map_or(id, |(_, init)| skip_parens(ast, init));
    match *ast.node(node) {
        Node::New { callee, .. } => {
            matches!(ast.node(callee), Node::Ident { .. }) && !is_global(cx, callee, "Array")
        }
        _ => false,
    }
}

fn is_typed_array_construction(ast: &Ast, id: NodeId) -> bool {
    let is_ctor = |name: Option<&str>| {
        name.is_some_and(|n| {
            TYPED_ARRAYS.contains(&n) || n == "ArrayBuffer" || n == "SharedArrayBuffer"
        })
    };
    match *ast.node(id) {
        Node::Call {
            callee,
            optional: false,
            ..
        } => {
            matches!(*ast.node(skip_parens(ast, callee)), Node::Member { obj, prop, optional: false }
            if matches!(ast.node(obj), Node::Ident { .. })
                && is_ctor(ident_name(ast, obj))
                && matches!(ident_name(ast, prop), Some("from" | "of")))
        }
        Node::New { callee, .. } => {
            matches!(ast.node(callee), Node::Ident { .. }) && is_ctor(ident_name(ast, callee))
        }
        _ => false,
    }
}

/// The import specifier binding `id` when it is imported from `buffer` / `node:buffer`.
fn buffer_import(cx: &Ctx<'_, '_>, id: NodeId) -> Option<Decl> {
    let ast = cx.ast();
    if !matches!(ast.node(id), Node::Ident { .. }) {
        return None;
    }
    let sem = cx.semantic();
    let sym = sem.symbol(sem.symbol_of_ident(id)?);
    let [decl] = sym.decls.as_slice() else {
        return None;
    };
    let Node::ImportDecl { source, .. } = *ast.node(decl.parent) else {
        return None;
    };
    let Node::StringLit { value } = *ast.node(source) else {
        return None;
    };
    matches!(ast.atom(value), "buffer" | "node:buffer").then_some(*decl)
}

fn is_buffer_reference(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let id = unwrap_ts(ast, id);
    match *ast.node(id) {
        Node::Member { obj, prop, .. } if ident_name(ast, prop) == Some("Buffer") => {
            let obj = unwrap_ts(ast, obj);
            if ["globalThis", "window", "self", "global"]
                .iter()
                .any(|n| is_global(cx, obj, n))
            {
                return true;
            }
            buffer_import(cx, obj).is_some_and(|d| {
                matches!(d.kind, DeclKind::ImportNamespace | DeclKind::ImportDefault)
            })
        }
        Node::Ident { .. } => {
            if is_global(cx, id, "Buffer") {
                return true;
            }
            let Some(d) = buffer_import(cx, id) else {
                return false;
            };
            let Node::ImportSpecifier {
                imported, local, ..
            } = *ast.node(d.node)
            else {
                return false;
            };
            ident_name(ast, imported.unwrap_or(local)) == Some("Buffer")
        }
        _ => false,
    }
}

fn is_known_non_array_call(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    if let Some(m) = method_call(cx, id, "concat", None)
        && is_buffer_reference(cx, m.obj)
    {
        return true;
    }
    match *ast.node(id) {
        Node::Call { callee, .. } => ["String", "Number", "Boolean", "BigInt", "RegExp"]
            .iter()
            .any(|n| is_global(cx, callee, n)),
        _ => false,
    }
}

fn is_global_array_reference(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let id = skip_parens(ast, id);
    if is_global(cx, id, "Array") {
        return true;
    }
    let (obj, name) = match *ast.node(id) {
        Node::Member { obj, prop, .. } => (obj, ident_name(ast, prop).map(str::to_owned)),
        Node::Index { obj, index, .. } => (
            obj,
            match static_value_no_side_effects(cx, index) {
                Some(Static::Str(s)) => Some(s),
                _ => None,
            },
        ),
        _ => return false,
    };
    let obj = skip_parens(ast, obj);
    name.as_deref() == Some("Array")
        && (is_global(cx, obj, "global") || is_global(cx, obj, "globalThis"))
}

fn is_array_constructor_call(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    match *ast.node(unwrap_ts(ast, id)) {
        Node::Call { callee, .. } | Node::New { callee, .. } => {
            is_global_array_reference(cx, callee)
        }
        _ => false,
    }
}

fn is_array_constructor_with_one_argument(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    match *ast.node(unwrap_ts(ast, id)) {
        Node::Call { callee, args, .. } | Node::New { callee, args, .. } => {
            ast.list(args).len() == 1 && is_global_array_reference(cx, callee)
        }
        _ => false,
    }
}

fn is_same_binding(cx: &Ctx<'_, '_>, a: NodeId, b: NodeId) -> bool {
    let sem = cx.semantic();
    match (sem.symbol_of_ident(a), sem.symbol_of_ident(b)) {
        (None, None) => ident_name(cx.ast(), a) == ident_name(cx.ast(), b),
        (x, y) => x == y,
    }
}

fn is_same_receiver(cx: &Ctx<'_, '_>, a: NodeId, b: NodeId, depth: u32) -> bool {
    let ast = cx.ast();
    let (a, b) = (unwrap_ts(ast, a), unwrap_ts(ast, b));
    if depth > 100 || !is_same_reference(cx, a, b) {
        return false;
    }
    match (*ast.node(a), *ast.node(b)) {
        (Node::Ident { .. }, Node::Ident { .. }) => is_same_binding(cx, a, b),
        (Node::Member { obj: x, .. }, Node::Member { obj: y, .. })
        | (Node::Member { obj: x, .. }, Node::Index { obj: y, .. })
        | (Node::Index { obj: x, .. }, Node::Member { obj: y, .. }) => {
            is_same_receiver(cx, x, y, depth + 1)
        }
        (
            Node::Index {
                obj: x, index: i, ..
            },
            Node::Index {
                obj: y, index: j, ..
            },
        ) => is_same_receiver(cx, x, y, depth + 1) && is_same_receiver(cx, i, j, depth + 1),
        _ => true,
    }
}

fn is_function(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(id),
        Node::FunctionDecl { .. } | Node::FunctionExpr { .. } | Node::Arrow { .. }
    )
}

/// `hasWriteBeforeNode`: an assignment, update or receiver declaration in `start` before
/// `end` (not inside functions).
fn has_write_before(cx: &Ctx<'_, '_>, start: NodeId, end: NodeId) -> bool {
    let ast = cx.ast();
    let lo = cx.span(start).start;
    let hi = cx.span(end).start;
    let mut stack = vec![start];
    let mut guard = 0;
    while let Some(n) = stack.pop() {
        guard += 1;
        if guard > 1_000_000 {
            return true;
        }
        let span = cx.span(n);
        if span.end <= lo || span.start >= hi || is_function(ast, n) {
            continue;
        }
        if span.end <= hi {
            let write = match *ast.node(n) {
                Node::Assign { .. } | Node::Update { .. } => true,
                Node::VarDeclarator { name, init, .. } => {
                    init.is_some()
                        && (!matches!(ast.node(name), Node::Ident { .. })
                            || !matches!(ast.node(end), Node::Ident { .. })
                            || ident_name(ast, name) == ident_name(ast, end))
                }
                _ => false,
            };
            if write {
                return true;
            }
        }
        ast.for_each_child(n, |c| stack.push(c));
    }
    false
}

fn array_is_array_state(cx: &Ctx<'_, '_>, test: NodeId, arg: NodeId) -> Option<bool> {
    let ast = cx.ast();
    let test = skip_parens(ast, test);
    let is_call = |id: NodeId| {
        method_call(cx, skip_parens(ast, id), "isArray", Some(1)).is_some_and(|m| {
            m.args.len() == 1
                && is_global(cx, m.obj, "Array")
                && is_same_receiver(cx, m.args[0], arg, 0)
        })
    };
    if is_call(test) {
        return Some(true);
    }
    match *ast.node(test) {
        Node::Unary {
            op: UnaryOp::Not,
            arg: inner,
        } if is_call(inner) => Some(false),
        _ => None,
    }
}

fn is_inside(cx: &Ctx<'_, '_>, node: NodeId, outer: NodeId) -> bool {
    let (a, b) = (cx.span(node), cx.span(outer));
    a.start >= b.start && a.end <= b.end
}

fn is_not_array_by_test(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let mut cur = cx.parent(node);
    while let Some(a) = cur {
        if is_function(ast, a) {
            break;
        }
        if let Node::If { test, cons, alt } = *ast.node(a) {
            let state = array_is_array_state(cx, test, node);
            let branch = if state == Some(false) {
                Some(cons)
            } else {
                alt
            };
            if let Some(branch) = branch
                && is_inside(cx, node, branch)
                && !has_write_before(cx, branch, node)
                && state.is_some()
            {
                return true;
            }
        }
        cur = cx.parent(a);
    }
    false
}

fn syntactic_array_state(cx: &Ctx<'_, '_>, id: NodeId) -> Option<bool> {
    let ast = cx.ast();
    if matches!(ast.node(id), Node::ArrayLit { .. }) || is_array_constructor_call(cx, id) {
        return Some(true);
    }
    let non_array = is_not_array(cx, id)
        || matches!(
            ast.node(id),
            Node::ObjectLit { .. }
                | Node::FunctionExpr { .. }
                | Node::Arrow { .. }
                | Node::ClassExpr { .. }
        )
        || is_known_non_array_call(cx, id)
        || is_known_non_array_construction(cx, id)
        || is_not_array_by_test(cx, id);
    if non_array { Some(false) } else { None }
}

fn combine(states: &[Option<bool>], all: bool) -> Option<bool> {
    let hit = if all {
        states.iter().all(|s| *s == Some(true))
    } else {
        states.contains(&Some(true))
    };
    if hit {
        Some(true)
    } else if states.iter().any(|s| s.is_none() || *s == Some(true)) {
        None
    } else {
        Some(false)
    }
}

const KNOWN_NON_ARRAY: &[&str] = &["ApolloLink", "Buffer"];

fn rightmost_name(ast: &Ast, id: NodeId) -> Option<&str> {
    match *ast.node(id) {
        Node::QualifiedName { right, .. } => rightmost_name(ast, right),
        _ => ident_name(ast, id),
    }
}

fn leftmost(ast: &Ast, id: NodeId) -> NodeId {
    match *ast.node(id) {
        Node::QualifiedName { left, .. } => leftmost(ast, left),
        _ => id,
    }
}

/// `getTypeAnnotationArrayState`.
fn annotation_state(
    cx: &Ctx<'_, '_>,
    ty: Option<NodeId>,
    visited: &mut Vec<SymbolId>,
) -> Option<bool> {
    let ast = cx.ast();
    let ty = ty?;
    if visited.len() > 100 {
        return None;
    }
    match *ast.node(ty) {
        Node::KeywordType { kind } => match kind {
            KeywordTypeKind::Any | KeywordTypeKind::Unknown => None,
            KeywordTypeKind::Never
            | KeywordTypeKind::Void
            | KeywordTypeKind::Null
            | KeywordTypeKind::Undefined
            | KeywordTypeKind::String
            | KeywordTypeKind::Number
            | KeywordTypeKind::Boolean
            | KeywordTypeKind::BigInt
            | KeywordTypeKind::Symbol
            | KeywordTypeKind::Object => Some(false),
            _ => None,
        },
        Node::ParenType { ty } => annotation_state(cx, Some(ty), visited),
        Node::ArrayType { .. } | Node::TupleType { .. } => Some(true),
        Node::TypeOperator { op, ty } => {
            if op == TypeOperatorKind::Readonly {
                annotation_state(cx, Some(ty), visited)
            } else {
                Some(false)
            }
        }
        Node::TypeRef { name, .. } => type_reference_state(cx, name, visited),
        Node::ImportType {
            qualifier: Some(q), ..
        } if rightmost_name(ast, q).is_some_and(|n| KNOWN_NON_ARRAY.contains(&n)) => Some(false),
        Node::UnionType { types } => {
            let states: Vec<_> = ast
                .list(types)
                .iter()
                .map(|&t| annotation_state(cx, Some(t), visited))
                .collect();
            combine(&states, true)
        }
        Node::IntersectionType { types } => {
            let states: Vec<_> = ast
                .list(types)
                .iter()
                .map(|&t| annotation_state(cx, Some(t), visited))
                .collect();
            combine(&states, false)
        }
        Node::TypeLiteral { .. }
        | Node::FunctionType { .. }
        | Node::ConstructorType { .. }
        | Node::StringLit { .. }
        | Node::NumberLit { .. }
        | Node::BoolLit { .. }
        | Node::NullLit {}
        | Node::BigIntLit { .. }
        | Node::TemplateLit { .. }
        | Node::Unary { .. } => Some(false),
        _ => None,
    }
}

fn type_reference_state(
    cx: &Ctx<'_, '_>,
    name: NodeId,
    visited: &mut Vec<SymbolId>,
) -> Option<bool> {
    let ast = cx.ast();
    let sem = cx.semantic();
    if matches!(ast.node(name), Node::QualifiedName { .. }) {
        if !rightmost_name(ast, name).is_some_and(|n| KNOWN_NON_ARRAY.contains(&n)) {
            return None;
        }
        let ns = leftmost(ast, name);
        let ns_name = ident_name(ast, ns)?;
        let sym = sem.lookup(sem.enclosing_scope(ns), ns_name)?;
        let namespace_import = sem
            .symbol(sym)
            .decls
            .first()
            .is_some_and(|d| d.kind == DeclKind::ImportNamespace);
        return if namespace_import { Some(false) } else { None };
    }
    let type_name = ident_name(ast, name)?;
    if matches!(type_name, "Array" | "ReadonlyArray") {
        return Some(true);
    }
    let Some((sym, decl)) = type_definition(cx, type_name, sem.enclosing_scope(name)) else {
        return KNOWN_NON_ARRAY.contains(&type_name).then_some(false);
    };
    if visited.contains(&sym) {
        return None;
    }
    visited.push(sym);
    let state = match decl.kind {
        DeclKind::Class | DeclKind::Interface => Some(false),
        DeclKind::ImportNamed | DeclKind::ImportDefault | DeclKind::ImportNamespace => {
            let imported = match *ast.node(decl.node) {
                Node::ImportSpecifier {
                    imported: Some(i), ..
                } if matches!(ast.node(i), Node::Ident { .. }) => ident_name(ast, i),
                _ => Some(type_name),
            };
            imported
                .is_some_and(|n| KNOWN_NON_ARRAY.contains(&n))
                .then_some(false)
        }
        DeclKind::TypeAlias => match *ast.node(decl.node) {
            Node::TypeAlias { ty, .. } => annotation_state(cx, Some(ty), visited),
            _ => None,
        },
        DeclKind::TypeParam => match *ast.node(decl.node) {
            Node::TypeParam { constraint, .. } => annotation_state(cx, constraint, visited),
            _ => None,
        },
        _ => None,
    };
    visited.retain(|s| *s != sym);
    state
}

fn receiver_annotation_state(cx: &Ctx<'_, '_>, mut id: NodeId) -> Option<bool> {
    let ast = cx.ast();
    for _ in 0..10_000 {
        match *ast.node(id) {
            Node::As { expr, ty }
            | Node::Satisfies { expr, ty }
            | Node::TypeAssertion { expr, ty } => {
                if let Some(s) = annotation_state(cx, Some(ty), &mut Vec::new()) {
                    return Some(s);
                }
                id = expr;
            }
            Node::NonNull { expr } | Node::Instantiation { expr, .. } | Node::Paren { expr } => {
                id = expr
            }
            _ => break,
        }
    }
    if !matches!(ast.node(id), Node::Ident { .. }) {
        return None;
    }
    let sem = cx.semantic();
    let sym = sem.symbol(sem.symbol_of_ident(id)?);
    sym.decls
        .iter()
        .find_map(|d| annotation_state(cx, binding_annotation(cx, d.name).0, &mut Vec::new()))
}

/// `getConcatReceiverArrayState`.
fn receiver_array_state(cx: &Ctx<'_, '_>, id: NodeId) -> Option<bool> {
    receiver_annotation_state(cx, id).or_else(|| syntactic_array_state(cx, unwrap_ts(cx.ast(), id)))
}

// ---- concat ------------------------------------------------------------------------------

/// `isArrayConcatInLoopCall`: `x = x.concat(...)` in a loop, `x` a local `let`/`var`
/// initialised with `[]` outside it.
fn is_concat_in_loop(cx: &Ctx<'_, '_>, call: NodeId, m: &MethodCall<'_>) -> bool {
    let ast = cx.ast();
    if m.args.is_empty() {
        return false;
    }
    let mut child = call;
    while let Some(p) = cx.parent(child) {
        if matches!(
            ast.node(p),
            Node::Paren { .. }
                | Node::As { .. }
                | Node::Satisfies { .. }
                | Node::NonNull { .. }
                | Node::TypeAssertion { .. }
                | Node::Instantiation { .. }
        ) {
            child = p;
        } else {
            break;
        }
    }
    let Some(assign) = cx.parent(child) else {
        return false;
    };
    let Node::Assign {
        left,
        op: AssignOp::Assign,
        right,
    } = *ast.node(assign)
    else {
        return false;
    };
    if right != child || !matches!(ast.node(left), Node::Ident { .. }) {
        return false;
    }
    let receiver = unwrap_ts(ast, m.obj);
    if !matches!(ast.node(receiver), Node::Ident { .. }) {
        return false;
    }
    let sem = cx.semantic();
    let (Some(s), Some(r)) = (sem.symbol_of_ident(left), sem.symbol_of_ident(receiver)) else {
        return false;
    };
    if s != r {
        return false;
    }
    let symbol = sem.symbol(s);
    let [decl] = symbol.decls.as_slice() else {
        return false;
    };
    if !matches!(decl.kind, DeclKind::Let | DeclKind::Var)
        || matches!(
            sem.scope(symbol.scope).kind,
            crate::semantic::ScopeKind::Global
        )
    {
        return false;
    }
    let Node::VarDeclarator {
        name,
        init: Some(init),
        ..
    } = *ast.node(decl.node)
    else {
        return false;
    };
    if name != decl.name || !is_empty_array(ast, unwrap_ts(ast, init)) {
        return false;
    }
    let mut cur = cx.parent(assign);
    while let Some(a) = cur {
        if is_function(ast, a) {
            return false;
        }
        let body = match *ast.node(a) {
            Node::DoWhile { body, .. }
            | Node::ForIn { body, .. }
            | Node::ForOf { body, .. }
            | Node::For { body, .. }
            | Node::While { body, .. } => Some(body),
            _ => None,
        };
        if let Some(body) = body {
            return !is_inside(cx, decl.node, body);
        }
        cur = cx.parent(a);
    }
    false
}

#[derive(Clone, Copy)]
struct ConcatArg {
    node: NodeId,
    array_literal: bool,
    spreadable: bool,
    test: bool,
}

fn concat_arg(cx: &Ctx<'_, '_>, id: NodeId) -> Option<ConcatArg> {
    let ast = cx.ast();
    let id = skip_parens(ast, id);
    if is_spread(ast, id) || is_array_constructor_with_one_argument(cx, id) {
        return None;
    }
    if matches!(ast.node(id), Node::ArrayLit { .. }) {
        return Some(ConcatArg {
            node: id,
            array_literal: true,
            spreadable: false,
            test: false,
        });
    }
    let v = static_value_no_side_effects(cx, id)?;
    Some(ConcatArg {
        node: id,
        array_literal: false,
        spreadable: matches!(v, Static::Array(_)),
        test: false,
    })
}

fn fixable_args(cx: &Ctx<'_, '_>, args: &[NodeId]) -> Vec<ConcatArg> {
    args.iter().map_while(|&a| concat_arg(cx, a)).collect()
}

/// The literal has parentheses with comments between them and the brackets.
fn outer_comments_preservable(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    if has_holes(cx.ast(), id) {
        return false;
    }
    let outer = paren_span(cx, id);
    let inner = cx.span(id);
    outer != inner
        && (has_comment_in(cx, Span::new(outer.start, inner.start))
            || has_comment_in(cx, Span::new(inner.end, outer.end)))
}

fn elements_text(cx: &Ctx<'_, '_>, id: NodeId, keep_trailing_comma: bool) -> String {
    let span = cx.span(id);
    let src = cx.src();
    let end = match trailing_comma(cx, id) {
        Some(comma) if !keep_trailing_comma => comma,
        _ => span.end - 1,
    };
    src[span.start as usize + 1..end as usize].to_owned()
}

fn comma_after(cx: &Ctx<'_, '_>, id: NodeId) -> Option<u32> {
    let end = paren_span(cx, id).end as usize;
    let pos = skip_trivia_forward(cx.src(), end);
    (cx.src().as_bytes().get(pos) == Some(&b',')).then_some(pos as u32)
}

fn concat_fix(cx: &Ctx<'_, '_>, call: NodeId, m: &MethodCall<'_>, fixable: &[ConcatArg]) -> Fix {
    let ast = cx.ast();
    let src = cx.src();
    let array = m.obj;
    let array_is_literal = matches!(ast.node(array), Node::ArrayLit { .. });
    let array_trailing = array_is_literal && trailing_comma(cx, array).is_some();
    let mut edits = Vec::new();
    if !array_is_literal && needs_semicolon(cx, cx.span(call).start, "[") {
        edits.push((Span::at(cx.span(call).start), ";".to_owned()));
    }
    if m.args.len() == fixable.len() {
        edits.extend(remove_method_call(cx, call, m));
    } else if let (Some(first), Some(last)) = (m.args.first(), fixable.last()) {
        let start = paren_span(cx, *first).start;
        let mut end = comma_after(cx, last.node).map_or(paren_span(cx, last.node).end, |c| c + 1);
        let rest = &src[end as usize..];
        end += (rest.len() - rest.trim_start().len()) as u32;
        edits.push((Span::new(start, end), String::new()));
    }
    let non_empty: Vec<&ConcatArg> = fixable
        .iter()
        .filter(|a| !a.array_literal || !is_empty_array(ast, a.node))
        .collect();
    let last = non_empty.last().map(|a| a.node);
    let parts: Vec<String> = non_empty
        .iter()
        .map(|a| {
            if a.array_literal {
                return if outer_comments_preservable(cx, a.node) {
                    format!("...{}", paren_text(cx, a.node))
                } else {
                    elements_text(cx, a.node, Some(a.node) == last)
                };
            }
            let text = paren_text(cx, a.node);
            if a.test {
                format!("...(Array.isArray({text}) ? {text} : [{text}])")
            } else if a.spreadable {
                format!("...{text}")
            } else {
                text.to_owned()
            }
        })
        .collect();
    let mut text = parts.join(", ");
    if !text.is_empty() {
        if array_is_literal {
            if !is_empty_array(ast, array) {
                text = format!(" {text}");
                if !array_trailing {
                    text.insert(0, ',');
                }
                let last_arg = non_empty.last();
                if array_trailing
                    && last_arg
                        .is_some_and(|l| !l.array_literal || trailing_comma(cx, l.node).is_none())
                {
                    text.push(',');
                }
            }
        } else {
            text = format!(", {text}");
        }
    }
    if array_is_literal {
        let close = cx.span(array).end - 1;
        edits.push((Span::at(close), text));
    } else {
        let range = paren_span(cx, array);
        edits.push((Span::at(range.start), "[...".to_owned()));
        edits.push((Span::at(range.end), text));
        edits.push((Span::at(range.end), "]".to_owned()));
    }
    build(edits)
}

fn concat_preserved(cx: &Ctx<'_, '_>, call: NodeId, m: &MethodCall<'_>, count: usize) -> Vec<Span> {
    let ast = cx.ast();
    let fixed: Vec<NodeId> = m.args[..count]
        .iter()
        .map(|&a| skip_parens(ast, a))
        .collect();
    let last_non_empty = fixed
        .iter()
        .rev()
        .find(|&&a| !matches!(ast.node(a), Node::ArrayLit { .. }) || !is_empty_array(ast, a))
        .copied();
    let mut ranges = vec![paren_span(cx, m.obj)];
    for &a in &fixed {
        if matches!(ast.node(a), Node::ArrayLit { .. }) {
            if is_empty_array(ast, a) {
                continue;
            }
            if outer_comments_preservable(cx, a) {
                ranges.push(paren_span(cx, a));
                continue;
            }
            let span = cx.span(a);
            if let Some(comma) = trailing_comma(cx, a)
                && Some(a) != last_non_empty
            {
                ranges.push(Span::new(span.start + 1, comma));
                continue;
            }
            if has_holes(ast, a) {
                ranges.push(span);
                continue;
            }
        }
        ranges.push(paren_span(cx, a));
    }
    if count < m.args.len()
        && let Some(&last) = fixed.last()
        && let Some(comma) = comma_after(cx, last)
    {
        ranges.push(Span::new(comma + 1, cx.span(call).end));
    }
    ranges
}

fn is_static_string(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    matches!(static_value_no_side_effects(cx, id), Some(Static::Str(_)))
}

fn check_concat(cx: &mut Ctx<'_, '_>, node: NodeId) {
    let ast = cx.ast();
    let Some(m) = method_call(cx, node, "concat", None) else {
        return;
    };
    if is_concat_in_loop(cx, node, &m) {
        return;
    }
    let state = receiver_array_state(cx, m.obj);
    if state == Some(false) {
        return;
    }
    if state != Some(true) && !m.args.is_empty() && m.args.iter().all(|&a| is_static_string(cx, a))
    {
        return;
    }
    let msg = "Prefer the spread operator over `Array#concat(…)`.";
    let fixable = fixable_args(cx, m.args);
    let receiver_safe = !mutable_member_access(cx, m.obj, &mut Vec::new())
        && !is_array_constructor_with_one_argument(cx, m.obj);
    if !fixable.is_empty() || m.args.is_empty() {
        let fix = (receiver_safe
            && !would_remove_comments(
                cx,
                cx.span(node),
                &concat_preserved(cx, node, &m, fixable.len()),
            ))
        .then(|| concat_fix(cx, node, &m, &fixable));
        let suggest = m.args.iter().any(|&a| has_possible_side_effect(cx, a)) || is_ts_file(cx);
        report_spread(cx, m.prop, "array-concat", msg, fix, suggest);
        return;
    }
    let first = skip_parens(ast, m.args[0]);
    let rest = &m.args[1..];
    if is_spread(ast, first) || !receiver_safe || is_array_constructor_with_one_argument(cx, first)
    {
        cx.report_node(m.prop, msg).id("array-concat");
        return;
    }
    let after = fixable_args(cx, rest);
    let unsafe_rest = rest
        .iter()
        .any(|&a| is_array_constructor_with_one_argument(cx, a));
    let with_first = |spreadable: bool, test: bool| {
        let mut args = vec![ConcatArg {
            node: first,
            array_literal: false,
            spreadable,
            test,
        }];
        args.extend(after.iter().copied());
        concat_fix(cx, node, &m, &args)
    };
    let mut suggestions = vec![
        (
            "argument-is-spreadable",
            "First argument is an `array`.",
            with_first(true, false),
        ),
        (
            "argument-is-not-spreadable",
            "First argument is not an `array`.",
            with_first(false, false),
        ),
    ];
    if !has_side_effect(ast, first) {
        suggestions.push((
            "test-argument",
            "Test first argument with `Array.isArray(…)`.",
            with_first(false, true),
        ));
    }
    if !unsafe_rest && after.len() < rest.len() && !rest.iter().any(|&a| is_spread(ast, a)) {
        let all: Vec<ConcatArg> = m
            .args
            .iter()
            .map(|&a| {
                concat_arg(cx, a).unwrap_or(ConcatArg {
                    node: skip_parens(ast, a),
                    array_literal: false,
                    spreadable: true,
                    test: false,
                })
            })
            .collect();
        suggestions.push((
            "spread-all-arguments",
            "Spread all unknown arguments.",
            concat_fix(cx, node, &m, &all),
        ));
    }
    let mut report = cx.report_node(m.prop, msg).id("array-concat");
    for (id, desc, fix) in suggestions {
        report = report.suggest(id, desc, fix);
    }
}

// ---- slice / toSpliced -------------------------------------------------------------------

fn check_slice(cx: &mut Ctx<'_, '_>, node: NodeId, method: &str) {
    let ast = cx.ast();
    let max = if method == "slice" { 1 } else { 0 };
    let Some(m) = method_call(cx, node, method, Some(max)) else {
        return;
    };
    let obj = m.obj;
    let ignored = match *ast.node(obj) {
        Node::This {} => true,
        Node::Ident { .. } => matches!(
            ident_name(ast, obj),
            Some("arrayBuffer" | "blob" | "buffer" | "file" | "this")
        ),
        _ => false,
    };
    if matches!(ast.node(obj), Node::ArrayLit { .. })
        || has_unparenthesized_optional_chain(cx, obj)
        || ignored
        || is_not_array(cx, obj)
        || is_known_non_array_construction(cx, obj)
        || is_typed_array_construction(ast, obj)
    {
        return;
    }
    if let Some(&first) = m.args.first()
        && !matches!(*ast.node(skip_parens(ast, first)), Node::NumberLit { value } if value.get() == 0.0)
    {
        return;
    }
    let fix = (!would_remove_comments(cx, cx.span(node), &[paren_span(cx, obj)])).then(|| {
        let span = cx.span(node);
        let mut edits = Vec::new();
        if needs_semicolon(cx, span.start, "[") {
            edits.push((Span::at(span.start), ";".to_owned()));
        }
        edits.push((Span::at(span.start), "[...".to_owned()));
        edits.push((Span::at(span.end), "]".to_owned()));
        edits.extend(remove_method_call(cx, node, &m));
        build(edits)
    });
    let (id, msg) = if method == "slice" {
        (
            "array-slice",
            "Prefer the spread operator over `Array#slice()`.",
        )
    } else {
        (
            "array-to-spliced",
            "Prefer the spread operator over `Array#toSpliced()`.",
        )
    };
    let suggest = is_ts_file(cx);
    report_spread(cx, m.prop, id, msg, fix, suggest);
}

// ---- for…of copies -----------------------------------------------------------------------

/// The statement before `node` in its statement list.
fn previous_statement(cx: &Ctx<'_, '_>, node: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let list = match *ast.node(cx.parent(node)?) {
        Node::Block { stmts } | Node::Program { stmts } | Node::ModuleBlock { stmts } => stmts,
        Node::SwitchCase { body, .. } => body,
        Node::StaticBlock { body } => body,
        _ => return None,
    };
    let list = ast.list(list);
    let i = list.iter().position(|&s| s == node)?;
    i.checked_sub(1).map(|j| list[j])
}

/// `const a = [];` / `let a = [];`: the declarator.
fn empty_array_declarator(ast: &Ast, decl: NodeId) -> Option<(NodeId, VarKind)> {
    let Node::VarDecl { kind, decls, .. } = *ast.node(decl) else {
        return None;
    };
    if !matches!(kind, VarKind::Const | VarKind::Let) {
        return None;
    }
    let &[d] = ast.list(decls) else {
        return None;
    };
    let Node::VarDeclarator {
        name,
        init: Some(init),
        ..
    } = *ast.node(d)
    else {
        return None;
    };
    (matches!(ast.node(name), Node::Ident { .. }) && is_empty_array(ast, skip_parens(ast, init)))
        .then_some((d, kind))
}

/// `for (const x of ...)` → `(x, None)`; `for (const [i, x] of ...)` → `(x, Some(i))`.
fn for_of_binding(ast: &Ast, left: NodeId) -> Option<(NodeId, Option<NodeId>)> {
    let Node::VarDecl { kind, decls, .. } = *ast.node(left) else {
        return None;
    };
    if !matches!(kind, VarKind::Const | VarKind::Let) {
        return None;
    }
    let &[d] = ast.list(decls) else {
        return None;
    };
    let Node::VarDeclarator { name, .. } = *ast.node(d) else {
        return None;
    };
    match *ast.node(name) {
        Node::Ident { .. } => Some((name, None)),
        Node::ArrayPattern { elements } => match ast.list(elements) {
            &[i, e]
                if matches!(ast.node(i), Node::Ident { .. })
                    && matches!(ast.node(e), Node::Ident { .. }) =>
            {
                Some((e, Some(i)))
            }
            _ => None,
        },
        _ => None,
    }
}

fn only_expression(ast: &Ast, body: NodeId) -> Option<NodeId> {
    let stmt = match *ast.node(body) {
        Node::Block { stmts } => match ast.list(stmts) {
            &[s] => s,
            _ => return None,
        },
        _ => body,
    };
    match *ast.node(stmt) {
        Node::ExprStmt { expr } => Some(skip_parens(ast, expr)),
        _ => None,
    }
}

fn named(ast: &Ast, id: NodeId, name: &str) -> bool {
    let id = skip_parens(ast, id);
    matches!(ast.node(id), Node::Ident { .. }) && ident_name(ast, id) == Some(name)
}

fn for_of_iterable(cx: &Ctx<'_, '_>, node: NodeId, array: &str) -> Option<NodeId> {
    let ast = cx.ast();
    let Node::ForOf {
        left, right, body, ..
    } = *ast.node(node)
    else {
        return None;
    };
    let expr = only_expression(ast, body)?;
    let (element, index) = for_of_binding(ast, left)?;
    let element_name = ident_name(ast, element)?;
    if element_name == array || index.is_some_and(|i| ident_name(ast, i) == Some(array)) {
        return None;
    }
    if index.is_none()
        && let Some(m) = method_call(cx, expr, "push", Some(1))
        && let &[arg] = m.args
        && named(ast, m.obj, array)
        && named(ast, arg, element_name)
    {
        return Some(right);
    }
    let index = index?;
    let m = method_call(cx, skip_parens(ast, right), "entries", Some(0))?;
    if !m.args.is_empty() || receiver_array_state(cx, m.obj) != Some(true) {
        return None;
    }
    let Node::Assign {
        left,
        op: AssignOp::Assign,
        right: value,
    } = *ast.node(expr)
    else {
        return None;
    };
    let Node::Index {
        obj, index: key, ..
    } = *ast.node(skip_parens(ast, left))
    else {
        return None;
    };
    (named(ast, obj, array)
        && named(ast, key, ident_name(ast, index)?)
        && named(ast, value, element_name))
    .then_some(m.obj)
}

fn mentions(ast: &Ast, id: NodeId, name: &str) -> bool {
    let mut stack = vec![id];
    let mut guard = 0;
    while let Some(n) = stack.pop() {
        guard += 1;
        if guard > 1_000_000 {
            return true;
        }
        if matches!(ast.node(n), Node::Ident { .. }) && ident_name(ast, n) == Some(name) {
            return true;
        }
        ast.for_each_child(n, |c| stack.push(c));
    }
    false
}

/// The function (or the program) whose variables `id` belongs to, and whether it is a
/// function declaration.
fn variable_scope(cx: &Ctx<'_, '_>, id: NodeId) -> (Option<NodeId>, bool) {
    let ast = cx.ast();
    let mut cur = cx.parent(id);
    while let Some(c) = cur {
        match ast.node(c) {
            Node::FunctionDecl { .. } => return (Some(c), true),
            Node::FunctionExpr { .. }
            | Node::Arrow { .. }
            | Node::PropertyDef { .. }
            | Node::StaticBlock { .. } => return (Some(c), false),
            _ => {}
        }
        cur = cx.parent(c);
    }
    (None, false)
}

fn referenced_by_nested_function(cx: &Ctx<'_, '_>, name: NodeId, loop_node: NodeId) -> bool {
    let sem = cx.semantic();
    let Some(sym) = sem.symbol_of_decl(name) else {
        return false;
    };
    let own = variable_scope(cx, name).0;
    let loop_end = cx.span(loop_node).end;
    sem.symbol(sym).refs.iter().any(|&r| {
        let ident = sem.reference(r).node;
        let (scope, declaration) = variable_scope(cx, ident);
        scope != own && (cx.span(ident).start < loop_end || declaration)
    })
}

fn check_for_of(cx: &mut Ctx<'_, '_>, node: NodeId) {
    let ast = cx.ast();
    let Node::ForOf {
        is_await: false, ..
    } = *ast.node(node)
    else {
        return;
    };
    let Some(declaration) = previous_statement(cx, node) else {
        return;
    };
    let Some((declarator, kind)) = empty_array_declarator(ast, declaration) else {
        return;
    };
    let Node::VarDeclarator { name, type_ann, .. } = *ast.node(declarator) else {
        return;
    };
    let Some(array) = ident_name(ast, name) else {
        return;
    };
    let Some(iterable) = for_of_iterable(cx, node, array) else {
        return;
    };
    if mentions(ast, iterable, array) {
        return;
    }
    let iterable_range = paren_span(cx, iterable);
    let replace = Span::new(cx.span(declaration).start, cx.span(node).end);
    let fix = (!would_remove_comments(cx, replace, &[iterable_range])).then(|| {
        let id_end = type_ann.map_or(cx.span(name).end, |t| cx.span(t).end);
        let id_text = &cx.src()[cx.span(name).start as usize..id_end as usize];
        let kind = if kind == VarKind::Const {
            "const"
        } else {
            "let"
        };
        let iterable_text = &cx.src()[iterable_range.start as usize..iterable_range.end as usize];
        Fix::replace(replace, format!("{kind} {id_text} = [...{iterable_text}];"))
    });
    let suggest = referenced_by_nested_function(cx, name, node);
    report_spread(
        cx,
        node,
        "for-of",
        "Prefer the spread operator over this `for…of` loop.",
        fix,
        suggest,
    );
}
