//! `unicorn/no-for-each`: every `x.forEach(…)` call is reported at `forEach`, except on
//! `React.Children`, `Children`, `R`, `pIteration`, `Effect`, a `strict-callbag-basics`
//! namespace import, and receivers known not to be arrays (`new Map()`...).
//!
//! It is fixed into a `for…of` loop only when the receiver is known to be an array and the
//! call is a whole expression statement or an arrow body, has one argument, a non-async,
//! non-generator function with one or two plain untyped parameters (two use
//! `.entries()`), no `return` inside a loop of the callback, no `this`/`arguments`/own
//! name use in a `function` callback, and no reference in the call to a parameter's name
//! that resolves outside the callback. `return x;` becomes `x; continue;` (braced under an
//! `if`), a reassigned parameter makes the binding `let`, `foo?.forEach` is guarded by
//! `if (foo)` (a suggestion when the receiver has side effects), and a comment in the
//! rebuilt head blocks the fix. The fix range spans the call's parent.

use super::prelude::*;
use super::unicorn_ast::matches_path;
use super::unicorn_text::paren_span;
use super::unicorn_text::{
    is_parenthesized, member_object_needs_parens, needs_semicolon,
    starts_with_declaration_like_token,
};
use super::unicorn_types::{is_array, should_skip_known_non_array_receiver};
use super::unicorn_util::space_around_keyword;
use super::unicorn_util::{fix_of, has_side_effect_with};
use crate::semantic::{DeclKind, RefFlags, Resolution, ScopeId};
use ts_r_ast::{Ast, FnFlags};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-for-each",
        "Prefer `for…of` over the `forEach` method.",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<NoForEach>,
};

const MESSAGE: &str = "Use `for…of` instead of `.forEach(…)`.";
const IGNORED_OBJECTS: &[&str] = &["React.Children", "Children", "R", "pIteration", "Effect"];

#[derive(Debug, Default)]
struct NoForEach;

impl FromOptions for NoForEach {
    fn from_options(_: &[Value]) -> Result<Self, String> {
        Ok(NoForEach)
    }
}

fn is_function(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(id),
        Node::FunctionDecl { .. } | Node::FunctionExpr { .. } | Node::Arrow { .. }
    )
}

fn is_loop(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(id),
        Node::While { .. }
            | Node::DoWhile { .. }
            | Node::For { .. }
            | Node::ForOf { .. }
            | Node::ForIn { .. }
    )
}

/// The `return`s that belong to `f` itself (not to a nested function).
fn own_returns(ast: &Ast, f: NodeId) -> Vec<NodeId> {
    let mut out = Vec::new();
    let mut stack = Vec::new();
    ast.for_each_child(f, |c| stack.push(c));
    while let Some(n) = stack.pop() {
        if is_function(ast, n) {
            continue;
        }
        if matches!(ast.node(n), Node::Return { .. }) {
            out.push(n);
        }
        ast.for_each_child(n, |c| stack.push(c));
    }
    out.sort_by_key(|&n| ast.span(n).start);
    out
}

/// `this` directly in `f`'s own scope (nested functions, arrows included, and class bodies
/// apart from computed keys and heritage have their own).
fn uses_own_this(ast: &Ast, f: NodeId) -> bool {
    let mut stack = Vec::new();
    ast.for_each_child(f, |c| stack.push(c));
    while let Some(n) = stack.pop() {
        match *ast.node(n) {
            Node::This {} => return true,
            Node::FunctionDecl { .. } | Node::FunctionExpr { .. } | Node::Arrow { .. } => {}
            Node::ClassDecl {
                heritage, members, ..
            }
            | Node::ClassExpr {
                heritage, members, ..
            } => {
                stack.extend(ast.list(heritage).iter().copied());
                for &m in ast.list(members) {
                    if let Node::PropertyDef { key, .. } | Node::MethodDef { key, .. } =
                        *ast.node(m)
                        && matches!(ast.node(key), Node::ComputedKey { .. })
                    {
                        stack.push(key);
                    }
                }
            }
            _ => ast.for_each_child(n, |c| stack.push(c)),
        }
    }
    false
}

fn is_ancestor_scope(cx: &Ctx<'_, '_>, ancestor: ScopeId, mut scope: ScopeId) -> bool {
    let sem = cx.semantic();
    for _ in 0..100_000 {
        if scope == ancestor {
            return true;
        }
        match sem.scope(scope).parent {
            Some(p) => scope = p,
            None => return false,
        }
    }
    false
}

