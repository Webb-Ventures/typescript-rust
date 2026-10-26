//! `unicorn/prefer-abort-signal-any`: a `const controller = new AbortController()` whose
//! only job is to abort when other signals do, written as `abort` listeners right after it.
//!
//! The bridge is either one `for (const signal of signals)` (not `await`) whose body is
//! only `signal.addEventListener('abort', () => controller.abort())`, over a known array
//! of at least two signal-like elements (or a name, `Array.of`, `Array(...)`,
//! `Array.from(<known array>)`), or two or more such listeners in a row on signal-like
//! names (`signal`, `*Signal`) or side-effect-free `AbortSignal.any/timeout(...)` calls.
//! Listener options may only be a boolean or `{capture, once, passive}` booleans, and the
//! callback may pass `source.reason` along. Sources that could hold the controller's own
//! signal or an already aborted one (`AbortSignal.abort()`), or a spread, or a constant
//! array mutated before the loop, stop it. The suggestion is the shared `AbortController`
//! one with `AbortSignal.any([...])`, copying a readonly array (`as const`, `readonly T[]`,
//! `ReadonlyArray`) into a fresh one.

use super::prelude::*;
use super::unicorn_abort::{
    abort_controller_declarator, abort_reference_from, next_statement, report_abort_controller,
    signal_members,
};
use super::unicorn_no_array_reduce::const_initializer;
use super::unicorn_static::{has_side_effect, static_string_literal};
use super::unicorn_types::{is_array, type_definition};
use super::unicorn_util::skip_wrappers;
use ts_r_ast::TypeOperatorKind;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-abort-signal-any",
        "Prefer `AbortSignal.any()` over manually forwarding abort events between signals.",
        NO_PRESET,
        Category::Style,
    )
    .suggestions(),
    create: create_default::<PreferAbortSignalAny>,
};

#[derive(Debug, Default)]
struct PreferAbortSignalAny;

const MAX_DEPTH: usize = 64;

fn name_is(cx: &Ctx<'_, '_>, id: NodeId, name: &str) -> bool {
    matches!(cx.node(id), Node::Ident { .. }) && ident_name(cx.ast(), id) == Some(name)
}

/// `<Object>.<prop>(...)` (non-optional call and member): (property name, arguments).
fn static_call<'a>(cx: &Ctx<'a, '_>, id: NodeId, object: &str) -> Option<(&'a str, &'a [NodeId])> {
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
    let obj = skip_parens(ast, obj);
    if !name_is(cx, obj, object) || (object == "Array" && !cx.is_global_ref(obj, "Array")) {
        return None;
    }
    Some((ident_name(ast, prop)?, ast.list(args)))
}

fn abort_signal_call<'a>(cx: &Ctx<'a, '_>, id: NodeId, names: &[&str]) -> Option<&'a [NodeId]> {
    static_call(cx, id, "AbortSignal").and_then(|(n, args)| names.contains(&n).then_some(args))
}

fn array_call<'a>(cx: &Ctx<'a, '_>, id: NodeId, name: &str) -> Option<&'a [NodeId]> {
    static_call(cx, id, "Array").and_then(|(n, args)| (n == name).then_some(args))
}

/// `Array(...)` or `new Array(...)` of the global `Array`.
fn array_constructor<'a>(cx: &Ctx<'a, '_>, id: NodeId) -> Option<&'a [NodeId]> {
    let ast = cx.ast();
    let (callee, args) = match *ast.node(id) {
        Node::Call {
            callee,
            optional: false,
            args,
            ..
        }
        | Node::New { callee, args, .. } => (skip_parens(ast, callee), args),
        _ => return None,
    };
    (name_is(cx, callee, "Array") && cx.is_global_ref(callee, "Array")).then(|| ast.list(args))
}

fn is_spread(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    matches!(cx.node(id), Node::Spread { .. })
}

fn const_init(cx: &Ctx<'_, '_>, id: NodeId) -> Option<NodeId> {
    const_initializer(cx, skip_wrappers(cx.ast(), id))
}

