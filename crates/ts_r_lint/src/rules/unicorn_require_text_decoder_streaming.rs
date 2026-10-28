//! `unicorn/require-text-decoder-streaming`: chunks of a fetch body decoded in a loop with
//! a `TextDecoder` that is not streaming.
//!
//! A `decode(chunk[, options])` call (callee through parentheses and TS wrappers, `?.`
//! allowed) inside a loop (not across a function), whose options are missing, statically
//! nullish, or a plain object literal (no spread, accessor, method, unknown or `__proto__`
//! key) whose last `stream` is missing or statically falsy, on a receiver that resolves to
//! `new TextDecoder(...)`, with a chunk that is a fetch-body chunk of that loop:
//!
//! - the `for await` binding over `(await fetch(...)).body` (or its `.values(x?)`), or
//! - `value` of (or destructured from) `await reader.read()` awaited in that loop, where
//!   `reader` is `<fetch body>.getReader()`.
//!
//! Names are followed through `const` declarations in the same function declared before
//! use and never reassigned. `fetch` is the bare name or `globalThis.fetch`, with one or two
//! arguments.
//!
//! The suggestion needs the simple shapes: `text += decoder.decode(chunk, opts?)` as the
//! only statement of a `for await` body over the loop binding, or the last of exactly
//! three statements of `while (true)` after `const {value, done} = await reader.read()` and
//! `if (done) break;`; the loop sits directly in a block or the program, has no comments
//! inside or right after it on its last line; `decoder` is a `const new TextDecoder(...)`
//! and `text` a `let` initialised to `''`, both declared earlier in the loop's block, used
//! nowhere else (apart from a `text += decoder.decode()` right after the loop). It sets
//! `stream: true` (options missing, `{}`, or `{stream: false}`) and adds that flush when it
//! is missing.

use super::prelude::*;
use super::unicorn_ast::linebreak;
use super::unicorn_static::{static_string_literal, static_value, static_value_control_flow};
use super::unicorn_text::has_comment_in;
use super::unicorn_util::line_indent;
use super::unicorn_util::skip_wrappers;
use crate::semantic::Decl;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/require-text-decoder-streaming",
        "Require streaming decoding of fetch-body chunks with `TextDecoder`.",
        NO_PRESET,
        Category::Correctness,
    )
    .suggestions(),
    create: create_default::<RequireTextDecoderStreaming>,
};

#[derive(Debug, Default)]
struct RequireTextDecoderStreaming;

const MAX_DEPTH: u32 = 64;

fn is_function(node: &Node) -> bool {
    matches!(
        node,
        Node::FunctionDecl { .. } | Node::FunctionExpr { .. } | Node::Arrow { .. }
    )
}

/// The enclosing function (`None` for the program).
fn function_of(cx: &Ctx<'_, '_>, mut id: NodeId) -> Option<NodeId> {
    while let Some(p) = cx.parent(id) {
        if is_function(cx.node(p)) {
            return Some(p);
        }
        id = p;
    }
    None
}

/// The nearest enclosing loop that is not across a function.
fn loop_of(cx: &Ctx<'_, '_>, mut id: NodeId) -> Option<NodeId> {
    while let Some(p) = cx.parent(id) {
        match cx.node(p) {
            n if is_function(n) => return None,
            Node::ForOf { .. } | Node::For { .. } | Node::While { .. } | Node::DoWhile { .. } => {
                return Some(p);
            }
            _ => id = p,
        }
    }
    None
}

fn name_is(cx: &Ctx<'_, '_>, id: NodeId, name: &str) -> bool {
    matches!(cx.node(id), Node::Ident { .. }) && ident_name(cx.ast(), id) == Some(name)
}

/// The receiver and arguments of `<obj>.<name>(...)` (callee through wrappers).
fn method<'a>(cx: &Ctx<'a, '_>, call: NodeId, name: &str) -> Option<(NodeId, &'a [NodeId])> {
    let ast = cx.ast();
    let Node::Call { callee, args, .. } = *ast.node(call) else {
        return None;
    };
    let Node::Member { obj, prop, .. } = *ast.node(skip_wrappers(ast, callee)) else {
        return None;
    };
    name_is(cx, prop, name).then(|| (obj, ast.list(args)))
}

