//! `unicorn/prefer-toggle-attribute`: `element.toggleAttribute(name[, force])` over an
//! `if`/`else` or `?:` choosing between `setAttribute(name, value)` and
//! `removeAttribute(name)`.
//!
//! Each branch is (alone, or alone in a block) a call `<receiver>.setAttribute(name, value)`
//! or `<receiver>.removeAttribute(name)`: plain member (`<receiver>?.` allowed, the same on
//! both), not an optional call, exact argument count, no spread, and not a static `data-*`
//! name. The receivers and names are the same references; neither receiver has a `?.`
//! inside it or is known not to be a DOM node (a literal, function, class, ... or typed
//! otherwise).
//!
//! A test that is (possibly negated) `<receiver>.hasAttribute(name)` on the same receiver
//! and name, in the direction that makes it a plain toggle, is dropped
//! (`toggleAttribute(name)`); a known non-DOM receiver there means no report. Otherwise the
//! test becomes the `force` argument, negated when the true branch removes (a leading `!`
//! is dropped instead). The fix is withheld when a comment outside the receiver, the name
//! (and the kept test) would be lost, when `setAttribute`'s value isn't a static string, and
//! when a `force` would follow a `?.` receiver (it would not be evaluated). It is a
//! suggestion when a `force` is passed, the value isn't `""`, or the `?:` is not a whole
//! expression statement.

use super::prelude::*;
use super::unicorn_dom_node_dataset::DOM_NODE;
use super::unicorn_text::{
    expression_text, is_parenthesized, is_same_reference, member_object_text, needs_semicolon,
    paren_text, would_remove_comments,
};
use super::unicorn_util::{has_optional_chain, space_around_keyword};
use super::unicorn_util_h5::is_value_not_dom_node;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-toggle-attribute",
        "Prefer using `Element#toggleAttribute()` to toggle attributes.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create_default::<PreferToggleAttribute>,
};

#[derive(Debug, Default)]
struct PreferToggleAttribute;

#[derive(Clone, Copy, PartialEq)]
enum Method {
    Set,
    Remove,
    Has,
}

#[derive(Clone, Copy)]
struct AttrCall {
    method: Method,
    receiver: NodeId,
    name: NodeId,
    value: Option<NodeId>,
    optional: bool,
}

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

fn attribute_call(ast: &Ast, id: NodeId) -> Option<AttrCall> {
    let Node::Call {
        callee,
        optional: false,
        args,
        ..
    } = *ast.node(skip_parens(ast, id))
    else {
        return None;
    };
    let Node::Member {
        obj,
        optional,
        prop,
    } = *ast.node(skip_parens(ast, callee))
    else {
        return None;
    };
    let args = ast.list(args);
    let method = match ident_name(ast, prop)? {
        "setAttribute" if args.len() == 2 => Method::Set,
        "removeAttribute" if args.len() == 1 => Method::Remove,
        "hasAttribute" if args.len() == 1 => Method::Has,
        _ => return None,
    };
    if args
        .iter()
        .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
        || static_string(ast, args[0]).is_some_and(|n| n.to_lowercase().starts_with("data-"))
    {
        return None;
    }
    Some(AttrCall {
        method,
        receiver: obj,
        name: args[0],
        value: args.get(1).copied(),
        optional,
    })
}

fn clause_call(ast: &Ast, id: Option<NodeId>) -> Option<AttrCall> {
    let mut id = id?;
    if let Node::Block { stmts } = *ast.node(id) {
        let &[only] = ast.list(stmts) else {
            return None;
        };
        id = only;
    }
    if let Node::ExprStmt { expr } = *ast.node(id) {
        id = expr;
    }
    attribute_call(ast, id)
}

fn same_target(cx: &Ctx<'_, '_>, a: &AttrCall, b: &AttrCall) -> bool {
    a.optional == b.optional
        && is_same_reference(cx, a.receiver, b.receiver)
        && is_same_reference(cx, a.name, b.name)
}

fn supported_receiver(cx: &Ctx<'_, '_>, call: &AttrCall) -> bool {
    !has_optional_chain(cx.ast(), call.receiver)
}

fn known_non_dom(cx: &Ctx<'_, '_>, call: &AttrCall) -> bool {
    is_value_not_dom_node(cx.ast(), call.receiver)
        || DOM_NODE.is_known_non_target(cx, call.receiver)
}

/// A matching `hasAttribute` test: `(negative, known non-DOM)`.
fn has_attribute_test(cx: &Ctx<'_, '_>, test: NodeId, set: &AttrCall) -> Option<(bool, bool)> {
    let ast = cx.ast();
    let mut id = skip_parens(ast, test);
    let mut negative = false;
    for _ in 0..10_000 {
        match *ast.node(id) {
            Node::Unary {
                op: UnaryOp::Not,
                arg,
            } => {
                negative = !negative;
                id = skip_parens(ast, arg);
            }
            _ => break,
        }
    }
    let call = attribute_call(ast, id)?;
    (call.method == Method::Has && same_target(cx, &call, set) && supported_receiver(cx, &call))
        .then(|| (negative, known_non_dom(cx, &call)))
}