/// A `const` array name written to between its declaration and this use.
fn is_mutated_const_array(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let id = skip_wrappers(ast, id);
    if !matches!(ast.node(id), Node::Ident { .. }) {
        return false;
    }
    let sem = cx.semantic();
    let Some(sym) = sem.reference_at(id).and_then(|r| sem.reference(r).symbol()) else {
        return false;
    };
    let symbol = sem.symbol(sym);
    let [decl] = symbol.decls[..] else {
        return false;
    };
    let Node::VarDeclarator {
        init: Some(init), ..
    } = *ast.node(decl.node)
    else {
        return false;
    };
    if !matches!(
        ast.node(decl.parent),
        Node::VarDecl {
            kind: VarKind::Const,
            ..
        }
    ) {
        return false;
    }
    let (init_end, at) = (cx.span(init).end, cx.span(id).start);
    symbol.refs.iter().any(|&r| {
        let r = sem.reference(r);
        if r.flags.contains(RefFlags::INIT) || r.node == id {
            return false;
        }
        let start = cx.span(r.node).start;
        start > init_end && start < at
    })
}

fn is_controller_signal(cx: &Ctx<'_, '_>, id: NodeId, controller: &str) -> bool {
    let ast = cx.ast();
    matches!(
        *ast.node(skip_wrappers(ast, id)),
        Node::Member { obj, optional: false, prop }
            if name_is(cx, prop, "signal") && name_is(cx, obj, controller)
    )
}

/// The source can hold the controller's own signal or an already aborted one (or can't
/// be followed).
fn has_unsupported_source(
    cx: &Ctx<'_, '_>,
    id: NodeId,
    controller: &str,
    seen: &mut Vec<NodeId>,
) -> bool {
    let ast = cx.ast();
    let id = skip_wrappers(ast, id);
    if seen.contains(&id) {
        return false;
    }
    if seen.len() > MAX_DEPTH {
        return true;
    }
    seen.push(id);
    if is_spread(cx, id) {
        return true;
    }
    if let Node::ArrayLit { elements } = *ast.node(id) {
        return ast.list(elements).iter().any(|&e| {
            !matches!(ast.node(e), Node::Hole {}) && has_unsupported_source(cx, e, controller, seen)
        });
    }
    if let Some(args) = array_call(cx, id, "of").or_else(|| array_constructor(cx, id)) {
        return args
            .iter()
            .any(|&a| has_unsupported_source(cx, a, controller, seen));
    }
    if let Some(args) = array_call(cx, id, "from") {
        let &[source] = args else {
            return true;
        };
        if is_spread(cx, source) {
            return true;
        }
        if let Some(init) = const_init(cx, source) {
            if !is_array(cx, init) || is_mutated_const_array(cx, source) {
                return true;
            }
            return has_unsupported_source(cx, init, controller, seen);
        }
        let source = skip_wrappers(ast, source);
        return !matches!(ast.node(source), Node::ArrayLit { .. })
            || has_unsupported_source(cx, source, controller, seen);
    }
    if let Some(args) = abort_signal_call(cx, id, &["any"]) {
        return args
            .first()
            .is_some_and(|&a| has_unsupported_source(cx, a, controller, seen));
    }
    if let Some(init) = const_init(cx, id) {
        if is_array(cx, init) && is_mutated_const_array(cx, id) {
            return true;
        }
        return has_unsupported_source(cx, init, controller, seen);
    }
    is_controller_signal(cx, id, controller) || abort_signal_call(cx, id, &["abort"]).is_some()
}

fn is_signal_like_name(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    ident_name(cx.ast(), id).is_some_and(|n| n == "signal" || n.ends_with("Signal"))
}

fn is_direct_bridge_source(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let id = skip_wrappers(ast, id);
    if matches!(ast.node(id), Node::Ident { .. }) {
        return is_signal_like_name(cx, id);
    }
    abort_signal_call(cx, id, &["any", "timeout"]).is_some_and(|args| {
        args.iter()
            .all(|&a| !is_spread(cx, a) && !has_side_effect(ast, a))
    })
}