fn no_spread(cx: &Ctx<'_, '_>, args: &[NodeId], n: usize) -> bool {
    args.iter()
        .take(n)
        .all(|&a| !matches!(cx.node(a), Node::Spread { .. }))
}

/// A variable declared once, in the same function as `node` (a name, through parentheses).
fn binding(cx: &Ctx<'_, '_>, node: NodeId) -> Option<(SymbolId, Decl)> {
    let node = skip_parens(cx.ast(), node);
    if !matches!(cx.node(node), Node::Ident { .. }) {
        return None;
    }
    let sem = cx.semantic();
    let sym = sem.reference(sem.reference_at(node)?).symbol()?;
    let [decl] = sem.symbol(sym).decls[..] else {
        return None;
    };
    if !matches!(
        decl.kind,
        DeclKind::Var | DeclKind::Let | DeclKind::Const | DeclKind::Using
    ) || !matches!(cx.node(decl.node), Node::VarDeclarator { .. })
        || function_of(cx, decl.node) != function_of(cx, node)
    {
        return None;
    }
    Some((sym, decl))
}

fn is_kind(cx: &Ctx<'_, '_>, decl: &Decl, kind: VarKind) -> bool {
    matches!(*cx.node(decl.parent), Node::VarDecl { kind: k, .. } if k == kind)
}

/// A [`binding`] that is a never-reassigned `const`.
fn const_binding(cx: &Ctx<'_, '_>, node: NodeId) -> Option<(SymbolId, Decl)> {
    let (sym, decl) = binding(cx, node)?;
    let sem = cx.semantic();
    let reassigned = sem.symbol(sym).refs.iter().any(|&r| {
        let r = sem.reference(r);
        r.is_write() && !r.flags.contains(RefFlags::INIT)
    });
    (is_kind(cx, &decl, VarKind::Const) && !reassigned).then_some((sym, decl))
}

/// `node` through wrappers and earlier `const name = init` aliases; the name itself when it
/// is destructured or has no initialiser; `None` when a name can't be followed.
fn resolve(cx: &Ctx<'_, '_>, mut node: NodeId) -> Option<NodeId> {
    for _ in 0..MAX_DEPTH {
        let n = skip_wrappers(cx.ast(), node);
        if !matches!(cx.node(n), Node::Ident { .. }) {
            return Some(n);
        }
        let (_, decl) = const_binding(cx, n)?;
        let Node::VarDeclarator { name, init, .. } = *cx.node(decl.node) else {
            return None;
        };
        let Some(init) = init else {
            return Some(n);
        };
        if !matches!(cx.node(name), Node::Ident { .. }) {
            return Some(n);
        }
        if cx.span(decl.node).end > cx.span(n).start {
            return None;
        }
        node = init;
    }
    None
}

/// `(await fetch(...)).body` or `(await globalThis.fetch(...)).body`.
fn is_fetch_body(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let Some(n) = resolve(cx, node) else {
        return false;
    };
    let Node::Member { obj, prop, .. } = *ast.node(n) else {
        return false;
    };
    if !name_is(cx, prop, "body") {
        return false;
    }
    let Some(Node::Await { arg }) = resolve(cx, obj).map(|r| *ast.node(r)) else {
        return false;
    };
    let Node::Call { callee, args, .. } = *ast.node(skip_wrappers(ast, arg)) else {
        return false;
    };
    let args = ast.list(args);
    if !(1..=2).contains(&args.len()) || !no_spread(cx, args, 2) {
        return false;
    }
    let callee = skip_wrappers(ast, callee);
    match *ast.node(callee) {
        Node::Ident { .. } => name_is(cx, callee, "fetch"),
        Node::Member { obj, prop, .. } => {
            name_is(cx, prop, "fetch") && name_is(cx, skip_wrappers(ast, obj), "globalThis")
        }
        _ => false,
    }
}