/// The call's argument is a namespace import of `strict-callbag-basics`.
fn is_strict_callbag_namespace(cx: &Ctx<'_, '_>, object: NodeId) -> bool {
    let ast = cx.ast();
    let sem = cx.semantic();
    if !matches!(ast.node(object), Node::Ident { .. }) {
        return false;
    }
    let Some(sym) = sem
        .reference_at(object)
        .and_then(|r| sem.reference(r).symbol())
    else {
        return false;
    };
    sem.symbol(sym).decls.iter().any(|d| {
        d.kind == DeclKind::ImportNamespace
            && matches!(*ast.node(d.parent), Node::ImportDecl { source, .. }
                if matches!(*ast.node(source), Node::StringLit { value } if ast.atom(value) == "strict-callbag-basics"))
    })
}

struct Candidate<'a> {
    call: NodeId,
    callee: NodeId,
    object: NodeId,
    member_optional: bool,
    callback: NodeId,
    params: &'a [NodeId],
    returns: Vec<NodeId>,
}

/// The statement or arrow the call (through parentheses) is the whole of, or `None`.
fn call_ancestor(cx: &Ctx<'_, '_>, call: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let p = parent_skip_parens(ast, cx.file.parents, call)?;
    match *ast.node(p) {
        Node::ExprStmt { .. } => Some(p),
        Node::Arrow { body, .. } if skip_parens(ast, body) == call => Some(p),
        _ => None,
    }
}

fn fixable_candidate<'a>(cx: &Ctx<'a, '_>, call: NodeId) -> Option<Candidate<'a>> {
    let ast = cx.ast();
    let sem = cx.semantic();
    let Node::Call {
        callee,
        optional: false,
        args,
        ..
    } = *ast.node(call)
    else {
        return None;
    };
    let &[arg] = ast.list(args) else {
        return None;
    };
    call_ancestor(cx, call)?;
    let Node::Member {
        obj,
        optional: member_optional,
        ..
    } = *ast.node(skip_parens(ast, callee))
    else {
        return None;
    };
    let callback = skip_parens(ast, arg);
    let (flags, params, name) = match *ast.node(callback) {
        Node::FunctionExpr {
            flags,
            params,
            name,
            ..
        } => (flags, params, name),
        Node::Arrow { flags, params, .. } => (flags, params, None),
        _ => return None,
    };
    if flags.intersects(FnFlags::ASYNC | FnFlags::GENERATOR) {
        return None;
    }
    let params = ast.list(params);
    if !matches!(params.len(), 1 | 2) {
        return None;
    }
    for (i, &p) in params.iter().enumerate() {
        let Node::Param {
            rest,
            pattern,
            type_ann,
            init,
            ..
        } = *ast.node(p)
        else {
            return None;
        };
        if rest
            || type_ann.is_some()
            || (params.len() == 1 && init.is_some())
            || (i == 1 && (init.is_some() || !matches!(ast.node(pattern), Node::Ident { .. })))
        {
            return None;
        }
    }
    // Parameters: one declaration each, and no reference in the call to their names that
    // resolves outside the callback.
    let call_scope = sem.enclosing_scope(call);
    let call_span = cx.span(call);
    for &p in params {
        let Node::Param { pattern, .. } = *ast.node(p) else {
            return None;
        };
        let mut idents = Vec::new();
        binding_idents(ast, pattern, &mut idents);
        for ident in idents {
            let sym = sem.symbol_of_decl(ident)?;
            let symbol = sem.symbol(sym);
            if symbol.decls.len() != 1 {
                return None;
            }
            let name = symbol.name;
            for r in sem.references() {
                if r.name != name || r.flags.contains(RefFlags::IMPLICIT) {
                    continue;
                }
                let s = cx.span(r.node);
                if s.start < call_span.start || s.end > call_span.end {
                    continue;
                }
                match r.resolution {
                    Resolution::Symbol(other) => {
                        if is_ancestor_scope(cx, sem.symbol(other).scope, call_scope) {
                            return None;
                        }
                    }
                    Resolution::Global | Resolution::Unresolved => return None,
                }
            }
        }
    }
    if let Some(n) = name
        && let Some(sym) = sem.symbol_of_decl(n)
        && sem.symbol(sym).decls.len() != 1
    {
        return None;
    }
    let returns = own_returns(ast, callback);
    if returns.iter().any(|&r| {
        ancestors_inclusive(cx.file.parents, r)
            .take_while(|&a| a != callback)
            .any(|a| is_loop(ast, a))
    }) {
        return None;
    }
    if matches!(ast.node(callback), Node::FunctionExpr { .. }) {
        if uses_own_this(ast, callback) {
            return None;
        }
        let fscope = sem.scope_of_node(callback)?;
        if sem
            .references()
            .iter()
            .any(|r| r.scope == fscope && ast.atom(r.name) == "arguments")
        {
            return None;
        }
        if let Some(n) = name
            && let Some(sym) = sem.symbol_of_decl(n)
            && !sem.symbol(sym).refs.is_empty()
        {
            return None;
        }
    }
    Some(Candidate {
        call,
        callee: skip_parens(ast, callee),
        object: skip_parens(ast, obj),
        member_optional,
        callback,
        params,
        returns,
    })
}