fn known_array_from_source(cx: &Ctx<'_, '_>, id: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let &[source] = array_call(cx, id, "from")? else {
        return None;
    };
    if is_spread(cx, source) {
        return None;
    }
    let unwrapped = skip_wrappers(ast, source);
    if matches!(ast.node(unwrapped), Node::ArrayLit { .. }) {
        return Some(unwrapped);
    }
    let init = const_init(cx, source)?;
    (is_array(cx, init) && !is_mutated_const_array(cx, source)).then_some(init)
}

fn known_array_elements<'a>(
    cx: &Ctx<'a, '_>,
    id: NodeId,
    seen: &mut Vec<NodeId>,
) -> Option<&'a [NodeId]> {
    let ast = cx.ast();
    let id = skip_wrappers(ast, id);
    if seen.contains(&id) || seen.len() > MAX_DEPTH {
        return None;
    }
    seen.push(id);
    if let Node::ArrayLit { elements } = *ast.node(id) {
        let elements = ast.list(elements);
        return elements
            .iter()
            .all(|&e| !matches!(ast.node(e), Node::Hole {}))
            .then_some(elements);
    }
    if let Some(args) = array_call(cx, id, "of").or_else(|| array_constructor(cx, id)) {
        return Some(args);
    }
    if let Some(source) = known_array_from_source(cx, id) {
        return known_array_elements(cx, source, seen);
    }
    let init = const_init(cx, id)?;
    (is_array(cx, init) && !is_mutated_const_array(cx, id))
        .then(|| known_array_elements(cx, init, seen))
        .flatten()
}

fn is_allowed_array_composition(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    known_array_elements(cx, id, &mut Vec::new()).is_none_or(|elements| {
        elements.len() > 1 && elements.iter().all(|&e| is_direct_bridge_source(cx, e))
    })
}

fn is_allowed_for_of_source(cx: &Ctx<'_, '_>, id: NodeId, depth: usize) -> bool {
    let ast = cx.ast();
    let id = skip_wrappers(ast, id);
    if depth > MAX_DEPTH {
        return false;
    }
    if matches!(ast.node(id), Node::Ident { .. } | Node::ArrayLit { .. })
        || array_call(cx, id, "of").is_some()
        || array_constructor(cx, id).is_some()
    {
        return true;
    }
    known_array_from_source(cx, id).is_some_and(|s| is_allowed_for_of_source(cx, s, depth + 1))
}

// ---- Type annotations ----

/// The last name of a type reference (`AbortSignal`, `globalThis.AbortSignal` →
/// `AbortSignal`), and whether it is a plain name.
fn type_ref_name<'a>(cx: &Ctx<'a, '_>, name: NodeId) -> Option<(&'a str, bool)> {
    let ast = cx.ast();
    match *ast.node(name) {
        Node::Ident { .. } => Some((ident_name(ast, name)?, true)),
        Node::QualifiedName { right, .. } => Some((ident_name(ast, right)?, false)),
        _ => None,
    }
}

/// The aliased type of the local `type <name> = ...` the reference `at` sees.
fn alias_target(
    cx: &Ctx<'_, '_>,
    name: &str,
    at: NodeId,
    visited: &mut Vec<SymbolId>,
) -> Option<Option<NodeId>> {
    let sem = cx.semantic();
    let (sym, decl) = type_definition(cx, name, sem.enclosing_scope(at))?;
    if visited.contains(&sym) || visited.len() > MAX_DEPTH {
        return None;
    }
    visited.push(sym);
    Some(match (decl.kind, *cx.node(decl.node)) {
        (DeclKind::TypeAlias, Node::TypeAlias { ty, .. }) => Some(ty),
        _ => None,
    })
}

fn is_abort_signal_annotation(cx: &Ctx<'_, '_>, ty: NodeId, visited: &mut Vec<SymbolId>) -> bool {
    let Node::TypeRef { name, .. } = *cx.node(ty) else {
        return false;
    };
    let Some((n, plain)) = type_ref_name(cx, name) else {
        return false;
    };
    if n == "AbortSignal" {
        return true;
    }
    if !plain {
        return false;
    }
    match alias_target(cx, n, ty, visited) {
        Some(Some(target)) => {
            let r = is_abort_signal_annotation(cx, target, visited);
            visited.pop();
            r
        }
        Some(None) => {
            visited.pop();
            false
        }
        None => false,
    }
}