/// `await reader.read()` awaited in `lp`, `reader` being `<fetch body>.getReader()`.
fn is_fetch_read(cx: &Ctx<'_, '_>, node: NodeId, lp: NodeId) -> bool {
    let ast = cx.ast();
    let Some(n) = resolve(cx, node) else {
        return false;
    };
    let Node::Await { arg } = *ast.node(n) else {
        return false;
    };
    if loop_of(cx, n) != Some(lp) {
        return false;
    }
    let Some((reader, [])) = method(cx, skip_wrappers(ast, arg), "read") else {
        return false;
    };
    let Some(reader) = resolve(cx, reader) else {
        return false;
    };
    let Some((body, [])) = method(cx, reader, "getReader") else {
        return false;
    };
    is_fetch_body(cx, body)
}

/// The static name of a property or binding property key.
fn key_name_of(cx: &Ctx<'_, '_>, key: NodeId) -> Option<String> {
    match *cx.node(key) {
        Node::ComputedKey { expr } => static_value(cx, expr)?.to_js_string(),
        Node::NumberLit { .. } => static_value(cx, key)?.to_js_string(),
        _ => key_name(cx.ast(), cx.src(), key).map(|k| k.into_owned()),
    }
}

/// The binding property of `pattern` named `name`, binding a plain name.
fn pattern_name(cx: &Ctx<'_, '_>, prop: NodeId) -> Option<NodeId> {
    match *cx.node(prop) {
        Node::BindingProperty {
            key,
            value,
            init: None,
        } => {
            let target = value.unwrap_or(key);
            matches!(cx.node(target), Node::Ident { .. }).then_some(target)
        }
        _ => None,
    }
}

fn binding_key(cx: &Ctx<'_, '_>, prop: NodeId) -> Option<String> {
    match *cx.node(prop) {
        Node::BindingProperty { key, .. } => key_name_of(cx, key),
        _ => None,
    }
}

/// `chunk` is a fetch-body chunk of `lp`.
fn is_fetch_chunk(cx: &Ctx<'_, '_>, node: NodeId, lp: NodeId) -> bool {
    let ast = cx.ast();
    let Some(n) = resolve(cx, node) else {
        return false;
    };
    if let Node::Member { obj, prop, .. } = *ast.node(n) {
        return name_is(cx, prop, "value") && is_fetch_read(cx, obj, lp);
    }
    let Some((_, decl)) = const_binding(cx, n) else {
        return false;
    };
    let Node::VarDeclarator { name, init, .. } = *ast.node(decl.node) else {
        return false;
    };
    if let Node::ForOf {
        is_await: true,
        left,
        right,
        ..
    } = *ast.node(lp)
        && decl.parent == left
        && name == decl.name
    {
        let Some(mut iterable) = resolve(cx, right) else {
            return false;
        };
        if let Some((obj, args)) = method(cx, iterable, "values")
            && args.len() <= 1
            && no_spread(cx, args, 1)
        {
            iterable = obj;
        }
        return is_fetch_body(cx, iterable);
    }
    let Node::ObjectPattern { props } = *ast.node(name) else {
        return false;
    };
    ast.list(props).iter().any(|&p| {
        binding_key(cx, p).as_deref() == Some("value") && pattern_name(cx, p) == Some(decl.name)
    }) && init.is_some_and(|i| is_fetch_read(cx, i, lp))
}

fn is_non_streaming_options(cx: &Ctx<'_, '_>, options: Option<NodeId>) -> bool {
    let ast = cx.ast();
    let Some(options) = options else {
        return true;
    };
    let options = skip_wrappers(ast, options);
    let Node::ObjectLit { props } = *ast.node(options) else {
        return static_value_control_flow(cx, options).is_some_and(|v| v.is_nullish());
    };
    let mut stream = None;
    for &p in ast.list(props) {
        let (key, value) = match *ast.node(p) {
            Node::Property { key, value, .. } => (key_name_of(cx, key), value),
            Node::ShorthandProp {
                name, init: None, ..
            } => (ident_name(ast, name).map(str::to_owned), name),
            _ => return false,
        };
        match key.as_deref() {
            None | Some("__proto__") => return false,
            Some("stream") => stream = Some(value),
            _ => {}
        }
    }
    stream.is_none_or(|v| static_value_control_flow(cx, v).is_some_and(|v| !v.truthy()))
}

