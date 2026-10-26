//! `unicorn/prefer-object-iterable-methods`: the narrowest of `Object.keys/values/entries`.
//!
//! Sources are a non-`await` `for (const|let b of Object.m(o))` (body = target), and
//! `Object.keys(o).map(cb)` or `Object.entries(o).<every|findIndex|findLastIndex|flatMap|
//! forEach|map|some>(cb[, thisArg])` / `.reduce|reduceRight(cb, init)` (plain calls, no
//! spread) whose `cb` is a non-async, non-generator arrow or function expression with no
//! function or class as its body and, for a `function`, no `arguments` outside nested
//! non-arrow functions. `Object.m` is plain with exactly one argument. The binding is the
//! loop variable or the callback's element parameter; `keys` callbacks take only that
//! parameter, `entries` ones may add an index (a plain identifier).
//!
//! - `keys` with an untyped identifier `k` never written in the target, `o` neither written
//!   (by name or through a member assignment), and reads `o[k]` outside nested functions and
//!   classes, none of them written, called, tagged or deleted: `values` when `k` has no other
//!   use in the target, `entries` otherwise. The reads become `value` (first free name), and
//!   a TS cast on the read objects moves onto the argument (all casts must be identical).
//! - `entries` with `[k, v]` (identifiers, at most two, holes allowed) where neither is
//!   written and exactly one of them is used: `keys` or `values`.
//!
//! Fixes are dropped when the binding or a read holds a comment, when the moved cast would
//! drop a comment in the argument, when a side effect sits beside a read, for loops with
//! `break/continue/return/throw` and callbacks with `throw`.

use super::prelude::*;
use super::unicorn_iter::{for_of_declaration_pattern, unwrap_expression};
use super::unicorn_static::has_side_effect;
use super::unicorn_text::{has_comment_in, is_same_reference, is_written};
use super::unicorn_util::space_around_keyword;
use super::unicorn_util_h4::{available_name, scope_within};
use crate::semantic::{RefFlags, Resolution, ScopeId, SymbolId};
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-object-iterable-methods",
        "Prefer the most specific `Object` iterable method.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<PreferObjectIterableMethods>,
};

#[derive(Debug, Default)]
struct PreferObjectIterableMethods;

const MAX_NODES: usize = 200_000;

fn is_barrier(node: &Node) -> bool {
    matches!(
        node,
        Node::Arrow { .. }
            | Node::FunctionExpr { .. }
            | Node::FunctionDecl { .. }
            | Node::ClassDecl { .. }
            | Node::ClassExpr { .. }
    )
}

/// Nodes under `root` (itself included), not entering functions or classes other than
/// `root`. `None` when the subtree is too large to inspect.
fn target_nodes(ast: &Ast, root: NodeId) -> Option<Vec<NodeId>> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        out.push(n);
        if out.len() > MAX_NODES {
            return None;
        }
        if n != root && is_barrier(ast.node(n)) {
            continue;
        }
        ast.for_each_child(n, |c| stack.push(c));
    }
    Some(out)
}

fn symbol_of(cx: &Ctx<'_, '_>, ident: NodeId) -> Option<SymbolId> {
    let sem = cx.semantic();
    if let Some(r) = sem.reference_at(ident) {
        return match sem.reference(r).resolution {
            Resolution::Symbol(s) => Some(s),
            _ => None,
        };
    }
    sem.symbol_of_decl(ident)
}

fn same_binding(cx: &Ctx<'_, '_>, a: NodeId, b: NodeId) -> bool {
    match (symbol_of(cx, a), symbol_of(cx, b)) {
        (None, None) => ident_name(cx.ast(), a) == ident_name(cx.ast(), b),
        (x, y) => x == y,
    }
}

