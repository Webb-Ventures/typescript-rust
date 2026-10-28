//! `unicorn/prefer-observer-apis`: `ResizeObserver`/`IntersectionObserver` over `resize`/
//! `scroll` listeners that read layout.
//!
//! The registration is a plain `<x>.addEventListener(name, listener[, options])` or a call
//! of the global `addEventListener` (two or three arguments, no `?.`), with a static
//! `"resize"`/`"scroll"` name (through TS wrappers). The listener is an inline function
//! (through TS wrappers) or a binding of one that is never reassigned (a function
//! declaration, or a variable initialized with a function).
//!
//! Its body (not nested functions) reads layout when it has: a call of
//! `getBoundingClientRect`/`getClientRects` on a possible element; a read (not a plain
//! assignment or `delete` target) of `clientWidth`, `offsetTop`, `scrollHeight`, ... on a
//! possible element, of `innerWidth`/`innerHeight` on the global object (or as global
//! names), or of `width`/`height` on the global `visualViewport`; or an object pattern
//! taking those names from such an object. A possible element is anything but the global
//! object, `document`, `visualViewport`, literals and object/array literals, and `const`
//! bindings of object literals. Reported on the event name.

use super::prelude::*;
use super::unicorn_ast::is_left_hand_side;
use super::unicorn_static::{Static, static_value};
use super::unicorn_util::skip_wrappers;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-observer-apis",
        "Prefer observer APIs over resize and scroll listeners with layout reads.",
        NO_PRESET,
        Category::Pedantic,
    ),
    create: create_default::<PreferObserverApis>,
};

#[derive(Debug, Default)]
struct PreferObserverApis;

const ELEMENT_LAYOUT: &[&str] = &[
    "clientHeight",
    "clientLeft",
    "clientTop",
    "clientWidth",
    "offsetHeight",
    "offsetLeft",
    "offsetParent",
    "offsetTop",
    "offsetWidth",
    "scrollHeight",
    "scrollWidth",
];
const VIEWPORT: &[&str] = &["innerHeight", "innerWidth"];
const VISUAL_VIEWPORT: &[&str] = &["height", "width"];
const LAYOUT_METHODS: &[&str] = &["getBoundingClientRect", "getClientRects"];