/// `left += right` as a statement, `left` a name: (assignment, left, right).
fn accumulation(cx: &Ctx<'_, '_>, stmt: NodeId) -> Option<(NodeId, NodeId, NodeId)> {
    let ast = cx.ast();
    let Node::ExprStmt { expr } = *ast.node(stmt) else {
        return None;
    };
    let assign = skip_parens(ast, expr);
    let Node::Assign {
        left,
        op: AssignOp::AddAssign,
        right,
    } = *ast.node(assign)
    else {
        return None;
    };
    let left = skip_parens(ast, left);
    matches!(ast.node(left), Node::Ident { .. }).then_some((assign, left, skip_parens(ast, right)))
}

/// A non-optional `<obj>.decode(...)` with `n` arguments (callee through parentheses only).
fn plain_decode(cx: &Ctx<'_, '_>, call: NodeId, n: usize) -> Option<NodeId> {
    let ast = cx.ast();
    let Node::Call {
        callee,
        optional: false,
        args,
        ..
    } = *ast.node(call)
    else {
        return None;
    };
    let Node::Member {
        obj,
        optional: false,
        prop,
    } = *ast.node(skip_parens(ast, callee))
    else {
        return None;
    };
    let args = ast.list(args);
    (name_is(cx, prop, "decode") && args.len() == n && no_spread(cx, args, n))
        .then(|| skip_parens(ast, obj))
}

fn block_stmts<'a>(cx: &Ctx<'a, '_>, id: NodeId) -> &'a [NodeId] {
    match *cx.node(id) {
        Node::Block { stmts } | Node::Program { stmts } => cx.ast().list(stmts),
        _ => &[],
    }
}

fn is_done_break(cx: &Ctx<'_, '_>, stmt: NodeId, pattern: NodeId) -> bool {
    let ast = cx.ast();
    let Node::If {
        test,
        cons,
        alt: None,
    } = *ast.node(stmt)
    else {
        return false;
    };
    let Node::ObjectPattern { props } = *ast.node(pattern) else {
        return false;
    };
    let Some(done) = ast
        .list(props)
        .iter()
        .copied()
        .find(|&p| binding_key(cx, p).as_deref() == Some("done"))
    else {
        return false;
    };
    let Some((_, test_decl)) = const_binding(cx, test) else {
        return false;
    };
    let exit = match *ast.node(cons) {
        Node::Block { stmts } => match ast.list(stmts) {
            [only] => *only,
            _ => return false,
        },
        _ => cons,
    };
    pattern_name(cx, done) == Some(test_decl.name)
        && matches!(ast.node(exit), Node::Break { label: None })
}

fn is_simple_loop(cx: &Ctx<'_, '_>, lp: NodeId, stmt: NodeId, chunk: NodeId) -> bool {
    let ast = cx.ast();
    match *ast.node(lp) {
        Node::ForOf {
            is_await,
            left,
            body,
            ..
        } => {
            is_await
                && block_stmts(cx, body) == [stmt]
                && const_binding(cx, chunk).is_some_and(|(_, d)| d.parent == left)
        }
        Node::While { test, body } => {
            if !matches!(
                ast.node(skip_parens(ast, test)),
                Node::BoolLit { value: true }
            ) {
                return false;
            }
            let [read, done, last] = block_stmts(cx, body)[..] else {
                return false;
            };
            if last != stmt {
                return false;
            }
            let Some((_, decl)) = const_binding(cx, chunk) else {
                return false;
            };
            let Node::VarDecl { decls, .. } = *ast.node(read) else {
                return false;
            };
            let Node::VarDeclarator { name, init, .. } = *ast.node(decl.node) else {
                return false;
            };
            decl.parent == read
                && ast.list(decls).len() == 1
                && matches!(*ast.node(name), Node::ObjectPattern { props } if ast.list(props).len() == 2)
                && init.is_some_and(|i| is_fetch_read(cx, i, lp))
                && is_done_break(cx, done, name)
        }
        _ => false,
    }
}