/// Same reference, with every identifier on both sides bound to the same variable.
fn same_scoped_reference(cx: &Ctx<'_, '_>, a: NodeId, b: NodeId, depth: u32) -> bool {
    let ast = cx.ast();
    let a = unwrap_expression(ast, a);
    let b = unwrap_expression(ast, b);
    if depth > 200 {
        return false;
    }
    match (*ast.node(a), *ast.node(b)) {
        (Node::Ident { .. }, Node::Ident { .. }) => same_binding(cx, a, b),
        (Node::Member { obj: o1, .. }, Node::Member { obj: o2, .. }) => {
            is_same_reference(cx, a, b) && same_scoped_reference(cx, o1, o2, depth + 1)
        }
        (
            Node::Index {
                obj: o1, index: p1, ..
            },
            Node::Member {
                obj: o2, prop: p2, ..
            }
            | Node::Index {
                obj: o2, index: p2, ..
            },
        ) => {
            is_same_reference(cx, a, b)
                && same_scoped_reference(cx, o1, o2, depth + 1)
                && same_scoped_reference(cx, p1, p2, depth + 1)
        }
        (Node::Member { obj: o1, .. }, Node::Index { obj: o2, .. }) => {
            is_same_reference(cx, a, b) && same_scoped_reference(cx, o1, o2, depth + 1)
        }
        _ => is_same_reference(cx, a, b),
    }
}

fn inside(cx: &Ctx<'_, '_>, node: NodeId, outer: Span) -> bool {
    let s = cx.span(node);
    s.start >= outer.start && s.end <= outer.end
}

/// References of `sym` inside `target`; `None` when one of them writes it.
fn references_in(cx: &Ctx<'_, '_>, sym: SymbolId, target: NodeId) -> Option<Vec<NodeId>> {
    let sem = cx.semantic();
    let span = cx.span(target);
    let mut out = Vec::new();
    for &r in &sem.symbol(sym).refs {
        let r = sem.reference(r);
        if !inside(cx, r.node, span) {
            continue;
        }
        if r.is_write() && !r.flags.contains(RefFlags::INIT) {
            return None;
        }
        out.push(r.node);
    }
    Some(out)
}

/// Called, tagged or deleted (through parentheses and TS wrappers).
fn is_reference_context(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let mut cur = node;
    for _ in 0..10_000 {
        let Some(p) = cx.parent(cur) else {
            return false;
        };
        match *ast.node(p) {
            Node::Paren { .. }
            | Node::As { .. }
            | Node::Satisfies { .. }
            | Node::NonNull { .. }
            | Node::TypeAssertion { .. }
            | Node::Instantiation { .. } => cur = p,
            Node::Call { callee, .. } => return callee == cur,
            Node::TaggedTemplate { tag, .. } => return tag == cur,
            Node::Unary {
                op: UnaryOp::Delete,
                ..
            } => return true,
            _ => return false,
        }
    }
    false
}

fn is_ts_wrapper(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(skip_parens(ast, id)),
        Node::As { .. }
            | Node::Satisfies { .. }
            | Node::NonNull { .. }
            | Node::TypeAssertion { .. }
    )
}

/// `Object.<method>(o)`: `o`.
fn object_method_argument(ast: &Ast, call: NodeId, method: &str) -> Option<NodeId> {
    let Node::Call {
        callee,
        optional: false,
        args,
        ..
    } = *ast.node(skip_parens(ast, call))
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
    let obj = skip_parens(ast, obj);
    let ok = matches!(ast.node(obj), Node::Ident { .. })
        && ident_name(ast, obj) == Some("Object")
        && ident_name(ast, prop) == Some(method);
    match *ast.list(args) {
        [arg] if ok && !matches!(ast.node(arg), Node::Spread { .. }) => Some(arg),
        _ => None,
    }
}

fn method_property(ast: &Ast, call: NodeId) -> Option<NodeId> {
    let Node::Call { callee, .. } = *ast.node(skip_parens(ast, call)) else {
        return None;
    };
    match *ast.node(skip_parens(ast, callee)) {
        Node::Member { prop, .. } => Some(prop),
        _ => None,
    }
}

/// The binding as the plugin sees it: a `Param` contributes its pattern (and annotation).
struct Binding {
    pattern: NodeId,
    span: Span,
    /// The pattern is an arrow parameter written without parentheses.
    bare_arrow_param: bool,
    typed: bool,
}

struct Source {
    method_call: NodeId,
    binding: Binding,
    target: NodeId,
    can_fix: bool,
}

struct Problem {
    prop: NodeId,
    current: &'static str,
    preferred: &'static str,
    binding: Binding,
    binding_text: String,
    members: Vec<NodeId>,
    value_name: String,
    object: Option<(NodeId, String)>,
    can_fix: bool,
}

