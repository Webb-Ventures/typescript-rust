//! `unicorn/require-passive-events`: `{passive: true}` for `touchstart`, `touchmove`,
//! `touchenter`, `touchend`, `touchleave`, `wheel` and `mousewheel` listeners.
//!
//! The call is a plain `<x>.addEventListener(name, listener[, options])` (no `?.`) whose
//! name is a static string or substitution-free template and whose listener is an arrow
//! or function expression that can't call `preventDefault()`: a `function` that reads
//! `arguments` (also in nested arrows) can; otherwise the first parameter after a TS
//! `this` must be absent or a plain name whose every reference is the object of a member
//! chain (through `?.` and TS wrappers) with no `preventDefault` link, not written to, and
//! not called through a dynamic key.
//!
//! Reported on the name, fixed by adding `, {passive: true}` after the listener; turning a
//! boolean into `{capture: true, passive: true}` / `{passive: true}`; adding `passive: true`
//! to an object without a `passive` key (after the last property, on its own line in a
//! multi-line object; not fixed when a comment sits after the last property); or turning a
//! `passive: false` into `true`. `undefined`/`null` options are reported without a fix;
//! other options (a variable, a spread or computed key, any other `passive` value) are not
//! reported.

use super::prelude::*;
use super::unicorn_ast::{is_left_hand_side, linebreak};
use super::unicorn_static::{Static, static_value};
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/require-passive-events",
        "Require passive event listeners for high-frequency events.",
        NO_PRESET,
        Category::Pedantic,
    )
    .fixable(),
    create: create_default::<RequirePassiveEvents>,
};

#[derive(Debug, Default)]
struct RequirePassiveEvents;

const EVENTS: &[&str] = &[
    "touchstart",
    "touchmove",
    "touchenter",
    "touchend",
    "touchleave",
    "wheel",
    "mousewheel",
];

fn static_string(ast: &Ast, id: NodeId) -> Option<String> {
    match *ast.node(skip_parens(ast, id)) {
        Node::StringLit { value } => Some(ast.atom(value).to_owned()),
        Node::TemplateLit { parts } => match ast.list(parts) {
            &[quasi] => match *ast.node(quasi) {
                Node::TemplateElement {
                    cooked,
                    invalid_escape: false,
                } => Some(ast.atom(cooked).to_owned()),
                _ => None,
            },
            _ => None,
        },
        _ => None,
    }
}

/// `arguments` read in `function`, outside nested non-arrow functions.
fn uses_arguments(cx: &Ctx<'_, '_>, function: NodeId) -> bool {
    let ast = cx.ast();
    let sem = cx.semantic();
    let mut stack = vec![function];
    while let Some(id) = stack.pop() {
        if id != function
            && matches!(
                ast.node(id),
                Node::FunctionDecl { .. } | Node::FunctionExpr { .. }
            )
        {
            continue;
        }
        if matches!(ast.node(id), Node::Ident { .. })
            && ident_name(ast, id) == Some("arguments")
            && sem.reference_at(id).is_some()
        {
            return true;
        }
        ast.for_each_child(id, |c| stack.push(c));
    }
    false
}

/// `node` with its TS wrappers (and parentheses) around it.
fn outermost(cx: &Ctx<'_, '_>, node: NodeId) -> (NodeId, Option<NodeId>) {
    let mut cur = node;
    loop {
        let Some(p) = cx.parent(cur) else {
            return (cur, None);
        };
        match *cx.node(p) {
            Node::Paren { .. }
            | Node::As { .. }
            | Node::Satisfies { .. }
            | Node::NonNull { .. }
            | Node::TypeAssertion { .. } => cur = p,
            _ => return (cur, Some(p)),
        }
    }
}

