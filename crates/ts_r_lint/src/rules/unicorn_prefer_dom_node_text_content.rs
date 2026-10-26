//! `unicorn/prefer-dom-node-text-content`: `.textContent` over `.innerText`.
//!
//! - `obj.innerText` / `obj?.innerText` (a non-computed name) is reported on the name unless
//!   the syntax checker knows `obj` isn't a DOM node.
//! - A non-computed `innerText` key in a destructuring pattern (a binding pattern or an object
//!   literal written to) is reported unless the pattern itself (a typed parameter), the
//!   declarator's initializer or the assignment's right side is known not to be a DOM node.
//!
//! The suggestion renames the key, keeping the binding (`{innerText}` becomes
//! `{textContent: innerText}`).

use super::prelude::*;
use super::unicorn_types::Checker;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-dom-node-text-content",
        "Prefer `.textContent` over `.innerText`.",
        NO_PRESET,
        Category::Style,
    )
    .suggestions(),
    create: create_default::<PreferDomNodeTextContent>,
};

#[derive(Debug, Default)]
struct PreferDomNodeTextContent;

/// unicorn's `isKnownNonDomNode`.
static DOM_NODE: Checker = Checker {
    target_names: &[
        "CharacterData",
        "Document",
        "DocumentFragment",
        "Element",
        "HTMLElement",
        "Node",
        "SVGElement",
        "Text",
    ],
    ..Checker::DEFAULT
};

impl Rule for PreferDomNodeTextContent {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::Member,
            NodeKind::BindingProperty,
            NodeKind::Property,
            NodeKind::ShorthandProp,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        match *cx.node(node) {
            Node::Member { obj, prop, .. } => {
                if ident_name(ast, prop) == Some("innerText")
                    && !DOM_NODE.is_known_non_target(cx, obj)
                {
                    report(cx, prop, false);
                }
            }
            Node::BindingProperty { key, value, .. } => {
                if ident_name(ast, key) != Some("innerText") {
                    return;
                }
                let Some(pattern) = cx.parent(node) else {
                    return;
                };
                if !matches!(cx.node(pattern), Node::ObjectPattern { .. })
                    || binding_pattern_known_non_dom(cx, pattern)
                {
                    return;
                }
                report(cx, key, value.is_none());
            }
            Node::Property { key, .. } | Node::ShorthandProp { name: key, .. } => {
                if ident_name(ast, key) != Some("innerText") {
                    return;
                }
                let Some(pattern) = cx.parent(node) else {
                    return;
                };
                if !matches!(cx.node(pattern), Node::ObjectLit { .. })
                    || !is_assignment_target(ast, cx.file.parents, pattern)
                    || assigned_pattern_known_non_dom(cx, pattern)
                {
                    return;
                }
                let shorthand = matches!(cx.node(node), Node::ShorthandProp { .. });
                report(cx, key, shorthand);
            }
            _ => {}
        }
    }
}

fn report(cx: &mut Ctx<'_, '_>, key: NodeId, shorthand: bool) {
    let text = if shorthand {
        "textContent: innerText"
    } else {
        "textContent"
    };
    let fix = Fix::replace(cx.span(key), text);
    cx.report_node(key, "Prefer `.textContent` over `.innerText`.")
        .id("error")
        .suggest("suggestion", "Switch to `.textContent`.", fix);
}

fn binding_pattern_known_non_dom(cx: &Ctx<'_, '_>, pattern: NodeId) -> bool {
    match cx.parent(pattern).map(|p| (p, *cx.node(p))) {
        Some((
            _,
            Node::Param {
                type_ann: Some(t), ..
            },
        )) => DOM_NODE.annotation(cx, t) == super::unicorn_types::Guess::NonTarget,
        Some((
            _,
            Node::VarDeclarator {
                name,
                init: Some(init),
                type_ann,
                ..
            },
        )) if name == pattern => {
            type_ann.is_some_and(|t| {
                DOM_NODE.annotation(cx, t) == super::unicorn_types::Guess::NonTarget
            }) || DOM_NODE.is_known_non_target(cx, init)
        }
        _ => false,
    }
}

fn assigned_pattern_known_non_dom(cx: &Ctx<'_, '_>, pattern: NodeId) -> bool {
    let mut cur = pattern;
    while let Some(p) = cx.parent(cur) {
        match *cx.node(p) {
            Node::Paren { .. } => cur = p,
            Node::Assign { left, right, .. } => {
                return left == cur && DOM_NODE.is_known_non_target(cx, right);
            }
            _ => return false,
        }
    }
    false
}