fn keys_problem(cx: &Ctx<'_, '_>, source: Source, object: NodeId) -> Option<Problem> {
    let ast = cx.ast();
    let binding = source.binding;
    if !matches!(ast.node(binding.pattern), Node::Ident { .. }) || binding.typed {
        return None;
    }
    let key = cx.semantic().symbol_of_decl(binding.pattern)?;
    let references = references_in(cx, key, source.target)?;
    let nodes = target_nodes(ast, source.target)?;
    let object_inner = unwrap_expression(ast, object);
    if matches!(ast.node(object_inner), Node::Ident { .. })
        && let Some(sym) = symbol_of(cx, object_inner)
        && references_in(cx, sym, source.target).is_none()
    {
        return None;
    }
    let object_written = nodes.iter().any(|&n| match *ast.node(n) {
        Node::Member { obj, .. } | Node::Index { obj, .. } => {
            is_written(cx, n) && same_scoped_reference(cx, obj, object, 0)
        }
        _ => false,
    });
    if object_written {
        return None;
    }
    let mut members = Vec::new();
    for &n in &nodes {
        let Node::Index {
            obj,
            optional: false,
            index,
        } = *ast.node(n)
        else {
            continue;
        };
        let index = skip_parens(ast, index);
        if !matches!(ast.node(index), Node::Ident { .. })
            || symbol_of(cx, index) != Some(key)
            || !same_scoped_reference(cx, obj, object, 0)
        {
            continue;
        }
        if is_written(cx, n) || is_reference_context(cx, n) {
            return None;
        }
        members.push(n);
    }
    if members.is_empty() {
        return None;
    }
    members.sort_by_key(|&m| cx.span(m).start);
    let mut replacement = None;
    if members.iter().any(|&m| match *ast.node(m) {
        Node::Index { obj, .. } => is_ts_wrapper(ast, obj),
        _ => false,
    }) {
        let mut texts = members.iter().filter_map(|&m| match *ast.node(m) {
            Node::Index { obj, .. } => Some(cx.text(skip_parens(ast, obj))),
            _ => None,
        });
        let first = texts.next()?;
        if texts.any(|t| t != first) {
            return None;
        }
        replacement = Some((skip_parens(ast, object), first.to_owned()));
    }
    let member_keys: Vec<NodeId> = members
        .iter()
        .filter_map(|&m| match *ast.node(m) {
            Node::Index { index, .. } => Some(skip_parens(ast, index)),
            _ => None,
        })
        .collect();
    let key_used = references.iter().any(|r| !member_keys.contains(r));
    let value_name = value_name(cx, binding.pattern)?;
    let preferred = if key_used { "entries" } else { "values" };
    let binding_text = if key_used {
        let text = format!("[{}, {value_name}]", cx.text(binding.pattern));
        if binding.bare_arrow_param {
            format!("({text})")
        } else {
            text
        }
    } else {
        value_name.clone()
    };
    let side_effect = side_effect_beside(cx, &nodes, &members);
    let comment_in_object = replacement
        .as_ref()
        .is_some_and(|&(o, _)| has_comment_in(cx, cx.span(o)));
    Some(Problem {
        prop: method_property(ast, source.method_call)?,
        current: "keys",
        preferred,
        binding,
        binding_text,
        members,
        value_name,
        object: replacement,
        can_fix: source.can_fix && !side_effect && !comment_in_object,
    })
}

/// A node before or after one of the reads has a side effect.
fn side_effect_beside(cx: &Ctx<'_, '_>, nodes: &[NodeId], members: &[NodeId]) -> bool {
    let ast = cx.ast();
    let effects: Vec<Span> = nodes
        .iter()
        .filter(|&&n| has_side_effect(ast, n))
        .map(|&n| cx.span(n))
        .collect();
    members.iter().any(|&m| {
        let m = cx.span(m);
        effects.iter().any(|s| s.end <= m.start || s.start >= m.end)
    })
}

/// `value`, or the first `value_…` free in the binding's scopes.
fn value_name(cx: &Ctx<'_, '_>, binding: NodeId) -> Option<String> {
    let sem = cx.semantic();
    let scope = sem.enclosing_scope(binding);
    let scopes: Vec<ScopeId> = (0..sem.scopes().len())
        .map(|i| ScopeId(i as u32))
        .filter(|&s| scope_within(sem, s, scope))
        .collect();
    available_name(cx, "value", &scopes, &|_| true)
}