fn param_reassigned(cx: &Ctx<'_, '_>, params: &[NodeId]) -> bool {
    let ast = cx.ast();
    let sem = cx.semantic();
    params.iter().any(|&p| {
        let Node::Param { pattern, .. } = *ast.node(p) else {
            return false;
        };
        let mut idents = Vec::new();
        binding_idents(ast, pattern, &mut idents);
        idents.into_iter().any(|ident| {
            sem.symbol_of_decl(ident).is_some_and(|sym| {
                sem.symbol(sym).refs.iter().any(|&r| {
                    let r = sem.reference(r);
                    r.is_write() && !r.flags.contains(RefFlags::INIT)
                })
            })
        })
    })
}

fn callback_body(ast: &Ast, f: NodeId) -> Option<NodeId> {
    match *ast.node(f) {
        Node::FunctionExpr { body, .. } => body,
        Node::Arrow { body, .. } => Some(body),
        _ => None,
    }
}

fn byte_at(src: &str, pos: u32) -> Option<u8> {
    src.as_bytes().get(pos as usize).copied()
}

fn return_edits(cx: &Ctx<'_, '_>, ret: NodeId, edits: &mut Vec<(Span, String)>) {
    let ast = cx.ast();
    let src = cx.src();
    let Node::Return { arg } = *ast.node(ret) else {
        return;
    };
    let span = cx.span(ret);
    let kw = Span::new(span.start, span.start + 6);
    let Some(arg) = arg else {
        edits.push((kw, "continue".into()));
        return;
    };
    edits.push((kw, String::new()));
    let next = skip_trivia_forward(src, kw.end as usize) as u32;
    let arg = skip_parens(ast, arg);
    let add_parens = !is_parenthesized(cx, arg) && starts_with_declaration_like_token(cx, arg);
    let mut before = String::new();
    let mut after = String::new();
    if add_parens {
        before.push('(');
        after.push(')');
    }
    let parent = cx.parent(ret).map(|p| cx.node(p));
    let braces = match parent {
        Some(Node::If { .. }) => true,
        Some(Node::With { body, .. }) => *body == ret,
        _ => false,
    };
    if braces {
        before.insert_str(0, "{ ");
    } else {
        let code = if add_parens {
            "("
        } else {
            &src[next as usize..]
        };
        if needs_semicolon(cx, span.start, code) {
            before.insert(0, ';');
        }
    }
    if !before.is_empty() {
        edits.push((Span::at(next), before));
    }
    if !after.is_empty() {
        edits.push((Span::at(cx.span(arg).end), after));
    }
    if !span.text(src).ends_with(';') {
        edits.push((Span::at(span.end), ";".into()));
    }
    edits.push((Span::at(span.end), " continue;".into()));
    if braces {
        edits.push((Span::at(span.end), " }".into()));
    }
}