fn condition_text(cx: &Ctx<'_, '_>, test: NodeId, negative: bool, depth: u32) -> String {
    let ast = cx.ast();
    if negative {
        let inner = skip_parens(ast, test);
        if depth < 100
            && let Node::Unary {
                op: UnaryOp::Not,
                arg,
            } = *ast.node(inner)
            && ast.comments_in(cx.span(inner)).len() == ast.comments_in(cx.span(arg)).len()
        {
            return condition_text(cx, arg, false, depth + 1);
        }
        let parens = precedence(ast, inner) < 16
            || matches!(
                ast.node(inner),
                Node::Update { .. } | Node::NonNull { .. } | Node::TypeAssertion { .. }
            );
        return format!("!{}", expression_text(cx, inner, parens));
    }
    let inner = skip_parens(ast, test);
    let text = paren_text(cx, inner);
    if !is_parenthesized(cx, inner) && matches!(ast.node(inner), Node::Seq { .. }) {
        return format!("({text})");
    }
    text.to_owned()
}

fn parent_np(cx: &Ctx<'_, '_>, id: NodeId) -> Option<NodeId> {
    let mut cur = id;
    loop {
        let p = cx.parent(cur)?;
        if matches!(cx.node(p), Node::Paren { .. }) {
            cur = p;
        } else {
            return Some(p);
        }
    }
}

impl Rule for PreferToggleAttribute {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::If, NodeKind::Conditional]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let (test, cons, alt, is_if) = match *ast.node(node) {
            Node::If { test, cons, alt } => (test, cons, alt, true),
            Node::Conditional { test, cons, alt } => (test, cons, Some(alt), false),
            _ => return,
        };
        let (Some(c), Some(a)) = (clause_call(ast, Some(cons)), clause_call(ast, alt)) else {
            return;
        };
        let pair = matches!(
            (c.method, a.method),
            (Method::Set, Method::Remove) | (Method::Remove, Method::Set)
        );
        if !pair
            || !same_target(cx, &c, &a)
            || !supported_receiver(cx, &c)
            || !supported_receiver(cx, &a)
            || known_non_dom(cx, &c)
            || known_non_dom(cx, &a)
        {
            return;
        }
        let set_when_true = c.method == Method::Set;
        let set = if set_when_true { c } else { a };
        let has = has_attribute_test(cx, test, &set);
        if has.is_some_and(|(_, non_dom)| non_dom) {
            return;
        }
        let without_force = has.is_some_and(|(negative, _)| set_when_true == negative);
        let condition = if without_force {
            String::new()
        } else {
            condition_text(cx, test, !set_when_true, 0)
        };
        let value = set.value.and_then(|v| static_string(ast, v));
        let empty_value = value.as_deref() == Some("");
        let report_only = (!condition.is_empty() && set.optional) || value.is_none();
        let span = cx.span(node);
        let mut kept = vec![cx.span(set.receiver), cx.span(set.name)];
        if !condition.is_empty() {
            kept.push(cx.span(test));
        }
        let fix = (!report_only && !would_remove_comments(cx, span, &kept)).then(|| {
            let mut text = format!(
                "{}{}.toggleAttribute({}{})",
                member_object_text(cx, set.receiver, node),
                if set.optional { "?" } else { "" },
                paren_text(cx, set.name),
                if condition.is_empty() {
                    String::new()
                } else {
                    format!(", {condition}")
                }
            );
            if is_if {
                text.push(';');
            }
            if needs_semicolon(cx, span.start, &text) {
                text.insert(0, ';');
            }
            let mut fix = Fix::replace(span, text);
            if !is_if {
                for (s, t) in space_around_keyword(cx, node) {
                    fix = fix.and(s, t);
                }
            }
            fix
        });
        let statement = is_if
            || matches!(cx.node(node), Node::ExprStmt { .. })
            || parent_np(cx, node).is_some_and(|p| matches!(cx.node(p), Node::ExprStmt { .. }));
        let suggest = !condition.is_empty() || !empty_value || !statement;
        let r = cx
            .report(
                span,
                "Prefer using `Element#toggleAttribute()` to toggle attributes.",
            )
            .id("prefer-toggle-attribute/error");
        match fix {
            Some(f) if suggest => {
                r.suggest(
                    "prefer-toggle-attribute/suggestion",
                    "Replace with `Element#toggleAttribute()`.",
                    f,
                );
            }
            Some(f) => {
                r.fix(f);
            }
            None => {}
        }
    }
}