fn static_string(ast: &Ast, id: NodeId) -> Option<String> {
    match *ast.node(id) {
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

fn static_key(cx: &Ctx<'_, '_>, expr: NodeId) -> Option<String> {
    match static_value(cx, skip_wrappers(cx.ast(), expr))? {
        v @ (Static::Str(_) | Static::Num(_)) => v.to_js_string(),
        _ => None,
    }
}

/// The static name a member access reads.
fn member_name(cx: &Ctx<'_, '_>, id: NodeId) -> Option<(NodeId, String)> {
    let ast = cx.ast();
    match *ast.node(id) {
        Node::Member { obj, prop, .. } => Some((obj, ident_name(ast, prop)?.to_owned())),
        Node::Index { obj, index, .. } => Some((obj, static_key(cx, index)?)),
        _ => None,
    }
}

fn is_global_name(cx: &Ctx<'_, '_>, id: NodeId, names: &[&str]) -> bool {
    let ast = cx.ast();
    matches!(ast.node(id), Node::Ident { .. })
        && ident_name(ast, id).is_some_and(|n| names.contains(&n) && cx.is_global_ref(id, n))
}

fn is_global_object(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    is_global_name(cx, id, &["globalThis", "self", "window"])
}

/// The global `name`, or `<global object>.name`.
fn is_global_property(cx: &Ctx<'_, '_>, id: NodeId, name: &str) -> bool {
    let ast = cx.ast();
    let id = skip_wrappers(ast, id);
    if matches!(ast.node(id), Node::Ident { .. }) {
        return is_global_name(cx, id, &[name]);
    }
    member_name(cx, id)
        .is_some_and(|(obj, n)| n == name && is_global_object(cx, skip_wrappers(ast, obj)))
}

fn is_const_object(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    if !matches!(ast.node(id), Node::Ident { .. }) {
        return false;
    }
    let sem = cx.semantic();
    let Some(sym) = sem.reference_at(id).and_then(|r| sem.reference(r).symbol()) else {
        return false;
    };
    let &[decl] = &sem.symbol(sym).decls[..] else {
        return false;
    };
    decl.kind == DeclKind::Const
        && matches!(*ast.node(decl.node), Node::VarDeclarator { init: Some(init), .. }
            if matches!(ast.node(init), Node::ObjectLit { .. }))
}

fn is_non_element(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let node = skip_wrappers(ast, id);
    is_global_object(cx, node)
        || is_global_property(cx, node, "document")
        || is_global_property(cx, node, "visualViewport")
        || matches!(
            ast.node(node),
            Node::ObjectLit { .. }
                | Node::ArrayLit { .. }
                | Node::StringLit { .. }
                | Node::NumberLit { .. }
                | Node::BigIntLit { .. }
                | Node::BoolLit { .. }
                | Node::NullLit {}
                | Node::RegexLit { .. }
        )
        || static_string(ast, node).is_some()
        || is_const_object(cx, node)
}

/// Written to without being read: a plain assignment target or a `delete` operand.
fn is_write_only(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    if !is_left_hand_side(cx, id) {
        return false;
    }
    let ast = cx.ast();
    let mut cur = id;
    for _ in 0..10_000 {
        let Some(p) = cx.parent(cur) else {
            return true;
        };
        match *ast.node(p) {
            Node::Paren { .. }
            | Node::As { .. }
            | Node::Satisfies { .. }
            | Node::NonNull { .. }
            | Node::TypeAssertion { .. } => cur = p,
            Node::Update { .. } => return false,
            Node::Assign { left, op, .. } if left == cur => return op == AssignOp::Assign,
            _ => return true,
        }
    }
    true
}

fn is_layout_property_read(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    match *ast.node(id) {
        Node::Ident { .. } => {
            cx.semantic().reference_at(id).is_some()
                && is_global_name(cx, id, VIEWPORT)
                && !is_write_only(cx, id)
        }
        Node::Member { .. } | Node::Index { .. } => {
            let Some((obj, name)) = member_name(cx, id) else {
                return false;
            };
            if is_write_only(cx, id) {
                return false;
            }
            let object = skip_wrappers(ast, obj);
            (VIEWPORT.contains(&name.as_str()) && is_global_object(cx, object))
                || (VISUAL_VIEWPORT.contains(&name.as_str())
                    && is_global_property(cx, object, "visualViewport"))
                || (ELEMENT_LAYOUT.contains(&name.as_str()) && !is_non_element(cx, obj))
        }
        _ => false,
    }
}

fn pattern_names(cx: &Ctx<'_, '_>, pattern: NodeId) -> Option<Vec<String>> {
    let ast = cx.ast();
    let key_name = |key: NodeId| match *ast.node(key) {
        Node::Ident { .. } => ident_name(ast, key).map(str::to_owned),
        Node::StringLit { value } => Some(ast.atom(value).to_owned()),
        Node::NumberLit { .. } => static_value(cx, key).and_then(|v| v.to_js_string()),
        Node::ComputedKey { expr } => static_key(cx, expr),
        _ => None,
    };
    let props = match *ast.node(skip_parens(ast, pattern)) {
        Node::ObjectPattern { props } | Node::ObjectLit { props } => props,
        _ => return None,
    };
    Some(
        ast.list(props)
            .iter()
            .filter_map(|&p| match *ast.node(p) {
                Node::BindingProperty { key, .. } | Node::Property { key, .. } => key_name(key),
                Node::ShorthandProp { name, .. } => key_name(name),
                _ => None,
            })
            .collect(),
    )
}

fn is_layout_destructuring(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let (pattern, source) = match *ast.node(id) {
        Node::VarDeclarator {
            name,
            init: Some(init),
            ..
        } if matches!(ast.node(name), Node::ObjectPattern { .. }) => (name, init),
        Node::Assign {
            left,
            op: AssignOp::Assign,
            right,
        } if matches!(
            ast.node(skip_parens(ast, left)),
            Node::ObjectPattern { .. } | Node::ObjectLit { .. }
        ) =>
        {
            (left, right)
        }
        _ => return false,
    };
    let Some(names) = pattern_names(cx, pattern) else {
        return false;
    };
    let has = |list: &[&str]| names.iter().any(|n| list.contains(&n.as_str()));
    let init = skip_wrappers(ast, source);
    (has(VIEWPORT) && is_global_object(cx, init))
        || (has(VISUAL_VIEWPORT) && is_global_property(cx, init, "visualViewport"))
        || (has(ELEMENT_LAYOUT) && !is_non_element(cx, init))
}

fn is_layout_method_call(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let Node::Call { callee, .. } = *ast.node(id) else {
        return false;
    };
    member_name(cx, skip_parens(ast, callee)).is_some_and(|(obj, name)| {
        LAYOUT_METHODS.contains(&name.as_str()) && !is_non_element(cx, obj)
    })
}

fn is_function(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(id),
        Node::FunctionDecl { .. } | Node::FunctionExpr { .. } | Node::Arrow { .. }
    )
}

fn contains_layout_read(cx: &Ctx<'_, '_>, root: NodeId) -> bool {
    let ast = cx.ast();
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        if id != root && is_function(ast, id) {
            continue;
        }
        if is_layout_method_call(cx, id)
            || is_layout_property_read(cx, id)
            || is_layout_destructuring(cx, id)
        {
            return true;
        }
        ast.for_each_child(id, |c| stack.push(c));
    }
    false
}