fn type_args<'a>(cx: &Ctx<'a, '_>, args: Option<NodeId>) -> &'a [NodeId] {
    match args.map(|a| *cx.node(a)) {
        Some(Node::TypeArgs { args }) => cx.ast().list(args),
        _ => &[],
    }
}

/// Whether the annotation is an array of `AbortSignal`s (`None`: can't tell).
fn signal_array_state(cx: &Ctx<'_, '_>, ty: NodeId, visited: &mut Vec<SymbolId>) -> Option<bool> {
    let ast = cx.ast();
    match *ast.node(ty) {
        Node::ParenType { ty } => signal_array_state(cx, ty, visited),
        Node::TypeOperator {
            op: TypeOperatorKind::Readonly,
            ty,
        } => signal_array_state(cx, ty, visited),
        Node::ArrayType { elem } => Some(is_abort_signal_annotation(cx, elem, &mut Vec::new())),
        Node::TupleType { elems } => {
            let elems = ast.list(elems);
            Some(
                !elems.is_empty()
                    && elems.iter().all(|&e| {
                        let e = match *ast.node(e) {
                            Node::NamedTupleMember { ty, .. } => ty,
                            _ => e,
                        };
                        is_abort_signal_annotation(cx, e, &mut Vec::new())
                    }),
            )
        }
        Node::TypeRef { name, type_args: a } => {
            let (n, plain) = type_ref_name(cx, name)?;
            let args = type_args(cx, a);
            if matches!(n, "Array" | "ReadonlyArray") && args.len() == 1 {
                return Some(is_abort_signal_annotation(cx, args[0], &mut Vec::new()));
            }
            if !plain {
                return Some(false);
            }
            let target = alias_target(cx, n, ty, visited)?;
            let state = target.and_then(|t| signal_array_state(cx, t, visited));
            visited.pop();
            state
        }
        _ => None,
    }
}

fn is_readonly_array_annotation(cx: &Ctx<'_, '_>, ty: NodeId, visited: &mut Vec<SymbolId>) -> bool {
    let ast = cx.ast();
    match *ast.node(ty) {
        Node::ParenType { ty } => is_readonly_array_annotation(cx, ty, visited),
        Node::TypeOperator {
            op: TypeOperatorKind::Readonly,
            ty,
        } => matches!(
            ast.node(ty),
            Node::ArrayType { .. } | Node::TupleType { .. }
        ),
        Node::TypeRef { name, .. } => {
            let Some((n, plain)) = type_ref_name(cx, name) else {
                return false;
            };
            if n == "ReadonlyArray" {
                return true;
            }
            if !plain {
                return false;
            }
            match alias_target(cx, n, ty, visited) {
                Some(target) => {
                    let r = target.is_some_and(|t| is_readonly_array_annotation(cx, t, visited));
                    visited.pop();
                    r
                }
                None => false,
            }
        }
        _ => false,
    }
}

/// `(expr, type)` of `as`, `<T>` or `satisfies` (through parentheses).
fn type_wrapper(cx: &Ctx<'_, '_>, id: NodeId) -> Option<(NodeId, NodeId)> {
    match *cx.node(skip_parens(cx.ast(), id)) {
        Node::As { expr, ty } | Node::Satisfies { expr, ty } | Node::TypeAssertion { ty, expr } => {
            Some((expr, ty))
        }
        _ => None,
    }
}

/// The first declaration of the name `id` reads: (type annotation, variable initializer).
fn definition(cx: &Ctx<'_, '_>, id: NodeId) -> Option<(Option<NodeId>, Option<NodeId>)> {
    let sem = cx.semantic();
    let sym = sem
        .reference_at(id)
        .and_then(|r| sem.reference(r).symbol())?;
    let decl = *sem.symbol(sym).decls.first()?;
    match *cx.node(decl.node) {
        Node::VarDeclarator {
            name,
            type_ann,
            init,
            ..
        } if name == decl.name => Some((type_ann, init)),
        Node::VarDeclarator { init, .. } => Some((None, init)),
        Node::Param {
            pattern, type_ann, ..
        } if pattern == decl.name => Some((type_ann, None)),
        _ => Some((None, None)),
    }
}