fn build_fix(cx: &Ctx<'_, '_>, c: &Candidate<'_>) -> Option<Fix> {
    let ast = cx.ast();
    let src = cx.src();
    let body = callback_body(ast, c.callback)?;
    let body_is_block = matches!(ast.node(body), Node::Block { .. });
    let paren_body = !body_is_block
        && !is_parenthesized(cx, skip_parens(ast, body))
        && starts_with_declaration_like_token(cx, skip_parens(ast, body));
    let call_span = cx.span(c.call);
    let head_range = Span::new(
        call_span.start,
        paren_span(cx, skip_parens(ast, body)).start,
    );
    if ast
        .comments_in(head_range)
        .iter()
        .any(|cm| cm.span.start >= head_range.start && cm.span.end <= head_range.end)
    {
        return None;
    }
    let texts: Vec<&str> = c.params.iter().map(|&p| cx.text(p)).collect();
    let entries = c.params.len() == 2;
    let object_text = cx.text(c.object);
    let mut head = String::from("for (");
    head.push_str(if param_reassigned(cx, c.params) {
        "let"
    } else {
        "const"
    });
    head.push(' ');
    if entries {
        head.push_str(&format!("[{}, {}]", texts[1], texts[0]));
    } else {
        head.push_str(texts[0]);
    }
    head.push_str(" of ");
    let wrap_object = is_parenthesized(cx, c.object)
        || (c.member_optional && entries && member_object_needs_parens(cx, c.object, c.object));
    if wrap_object {
        head.push_str(&format!("({object_text})"));
    } else {
        head.push_str(object_text);
    }
    if entries {
        head.push_str(".entries()");
    }
    head.push_str(") ");
    if paren_body {
        head.push('(');
    }
    let mut edits = Vec::new();
    // Parentheses around the call.
    let mut cur = c.call;
    while let Some(p) = cx.parent(cur) {
        if !matches!(cx.node(p), Node::Paren { .. }) {
            break;
        }
        let s = cx.span(p);
        edits.push((Span::new(s.start, s.start + 1), String::new()));
        edits.push((Span::new(s.end - 1, s.end), String::new()));
        cur = p;
    }
    edits.push((head_range, head));
    // Closing parentheses around the callback.
    let mut cur = c.callback;
    while let Some(p) = cx.parent(cur) {
        if !matches!(cx.node(p), Node::Paren { .. }) {
            break;
        }
        let s = cx.span(p);
        edits.push((Span::new(s.end - 1, s.end), String::new()));
        cur = p;
    }
    let close = call_span.end - 1;
    let before_close = skip_trivia_backward(ast, src, close as usize) as u32;
    if before_close > 0 && byte_at(src, before_close - 1) == Some(b',') {
        edits.push((Span::new(before_close - 1, before_close), String::new()));
    }
    edits.push((Span::new(close, close + 1), String::new()));
    if paren_body {
        edits.push((Span::at(call_span.end), ")".into()));
    }
    for &r in &c.returns {
        return_edits(cx, r, &mut edits);
    }
    let ancestor = call_ancestor(cx, c.call)?;
    match *ast.node(ancestor) {
        Node::ExprStmt { .. } => {
            let s = cx.span(ancestor);
            if body_is_block && s.text(src).ends_with(';') {
                edits.push((Span::new(s.end - 1, s.end), String::new()));
            }
        }
        Node::Arrow { .. } => {
            edits.push((Span::at(call_span.start), "{ ".into()));
            edits.push((Span::at(call_span.end), " }".into()));
        }
        _ => {}
    }
    edits.extend(space_around_keyword(
        cx,
        if c.member_optional { c.call } else { ancestor },
    ));
    if c.member_optional {
        let wrap = matches!(ast.node(ancestor), Node::ExprStmt { .. })
            && !cx.parent(ancestor).is_some_and(|p| {
                matches!(
                    cx.node(p),
                    Node::Program { .. }
                        | Node::Block { .. }
                        | Node::StaticBlock { .. }
                        | Node::SwitchCase { .. }
                )
            });
        edits.push((
            Span::at(call_span.start),
            format!("{}if ({object_text}) ", if wrap { "{ " } else { "" }),
        ));
        if wrap {
            edits.push((Span::at(cx.span(ancestor).end), " }".into()));
        }
    }
    // The fix covers the call's parent, so fixes of nested calls don't interleave.
    let parent = if c.member_optional {
        parent_skip_parens(ast, cx.file.parents, c.call)?
    } else {
        ancestor
    };
    let ps = cx.span(parent);
    edits.push((Span::at(ps.start), String::new()));
    edits.push((Span::at(ps.end), String::new()));
    Some(fix_of(edits))
}

impl Rule for NoForEach {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Call { callee, .. } = *ast.node(node) else {
            return;
        };
        let callee = skip_parens(ast, callee);
        let Node::Member { obj, prop, .. } = *ast.node(callee) else {
            return;
        };
        if ident_name(ast, prop) != Some("forEach") {
            return;
        }
        let object = skip_parens(ast, obj);
        if IGNORED_OBJECTS.iter().any(|p| matches_path(ast, object, p))
            || is_strict_callbag_namespace(cx, object)
            || should_skip_known_non_array_receiver(cx, object)
        {
            return;
        }
        let fix = if is_array(cx, object) {
            fixable_candidate(cx, node).and_then(|c| {
                let suggest =
                    c.member_optional && has_side_effect_with(ast, c.callee, false, false);
                build_fix(cx, &c).map(|f| (f, suggest))
            })
        } else {
            None
        };
        let span = cx.span(prop);
        let report = cx.report(span, MESSAGE).id("no-for-each/error");
        match fix {
            Some((fix, true)) => {
                report.suggest("no-for-each/suggestion", "Switch to `for…of`.", fix);
            }
            Some((fix, false)) => {
                report.fix(fix);
            }
            None => {}
        }
    }
}