/// The static name of a member link; `None` for a dynamic key.
fn link_name(cx: &Ctx<'_, '_>, member: NodeId) -> Option<String> {
    let ast = cx.ast();
    match *ast.node(member) {
        Node::Member { prop, .. } => ident_name(ast, prop).map(str::to_owned),
        Node::Index { index, .. } => match static_value(cx, index)? {
            v @ (Static::Str(_) | Static::Num(_)) => v.to_js_string(),
            _ => None,
        },
        _ => None,
    }
}

/// The member whose object is `node`.
fn member_of(cx: &Ctx<'_, '_>, node: NodeId, parent: Option<NodeId>) -> Option<NodeId> {
    let p = parent?;
    match *cx.node(p) {
        Node::Member { obj, .. } | Node::Index { obj, .. } if obj == node => Some(p),
        _ => None,
    }
}

fn is_safe_reference(cx: &Ctx<'_, '_>, ident: NodeId) -> bool {
    let (mut node, mut parent) = outermost(cx, ident);
    let Some(mut member) = member_of(cx, node, parent) else {
        return false;
    };
    let mut dynamic = false;
    for _ in 0..10_000 {
        let name = link_name(cx, member);
        if name.as_deref() == Some("preventDefault") {
            return false;
        }
        dynamic |= name.is_none();
        (node, parent) = outermost(cx, member);
        match member_of(cx, node, parent) {
            Some(m) => member = m,
            None => break,
        }
    }
    let called =
        parent.is_some_and(|p| matches!(*cx.node(p), Node::Call { callee, .. } if callee == node));
    !(is_left_hand_side(cx, node) || dynamic && called)
}

fn listener_is_safe(cx: &Ctx<'_, '_>, listener: NodeId) -> bool {
    let ast = cx.ast();
    let params = match *ast.node(listener) {
        Node::Arrow { params, .. } => params,
        Node::FunctionExpr { params, .. } => {
            if uses_arguments(cx, listener) {
                return false;
            }
            params
        }
        _ => return false,
    };
    let params = ast.list(params);
    let pattern = |p: NodeId| match *ast.node(p) {
        Node::Param { pattern, .. } => pattern,
        _ => p,
    };
    let first = match params.first() {
        Some(&p)
            if matches!(ast.node(pattern(p)), Node::This { .. })
                || ident_name(ast, pattern(p)) == Some("this") =>
        {
            params.get(1)
        }
        other => other,
    };
    let Some(&param) = first else {
        return true;
    };
    let name = pattern(param);
    if !matches!(ast.node(name), Node::Ident { .. })
        || matches!(
            ast.node(param),
            Node::Param { rest: true, .. } | Node::Param { init: Some(_), .. }
        )
    {
        return false;
    }
    let sem = cx.semantic();
    let Some(sym) = sem.symbol_of_decl(name) else {
        return false;
    };
    sem.symbol(sym)
        .refs
        .iter()
        .map(|&r| sem.reference(r).node)
        .filter(|&n| n != name)
        .all(|n| is_safe_reference(cx, n))
}

/// The leading whitespace of the line `pos` is on.
fn line_indent(src: &str, pos: u32) -> &str {
    let start = src[..pos as usize]
        .rfind(['\n', '\r', '\u{2028}', '\u{2029}'])
        .map_or(0, |i| i + src[i..].chars().next().map_or(1, char::len_utf8));
    let line = &src[start..];
    &line[..line.len()
        - line
            .trim_start_matches([' ', '\t', '\u{feff}', '\u{a0}'])
            .len()]
}

fn property_name(cx: &Ctx<'_, '_>, prop: NodeId) -> Option<String> {
    let ast = cx.ast();
    match *ast.node(prop) {
        Node::Property { key, .. } => match *ast.node(key) {
            Node::Ident { .. } => ident_name(ast, key).map(str::to_owned),
            Node::StringLit { value } => Some(ast.atom(value).to_owned()),
            Node::NumberLit { .. } => static_value(cx, key).and_then(|v| v.to_js_string()),
            _ => None,
        },
        Node::ShorthandProp { name, .. } => ident_name(ast, name).map(str::to_owned),
        _ => None,
    }
}