fn needs_array_copy(cx: &Ctx<'_, '_>, id: NodeId, depth: usize) -> bool {
    let ast = cx.ast();
    if depth > MAX_DEPTH {
        return false;
    }
    if let Some((expr, ty)) = type_wrapper(cx, id) {
        return matches!(
            ast.node(ty),
            Node::KeywordType {
                kind: KeywordTypeKind::Const
            }
        ) || is_readonly_array_annotation(cx, ty, &mut Vec::new())
            || needs_array_copy(cx, expr, depth + 1);
    }
    let id = skip_wrappers(ast, id);
    if !matches!(ast.node(id), Node::Ident { .. }) {
        return false;
    }
    let Some((ty, init)) = definition(cx, id) else {
        return false;
    };
    ty.is_some_and(|t| is_readonly_array_annotation(cx, t, &mut Vec::new()))
        || init.is_some_and(|i| needs_array_copy(cx, i, depth + 1))
}

fn is_for_of_array(cx: &Ctx<'_, '_>, id: NodeId, depth: usize) -> bool {
    let ast = cx.ast();
    if depth > MAX_DEPTH {
        return false;
    }
    if let Some((expr, ty)) = type_wrapper(cx, id) {
        return signal_array_state(cx, ty, &mut Vec::new())
            .unwrap_or_else(|| is_for_of_array(cx, expr, depth + 1));
    }
    let id = skip_wrappers(ast, id);
    if matches!(ast.node(id), Node::Ident { .. })
        && let Some((ty, init)) = definition(cx, id)
    {
        if let Some(state) = ty.and_then(|t| signal_array_state(cx, t, &mut Vec::new())) {
            return state;
        }
        if init.is_some_and(|i| is_for_of_array(cx, i, depth + 1)) {
            return true;
        }
    }
    is_array(cx, id)
}

fn any_argument_text(cx: &Ctx<'_, '_>, id: NodeId) -> String {
    let ast = cx.ast();
    let node = skip_parens(ast, id);
    let text = cx.text(node);
    if !needs_array_copy(cx, node, 0) {
        return text.to_owned();
    }
    let unwrapped = skip_wrappers(ast, node);
    if matches!(ast.node(unwrapped), Node::ArrayLit { .. }) {
        return cx.text(unwrapped).to_owned();
    }
    if unwrapped == node {
        format!("[...{text}]")
    } else {
        format!("[...({text})]")
    }
}

// ---- Listeners ----

fn is_allowed_listener_options(cx: &Ctx<'_, '_>, options: Option<NodeId>) -> bool {
    let ast = cx.ast();
    let Some(options) = options else {
        return true;
    };
    match *ast.node(skip_wrappers(ast, options)) {
        Node::BoolLit { .. } => true,
        Node::ObjectLit { props } => ast.list(props).iter().all(|&p| {
            let Node::Property { key, value, .. } = *ast.node(p) else {
                return false;
            };
            let name = match *ast.node(key) {
                Node::Ident { .. } => ident_name(ast, key).map(str::to_owned),
                Node::StringLit { value } => Some(ast.atom(value).to_owned()),
                _ => None,
            };
            matches!(ast.node(value), Node::BoolLit { .. })
                && matches!(name.as_deref(), Some("capture" | "once" | "passive"))
        }),
        _ => false,
    }
}

/// `<source>.addEventListener('abort', callback, options?)`: (source, callback).
fn abort_listener(cx: &Ctx<'_, '_>, stmt: NodeId) -> Option<(NodeId, NodeId)> {
    let ast = cx.ast();
    let Node::ExprStmt { expr } = *ast.node(stmt) else {
        return None;
    };
    let Node::Call {
        callee,
        optional: false,
        args,
        ..
    } = *ast.node(skip_parens(ast, expr))
    else {
        return None;
    };
    let args = ast.list(args);
    if !(2..=3).contains(&args.len()) || args.iter().any(|&a| is_spread(cx, a)) {
        return None;
    }
    let Node::Member {
        obj,
        optional: false,
        prop,
    } = *ast.node(skip_parens(ast, callee))
    else {
        return None;
    };
    (name_is(cx, prop, "addEventListener")
        && static_string_literal(ast, skip_wrappers(ast, args[0])).as_deref() == Some("abort")
        && is_allowed_listener_options(cx, args.get(2).copied()))
    .then_some((obj, args[1]))
}