/// A [`binding`] of a plain name declared earlier in the block that holds `lp`.
fn binding_before_loop(cx: &Ctx<'_, '_>, node: NodeId, lp: NodeId) -> Option<(SymbolId, Decl)> {
    let (sym, decl) = binding(cx, node)?;
    let Node::VarDeclarator { name, .. } = *cx.node(decl.node) else {
        return None;
    };
    (matches!(cx.node(name), Node::Ident { .. })
        && cx.parent(decl.parent) == cx.parent(lp)
        && cx.span(decl.node).end <= cx.span(lp).start)
        .then_some((sym, decl))
}

fn init_of(cx: &Ctx<'_, '_>, decl: &Decl) -> Option<NodeId> {
    match *cx.node(decl.node) {
        Node::VarDeclarator { init, .. } => init,
        _ => None,
    }
}

fn is_new_text_decoder(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    match *ast.node(skip_wrappers(ast, node)) {
        Node::New { callee, .. } => name_is(cx, skip_wrappers(ast, callee), "TextDecoder"),
        _ => false,
    }
}

/// A comment right after `lp`, starting on its last line.
fn comment_after_on_line(cx: &Ctx<'_, '_>, lp: NodeId) -> bool {
    let rest = &cx.src()[cx.span(lp).end as usize..];
    let rest = rest.trim_start_matches([' ', '\t', '\u{b}', '\u{c}', '\u{a0}', '\u{feff}']);
    rest.starts_with("//") || rest.starts_with("/*")
}

struct Flush {
    left: NodeId,
    decoder: NodeId,
}

fn following_flush(cx: &Ctx<'_, '_>, lp: NodeId, left: NodeId, decoder: NodeId) -> Option<Flush> {
    let stmts = block_stmts(cx, cx.parent(lp)?);
    let i = stmts.iter().position(|&s| s == lp)?;
    let (_, next_left, next_right) = accumulation(cx, *stmts.get(i + 1)?)?;
    let next_decoder = plain_decode(cx, next_right, 0)?;
    let sym = |n| binding(cx, n).map(|(s, _)| s);
    (sym(next_left) == sym(left) && sym(next_decoder) == sym(decoder)).then_some(Flush {
        left: next_left,
        decoder: next_decoder,
    })
}