fn entries_problem(cx: &Ctx<'_, '_>, source: Source) -> Option<Problem> {
    let ast = cx.ast();
    let binding = source.binding;
    let Node::ArrayPattern { elements } = *ast.node(binding.pattern) else {
        return None;
    };
    let elements = ast.list(elements);
    if elements.len() > 2 {
        return None;
    }
    let element = |i: usize| {
        elements
            .get(i)
            .copied()
            .filter(|&e| !matches!(ast.node(e), Node::Hole {}))
    };
    let (key, value) = (element(0), element(1));
    if key.is_none() && value.is_none() {
        return None;
    }
    let mut uses = [0usize; 2];
    for (i, e) in [key, value].into_iter().enumerate() {
        let Some(e) = e else {
            continue;
        };
        if !matches!(ast.node(e), Node::Ident { .. }) {
            return None;
        }
        let sym = cx.semantic().symbol_of_decl(e)?;
        uses[i] = references_in(cx, sym, source.target)?.len();
    }
    let (preferred, kept) = match uses {
        [0, v] if v > 0 => ("values", value?),
        [k, 0] if k > 0 => ("keys", key?),
        _ => return None,
    };
    Some(Problem {
        prop: method_property(ast, source.method_call)?,
        current: "entries",
        preferred,
        binding_text: cx.text(kept).to_owned(),
        binding,
        members: Vec::new(),
        value_name: String::new(),
        object: None,
        can_fix: source.can_fix,
    })
}

fn contains_abrupt(ast: &Ast, root: NodeId, throw_only: bool) -> bool {
    target_nodes(ast, root).is_none_or(|nodes| {
        nodes.iter().any(|&n| match ast.node(n) {
            Node::Throw { .. } => true,
            Node::Break { .. } | Node::Continue { .. } | Node::Return { .. } => !throw_only,
            _ => false,
        })
    })
}

/// `arguments` in a `function` callback, outside nested non-arrow functions and classes.
fn uses_arguments(ast: &Ast, callback: NodeId) -> bool {
    if !matches!(ast.node(callback), Node::FunctionExpr { .. }) {
        return false;
    }
    let mut stack = vec![callback];
    let mut seen = 0usize;
    while let Some(n) = stack.pop() {
        seen += 1;
        if seen > MAX_NODES {
            return true;
        }
        let node = ast.node(n);
        if matches!(node, Node::Ident { .. }) && ident_name(ast, n) == Some("arguments") {
            return true;
        }
        if n != callback
            && matches!(
                node,
                Node::FunctionExpr { .. }
                    | Node::FunctionDecl { .. }
                    | Node::ClassDecl { .. }
                    | Node::ClassExpr { .. }
            )
        {
            continue;
        }
        ast.for_each_child(n, |c| stack.push(c));
    }
    false
}

fn is_arrow_param_parenthesized(cx: &Ctx<'_, '_>, param: NodeId) -> bool {
    let src = cx.src();
    let span = cx.span(param);
    src[..span.start as usize].trim_end().ends_with('(')
        && src[span.end as usize..].trim_start().starts_with(')')
}

fn param_binding(cx: &Ctx<'_, '_>, param: NodeId, arrow: bool) -> Option<Binding> {
    let ast = cx.ast();
    let Node::Param {
        rest: false,
        pattern,
        type_ann,
        init: None,
        ..
    } = *ast.node(param)
    else {
        return None;
    };
    let mut span = cx.span(pattern);
    if let Some(t) = type_ann {
        span = Span::new(span.start, cx.span(t).end);
    }
    Some(Binding {
        pattern,
        span,
        bare_arrow_param: arrow && !is_arrow_param_parenthesized(cx, param),
        typed: type_ann.is_some(),
    })
}

fn is_plain_ident_param(ast: &Ast, param: NodeId) -> bool {
    matches!(*ast.node(param), Node::Param { rest: false, init: None, pattern, .. }
        if matches!(ast.node(pattern), Node::Ident { .. }))
}

impl PreferObjectIterableMethods {
    fn problem(&self, cx: &Ctx<'_, '_>, source: Source) -> Option<Problem> {
        let ast = cx.ast();
        if let Some(object) = object_method_argument(ast, source.method_call, "keys") {
            keys_problem(cx, source, object)
        } else if object_method_argument(ast, source.method_call, "entries").is_some() {
            entries_problem(cx, source)
        } else {
            None
        }
    }