/// The function a listener argument is, or names.
pub(crate) fn listener_function(cx: &Ctx<'_, '_>, id: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let node = skip_wrappers(ast, id);
    if is_function(ast, node) {
        return Some(node);
    }
    if !matches!(ast.node(node), Node::Ident { .. }) {
        return None;
    }
    let sem = cx.semantic();
    let sym = sem
        .reference_at(node)
        .and_then(|r| sem.reference(r).symbol())?;
    let symbol = sem.symbol(sym);
    let &[decl] = &symbol.decls[..] else {
        return None;
    };
    if symbol.refs.iter().any(|&r| {
        let r = sem.reference(r);
        r.is_write() && r.node != decl.name
    }) {
        return None;
    }
    match *ast.node(decl.node) {
        Node::FunctionDecl { body: Some(_), .. } if decl.kind == DeclKind::Function => {
            Some(decl.node)
        }
        Node::VarDeclarator {
            name,
            init: Some(init),
            ..
        } if name == decl.name => {
            let init = skip_wrappers(ast, init);
            is_function(ast, init).then_some(init)
        }
        _ => None,
    }
}

impl Rule for PreferObserverApis {
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
        let args = ast.list(args);
        if !(2..=3).contains(&args.len())
            || args
                .iter()
                .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
        {
            return;
        }
        let callee = skip_parens(ast, callee);
        let registration = match *ast.node(callee) {
            Node::Member {
                optional: false,
                prop,
                ..
            } => ident_name(ast, prop) == Some("addEventListener"),
            Node::Ident { .. } => is_global_name(cx, callee, &["addEventListener"]),
            _ => false,
        };
        if !registration {
            return;
        }
        let event = skip_parens(ast, args[0]);
        let Some(name) = static_string(ast, skip_wrappers(ast, event)) else {
            return;
        };
        let (id, message) = match name.as_str() {
            "resize" => (
                "resize",
                "Prefer ResizeObserver over a resize listener with layout reads.",
            ),
            "scroll" => (
                "scroll",
                "Prefer IntersectionObserver over a scroll listener with layout reads.",
            ),
            _ => return,
        };
        let Some(listener) = listener_function(cx, args[1]) else {
            return;
        };
        let body = match *ast.node(listener) {
            Node::FunctionDecl { body: Some(b), .. } | Node::FunctionExpr { body: Some(b), .. } => {
                b
            }
            Node::Arrow { body, .. } => body,
            _ => return,
        };
        if !contains_layout_read(cx, body) {
            return;
        }
        cx.report(cx.span(event), message).id(id);
    }
}