struct Bridge {
    first: NodeId,
    last: NodeId,
    aborts: Vec<NodeId>,
    replacement: String,
}

fn for_of_bridge(cx: &Ctx<'_, '_>, decl: NodeId, controller: &str) -> Option<Bridge> {
    let ast = cx.ast();
    let stmt = next_statement(cx, decl)?;
    let Node::ForOf {
        is_await: false,
        left,
        right,
        body,
    } = *ast.node(stmt)
    else {
        return None;
    };
    if !is_allowed_for_of_source(cx, right, 0)
        || is_mutated_const_array(cx, right)
        || has_unsupported_source(cx, right, controller, &mut Vec::new())
        || !is_for_of_array(cx, right, 0)
        || !is_allowed_array_composition(cx, right)
    {
        return None;
    }
    let Node::VarDecl {
        kind: VarKind::Const,
        decls,
        ..
    } = *ast.node(left)
    else {
        return None;
    };
    let Node::VarDeclarator { name: signal, .. } = *ast.node(*ast.list(decls).first()?) else {
        return None;
    };
    if !matches!(ast.node(signal), Node::Ident { .. }) || !is_signal_like_name(cx, signal) {
        return None;
    }
    let only = match *ast.node(body) {
        Node::Block { stmts } => match ast.list(stmts) {
            &[only] => only,
            _ => return None,
        },
        _ => body,
    };
    let (source, callback) = abort_listener(cx, only)?;
    let source = skip_wrappers(ast, source);
    if !matches!(ast.node(source), Node::Ident { .. })
        || ident_name(ast, source) != ident_name(ast, signal)
    {
        return None;
    }
    let abort = abort_reference_from(cx, callback, controller, Some(signal))?;
    Some(Bridge {
        first: stmt,
        last: stmt,
        aborts: vec![abort],
        replacement: format!("AbortSignal.any({})", any_argument_text(cx, right)),
    })
}

fn direct_bridge(cx: &Ctx<'_, '_>, decl: NodeId, controller: &str) -> Option<Bridge> {
    let ast = cx.ast();
    let mut stmts = Vec::new();
    let mut sources = Vec::new();
    let mut aborts = Vec::new();
    let mut stmt = next_statement(cx, decl);
    while let Some(s) = stmt {
        let Some((source, callback)) = abort_listener(cx, s) else {
            break;
        };
        if has_unsupported_source(cx, source, controller, &mut Vec::new())
            || !is_direct_bridge_source(cx, source)
        {
            break;
        }
        let Some(abort) = abort_reference_from(cx, callback, controller, Some(source)) else {
            break;
        };
        stmts.push(s);
        sources.push(cx.text(skip_parens(ast, source)));
        if !aborts.contains(&abort) {
            aborts.push(abort);
        }
        stmt = next_statement(cx, s);
    }
    if stmts.len() < 2 {
        return None;
    }
    Some(Bridge {
        first: stmts[0],
        last: *stmts.last()?,
        aborts,
        replacement: format!("AbortSignal.any([{}])", sources.join(", ")),
    })
}

impl Rule for PreferAbortSignalAny {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::VarDeclarator]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Some((decl, name, _)) = abort_controller_declarator(cx, node) else {
            return;
        };
        let controller = ident_name(cx.ast(), name).unwrap_or("");
        let Some(bridge) =
            for_of_bridge(cx, decl, controller).or_else(|| direct_bridge(cx, decl, controller))
        else {
            return;
        };
        let Some(members) = signal_members(cx, name, &bridge.aborts) else {
            return;
        };
        report_abort_controller(
            cx,
            node,
            bridge.first,
            bridge.last,
            bridge.replacement,
            &members,
            "prefer-abort-signal-any",
            "Prefer `AbortSignal.any()` over manually forwarding abort events between signals.",
            "prefer-abort-signal-any/suggestion",
            "Replace with `AbortSignal.any()`.",
        );
    }
}
