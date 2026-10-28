//! `unicorn/no-unsafe-dom-html`: writes to `innerHTML`, `outerHTML` and `srcdoc` (plain and
//! compound assignments, `for-in`/`for-of` heads and destructuring targets; static computed
//! names count), calls of `createContextualFragment`, `insertAdjacentHTML` and
//! `setHTMLUnsafe`, `setAttribute("srcdoc", ...)` (any case, two or more arguments), and calls
//! of the globals `Document.parseHTMLUnsafe`, `document.write` and `document.writeln` (followed
//! through `globalThis`-like objects and `const` aliases, as ESLint's reference tracker does).
//!
//! Member reports go on the property (the computed expression for `x["innerHTML"]`); global
//! calls are reported on the whole callee.

use super::prelude::*;
use super::unicorn_text::static_string_value;
use super::unicorn_util::{global_path, skip_wrappers, static_property_name};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-unsafe-dom-html",
        "Disallow unsafe DOM HTML APIs.",
        NO_PRESET,
        Category::Restriction,
    ),
    create: create_default::<NoUnsafeDomHtml>,
};

#[derive(Debug, Default)]
struct NoUnsafeDomHtml;

const MESSAGE: &str = "Do not use unsafe DOM HTML APIs.";
const HTML_PROPERTIES: &[&str] = &["innerHTML", "outerHTML", "srcdoc"];
const HTML_METHODS: &[&str] = &[
    "createContextualFragment",
    "insertAdjacentHTML",
    "setHTMLUnsafe",
];
const GLOBAL_CALLS: &[&str] = &[
    "Document.parseHTMLUnsafe",
    "document.write",
    "document.writeln",
];

/// The property node of a member access (`b` of `a.b`, the expression of `a[b]`).
fn property_node(cx: &Ctx<'_, '_>, member: NodeId) -> Option<NodeId> {
    match *cx.node(member) {
        Node::Member { prop, .. } => Some(prop),
        Node::Index { index, .. } => Some(index),
        _ => None,
    }
}

/// `member` is written by its parent: an assignment's left side, a `for-in`/`for-of` head,
/// or a target inside a destructuring assignment (not a default value).
fn is_written(cx: &Ctx<'_, '_>, member: NodeId) -> bool {
    let ast = cx.ast();
    let mut cur = member;
    for _ in 0..10_000 {
        let Some(p) = cx.parent(cur) else {
            return false;
        };
        match *ast.node(p) {
            Node::Paren { .. } | Node::ArrayLit { .. } | Node::Spread { .. } => {}
            Node::ObjectLit { .. } => {}
            Node::Property { value, .. } if value == cur => {}
            Node::Assign { left, .. } => return left == cur,
            Node::ForIn { left, .. } | Node::ForOf { left, .. } => return left == cur,
            _ => return false,
        }
        cur = p;
    }
    false
}

/// The callee could name one of `GLOBAL_CALLS`: a member whose last name matches, or a local
/// alias (only those are traced).
fn may_be_global_call(cx: &Ctx<'_, '_>, callee: NodeId) -> bool {
    let callee = skip_wrappers(cx.ast(), callee);
    match *cx.node(callee) {
        Node::Member { .. } | Node::Index { .. } => static_property_name(cx, callee)
            .is_some_and(|n| matches!(n.as_str(), "write" | "writeln" | "parseHTMLUnsafe")),
        Node::Ident { .. } => {
            let sem = cx.semantic();
            sem.reference_at(callee)
                .is_some_and(|r| matches!(sem.reference(r).resolution, Resolution::Symbol(_)))
        }
        _ => false,
    }
}

impl Rule for NoUnsafeDomHtml {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Member, NodeKind::Index, NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        match *ast.node(node) {
            Node::Member { .. } | Node::Index { .. } => {
                let Some(name) = static_property_name(cx, node) else {
                    return;
                };
                if !HTML_PROPERTIES.contains(&name.as_str()) || !is_written(cx, node) {
                    return;
                }
                if let Some(prop) = property_node(cx, node) {
                    cx.report_node(prop, MESSAGE).id("no-unsafe-dom-html");
                }
            }
            Node::Call { callee, args, .. } => {
                let member = skip_parens(ast, callee);
                if matches!(ast.node(member), Node::Member { .. } | Node::Index { .. })
                    && let Some(name) = static_property_name(cx, member)
                {
                    let args = ast.list(args);
                    let unsafe_call = HTML_METHODS.contains(&name.as_str())
                        || (name == "setAttribute"
                            && args.len() >= 2
                            && static_string_value(ast, args[0])
                                .is_some_and(|v| v.to_lowercase() == "srcdoc"));
                    if unsafe_call {
                        if let Some(prop) = property_node(cx, member) {
                            cx.report_node(prop, MESSAGE).id("no-unsafe-dom-html");
                        }
                        return;
                    }
                }
                if may_be_global_call(cx, callee)
                    && global_path(cx, callee).is_some_and(|p| GLOBAL_CALLS.contains(&p.as_str()))
                {
                    cx.report_node(callee, MESSAGE).id("no-unsafe-dom-html");
                }
            }
            _ => {}
        }
    }
}