    fn for_of_source(&self, cx: &Ctx<'_, '_>, id: NodeId) -> Option<Source> {
        let ast = cx.ast();
        let Node::ForOf {
            is_await: false,
            left,
            right,
            body,
        } = *ast.node(id)
        else {
            return None;
        };
        let pattern = for_of_declaration_pattern(ast, left)?;
        let typed = cx.parent(pattern).is_some_and(|d| {
            matches!(
                *ast.node(d),
                Node::VarDeclarator {
                    type_ann: Some(_),
                    ..
                }
            )
        });
        Some(Source {
            method_call: right,
            binding: Binding {
                pattern,
                span: cx.span(pattern),
                bare_arrow_param: false,
                typed,
            },
            target: body,
            can_fix: !contains_abrupt(ast, body, false),
        })
    }

    fn callback_source(&self, cx: &Ctx<'_, '_>, id: NodeId) -> Option<Source> {
        let ast = cx.ast();
        let Node::Call {
            callee,
            optional: false,
            args,
            ..
        } = *ast.node(id)
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
        if args
            .iter()
            .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
        {
            return None;
        }
        let method = ident_name(ast, prop)?;
        let (index, arg_range) = match method {
            "every" | "findIndex" | "findLastIndex" | "flatMap" | "forEach" | "map" | "some" => {
                (0, 1..=2)
            }
            "reduce" | "reduceRight" => (1, 2..=2),
            _ => return None,
        };
        let keys_map = method == "map" && object_method_argument(ast, obj, "keys").is_some();
        let entries = object_method_argument(ast, obj, "entries").is_some();
        if !(keys_map || entries) || !arg_range.contains(&args.len()) {
            return None;
        }
        let callback = skip_parens(ast, args[0]);
        let (flags, params, body, arrow) = match *ast.node(callback) {
            Node::Arrow {
                flags,
                params,
                body,
                ..
            } => (flags, params, body, true),
            Node::FunctionExpr {
                flags,
                params,
                body: Some(body),
                ..
            } => (flags, params, body, false),
            _ => return None,
        };
        let params = ast.list(params);
        let extra = if entries { 2 } else { 1 };
        if flags.contains(FnFlags::ASYNC)
            || flags.contains(FnFlags::GENERATOR)
            || params.len() <= index
            || params.len() > index + extra
            || (params.len() != index + 1 && !is_plain_ident_param(ast, params[params.len() - 1]))
            || uses_arguments(ast, callback)
            || is_barrier(ast.node(skip_parens(ast, body)))
        {
            return None;
        }
        Some(Source {
            method_call: obj,
            binding: param_binding(cx, params[index], arrow)?,
            target: body,
            can_fix: !contains_abrupt(ast, body, true),
        })
    }

    fn report(&self, cx: &mut Ctx<'_, '_>, p: Problem) {
        let commented = has_comment_in(cx, p.binding.span)
            || p.members.iter().any(|&m| has_comment_in(cx, cx.span(m)));
        let fix = (p.can_fix && !commented).then(|| {
            let mut fix =
                Fix::replace(cx.span(p.prop), p.preferred).and(p.binding.span, &*p.binding_text);
            let mut edits = space_around_keyword(cx, p.binding.pattern);
            for &m in &p.members {
                fix = fix.and(cx.span(m), &*p.value_name);
                edits.extend(space_around_keyword(cx, m));
            }
            if let Some((o, text)) = &p.object {
                fix = fix.and(cx.span(*o), &**text);
            }
            edits.into_iter().fold(fix, |f, (s, t)| f.and(s, t))
        });
        let r = cx
            .report(
                cx.span(p.prop),
                format!(
                    "Prefer `Object.{}()` over `Object.{}()`.",
                    p.preferred, p.current
                ),
            )
            .id("prefer-object-iterable-methods");
        if let Some(f) = fix {
            r.fix(f);
        }
    }
}

impl Rule for PreferObjectIterableMethods {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::ForOf, NodeKind::Call]
    }
    fn enter(&self, id: NodeId, cx: &mut Ctx<'_, '_>) {
        let source = match cx.node(id) {
            Node::ForOf { .. } => self.for_of_source(cx, id),
            _ => self.callback_source(cx, id),
        };
        if let Some(p) = source.and_then(|s| self.problem(cx, s)) {
            self.report(cx, p);
        }
    }
}