/// The options problem: `None` when not reported, `Some(None)` for no fix.
fn options_problem(
    cx: &Ctx<'_, '_>,
    listener: NodeId,
    options: Option<NodeId>,
) -> Option<Option<Fix>> {
    let ast = cx.ast();
    let Some(options) = options else {
        let end = super::unicorn_text::paren_span(cx, listener).end;
        return Some(Some(Fix::insert(end, ", {passive: true}")));
    };
    let options = skip_parens(ast, options);
    match *ast.node(options) {
        Node::BoolLit { value } => {
            let text = if value {
                "{capture: true, passive: true}"
            } else {
                "{passive: true}"
            };
            Some(Some(Fix::replace(cx.span(options), text)))
        }
        Node::NullLit {} => Some(None),
        Node::Ident { .. } if ident_name(ast, options) == Some("undefined") => Some(None),
        Node::ObjectLit { props } => {
            let props = ast.list(props);
            if props.iter().any(|&p| match *ast.node(p) {
                Node::Spread { .. } => true,
                Node::Property { key, .. } => matches!(ast.node(key), Node::ComputedKey { .. }),
                _ => false,
            }) {
                return None;
            }
            let passive = props.iter().rev().find(|&&p| {
                matches!(
                    ast.node(p),
                    Node::Property { .. } | Node::ShorthandProp { .. }
                ) && property_name(cx, p).as_deref() == Some("passive")
            });
            if let Some(&passive) = passive {
                if let Node::Property { value, .. } = *ast.node(passive)
                    && let Node::BoolLit { value: false } = *ast.node(skip_parens(ast, value))
                {
                    return Some(Some(Fix::replace(cx.span(skip_parens(ast, value)), "true")));
                }
                return None;
            }
            let span = cx.span(options);
            let src = cx.src();
            let after = props.last().map_or(span.start + 1, |&p| cx.span(p).end);
            if !ast.comments_in(Span::new(after, span.end)).is_empty() {
                return Some(None);
            }
            let Some(&last) = props.last() else {
                return Some(Some(Fix::replace(span, "{passive: true}")));
            };
            let text = &src[span.start as usize..span.end as usize];
            if !text.contains(['\n', '\r', '\u{2028}', '\u{2029}']) {
                return Some(Some(Fix::insert(after, ", passive: true")));
            }
            let indent = line_indent(src, cx.span(last).start);
            let lb = linebreak(src);
            let next = skip_trivia_forward(src, after as usize);
            Some(Some(if src.as_bytes().get(next) == Some(&b',') {
                Fix::insert(next as u32 + 1, format!("{lb}{indent}passive: true,"))
            } else {
                Fix::insert(after, format!(",{lb}{indent}passive: true"))
            }))
        }
        _ => None,
    }
}

impl Rule for RequirePassiveEvents {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Call {
            callee,
            optional: false,
            args,
            ..
        } = *ast.node(node)
        else {
            return;
        };
        let Node::Member {
            optional: false,
            prop,
            ..
        } = *ast.node(skip_parens(ast, callee))
        else {
            return;
        };
        let args = ast.list(args);
        if ident_name(ast, prop) != Some("addEventListener")
            || !(2..=3).contains(&args.len())
            || args
                .iter()
                .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
        {
            return;
        }
        let name = args[0];
        let listener = skip_parens(ast, args[1]);
        if !static_string(ast, name).is_some_and(|n| EVENTS.contains(&n.as_str()))
            || !listener_is_safe(cx, listener)
        {
            return;
        }
        let Some(fix) = options_problem(cx, listener, args.get(2).copied()) else {
            return;
        };
        cx.report(
            cx.span(skip_parens(ast, name)),
            "Use `{passive: true}` for this high-frequency event listener.",
        )
        .id("require-passive-events")
        .fix_opt(fix);
    }
}