fn streaming_suggestion(cx: &Ctx<'_, '_>, call: NodeId, lp: NodeId) -> Option<Fix> {
    let ast = cx.ast();
    let outside = |mut id: NodeId| {
        while let Some(p) = cx.parent(id)
            && matches!(cx.node(p), Node::Paren { .. })
        {
            id = p;
        }
        cx.parent(id)
    };
    let stmt = outside(outside(call)?)?;
    let (_, left, right) = accumulation(cx, stmt)?;
    let Node::Call { args, .. } = *ast.node(call) else {
        return None;
    };
    let args = ast.list(args);
    if right != call {
        return None;
    }
    let decoder = plain_decode(cx, call, args.len())?;
    if !is_simple_loop(cx, lp, stmt, args[0])
        || !matches!(
            cx.parent(lp).map(|p| cx.node(p)),
            Some(Node::Block { .. } | Node::Program { .. })
        )
    {
        return None;
    }
    let options = args.get(1).map(|&o| skip_parens(ast, o));
    let option_props = match options {
        None => None,
        Some(o) => {
            let Node::ObjectLit { props } = *ast.node(o) else {
                return None;
            };
            Some(ast.list(props))
        }
    };
    let stream_value = match option_props {
        None | Some([]) => None,
        Some([p]) => {
            let Node::Property { key, value, .. } = *ast.node(*p) else {
                return None;
            };
            if key_name_of(cx, key).as_deref() != Some("stream")
                || !matches!(
                    ast.node(skip_parens(ast, value)),
                    Node::BoolLit { value: false }
                )
            {
                return None;
            }
            Some(value)
        }
        Some(_) => return None,
    };
    if has_comment_in(cx, cx.span(lp)) || comment_after_on_line(cx, lp) {
        return None;
    }
    let flush = following_flush(cx, lp, left, decoder);

    // The decoder and the accumulator are used nowhere else.
    let (decoder_sym, decoder_decl) = binding_before_loop(cx, decoder, lp)?;
    let (text_sym, text_decl) = binding_before_loop(cx, left, lp)?;
    if !is_kind(cx, &decoder_decl, VarKind::Const)
        || !init_of(cx, &decoder_decl).is_some_and(|i| is_new_text_decoder(cx, i))
        || !is_kind(cx, &text_decl, VarKind::Let)
        || init_of(cx, &text_decl)
            .and_then(|i| static_string_literal(ast, skip_parens(ast, i)))
            .as_deref()
            != Some("")
    {
        return None;
    }
    let sem = cx.semantic();
    let decoder_ok = sem.symbol(decoder_sym).refs.iter().all(|&r| {
        let r = sem.reference(r);
        r.flags.contains(RefFlags::INIT)
            || r.node == decoder
            || flush.as_ref().is_some_and(|f| f.decoder == r.node)
    });
    let text_ok = sem.symbol(text_sym).refs.iter().all(|&r| {
        let r = sem.reference(r);
        !r.is_write()
            || r.flags.contains(RefFlags::INIT)
            || r.node == left
            || flush.as_ref().is_some_and(|f| f.left == r.node)
    });
    if !decoder_ok || !text_ok {
        return None;
    }

    let src = cx.src();
    let mut fix = match (options, stream_value) {
        (None, _) => {
            let close = cx.span(call).end - 1;
            let before = skip_trivia_backward(ast, src, close as usize);
            if src[..before].ends_with(',') {
                Fix::insert(close, " {stream: true},")
            } else {
                Fix::insert(close, ", {stream: true}")
            }
        }
        (Some(o), None) => Fix::insert(cx.span(o).start + 1, "stream: true"),
        (Some(_), Some(v)) => Fix::replace(cx.span(v), "true"),
    };
    if flush.is_none() {
        let end = cx.span(lp).end;
        let text = format!(
            "{}{}{} += {}.decode();",
            linebreak(src),
            line_indent(src, cx.span(lp).start as usize),
            cx.span(left).text(src),
            cx.span(decoder).text(src),
        );
        fix = fix.and(Span::at(end), text);
    }
    Some(fix)
}

impl Rule for RequireTextDecoderStreaming {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Call { callee, args, .. } = *ast.node(node) else {
            return;
        };
        let Node::Member { obj, prop, .. } = *ast.node(skip_wrappers(ast, callee)) else {
            return;
        };
        let args = ast.list(args);
        if !name_is(cx, prop, "decode")
            || !(1..=2).contains(&args.len())
            || !no_spread(cx, args, 2)
            || !is_non_streaming_options(cx, args.get(1).copied())
        {
            return;
        }
        let Some(lp) = loop_of(cx, node) else {
            return;
        };
        if !resolve(cx, obj).is_some_and(|d| is_new_text_decoder(cx, d))
            || !is_fetch_chunk(cx, args[0], lp)
        {
            return;
        }
        let suggestion = streaming_suggestion(cx, node, lp);
        let report = cx
            .report(
                cx.span(prop),
                "Decode fetch-body chunks with `{stream: true}` using the same decoder, then consume a final `decode()` flush, or use `TextDecoderStream`.",
            )
            .id("require-text-decoder-streaming/error");
        if let Some(fix) = suggestion {
            report.suggest(
                "require-text-decoder-streaming/suggestion",
                "Enable streaming decoding and consume the final flush.",
                fix,
            );
        }
    }
}
