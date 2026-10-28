//! `unicorn/prefer-global-this`: references to the globals `window`, `self` and `global`
//! (not shadowed) are reported in favour of `globalThis`, and fixed unless written to
//! (`window` can't be reassigned in a browser, `globalThis` can).
//!
//! Left alone: `typeof window` (through TS wrappers) and type queries; and for `window`,
//! computed members (`window[x]`), `"name" in window` for a window-specific API, and
//! `window.<api>` for those APIs (frames, `innerWidth`, `postMessage`, `onresize`, ...),
//! where `window.addEventListener` and friends count only with a window-specific event
//! name as the first argument. Shorthand properties become `window: globalThis`, a
//! shorthand export `globalThis as window`.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-global-this",
        "Prefer `globalThis` over `window`, `self`, and `global`.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<PreferGlobalThis>,
};

#[derive(Debug, Default)]
struct PreferGlobalThis;

const GLOBALS: &[&str] = &["window", "self", "global"];

const EVENTS: &[&str] = &[
    "resize",
    "blur",
    "focus",
    "load",
    "scroll",
    "scrollend",
    "wheel",
    "beforeunload",
    "message",
    "messageerror",
    "pagehide",
    "pagereveal",
    "pageshow",
    "pageswap",
    "unload",
];

const APIS: &[&str] = &[
    "name",
    "locationbar",
    "menubar",
    "personalbar",
    "scrollbars",
    "statusbar",
    "toolbar",
    "status",
    "close",
    "closed",
    "stop",
    "focus",
    "blur",
    "frames",
    "length",
    "top",
    "opener",
    "parent",
    "frameElement",
    "open",
    "originAgentCluster",
    "postMessage",
    "navigation",
    "addEventListener",
    "removeEventListener",
    "dispatchEvent",
    "event",
    "screen",
    "visualViewport",
    "moveTo",
    "moveBy",
    "resizeTo",
    "resizeBy",
    "innerWidth",
    "innerHeight",
    "outerWidth",
    "outerHeight",
    "scrollX",
    "pageXOffset",
    "scrollY",
    "pageYOffset",
    "scroll",
    "scrollTo",
    "scrollBy",
    "screenX",
    "screenLeft",
    "screenY",
    "screenTop",
    "screenWidth",
    "screenHeight",
    "devicePixelRatio",
];

fn is_window_api(name: &str) -> bool {
    APIS.contains(&name) || name.strip_prefix("on").is_some_and(|e| EVENTS.contains(&e))
}

impl Rule for PreferGlobalThis {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Program]
    }
    fn enter(&self, _node: NodeId, cx: &mut Ctx<'_, '_>) {
        let sem = cx.semantic();
        let mut found: Vec<(NodeId, bool)> = Vec::new();
        for r in sem.references() {
            if r.flags.contains(RefFlags::IMPLICIT) {
                continue;
            }
            let global = match r.resolution {
                Resolution::Symbol(s) => sem.symbol(s).flags.contains(SymbolFlags::COMMENT_GLOBAL),
                _ => true,
            };
            let name = cx.ast().atom(r.name);
            if !global || !GLOBALS.contains(&name) {
                continue;
            }
            let write = r.flags.contains(RefFlags::WRITE);
            match found.iter_mut().find(|(n, _)| *n == r.node) {
                Some(f) => f.1 |= write,
                None => found.push((r.node, write)),
            }
        }
        found.sort_by_key(|(n, _)| cx.span(*n).start);
        for (ident, written) in found {
            self.check(cx, ident, written);
        }
    }
}

impl PreferGlobalThis {
    fn check(&self, cx: &mut Ctx<'_, '_>, ident: NodeId, written: bool) {
        let ast = cx.ast();
        let Some(name) = ident_name(ast, ident) else {
            return;
        };
        if is_typeof_operand(cx, ident) {
            return;
        }
        if name == "window" && window_specific(cx, ident) {
            return;
        }
        let span = cx.span(ident);
        let fix = (!written).then(|| {
            let parent = cx.parent(ident).map(|p| cx.node(p));
            let text = match parent {
                Some(Node::ShorthandProp { .. })
                | Some(Node::BindingProperty { value: None, .. }) => {
                    format!("{name}: globalThis")
                }
                Some(Node::ExportSpecifier { exported: None, .. }) => {
                    format!("globalThis as {name}")
                }
                _ => "globalThis".to_owned(),
            };
            Fix::replace(span, text)
        });
        cx.report(span, format!("Prefer `globalThis` over `{name}`."))
            .id("prefer-global-this/error")
            .fix_opt(fix);
    }
}

/// `typeof x` (through parentheses and TS wrappers), or `x` inside a type query.
fn is_typeof_operand(cx: &Ctx<'_, '_>, ident: NodeId) -> bool {
    let ast = cx.ast();
    let mut cur = ident;
    while let Some(p) = cx.parent(cur) {
        match *ast.node(p) {
            Node::Paren { .. }
            | Node::As { .. }
            | Node::Satisfies { .. }
            | Node::NonNull { .. }
            | Node::TypeAssertion { .. } => cur = p,
            Node::Unary {
                op: UnaryOp::TypeOf,
                ..
            } => return true,
            _ => break,
        }
    }
    ancestors_inclusive(cx.file.parents, ident)
        .skip(1)
        .take_while(|&a| {
            matches!(
                ast.node(a),
                Node::QualifiedName { .. } | Node::TypeQuery { .. }
            )
        })
        .any(|a| matches!(ast.node(a), Node::TypeQuery { .. }))
}

fn window_specific(cx: &Ctx<'_, '_>, ident: NodeId) -> bool {
    let ast = cx.ast();
    let src = cx.src();
    let Some(parent) = cx.parent(ident) else {
        return false;
    };
    match *ast.node(parent) {
        Node::Index { obj, .. } => obj == ident,
        Node::Binary {
            left,
            op: BinaryOp::In,
            right,
        } if right == ident => {
            static_value_string(ast, src, left).is_some_and(|n| is_window_api(&n))
        }
        Node::Member { obj, prop, .. } if obj == ident => {
            let Some(p) = ident_name(ast, prop) else {
                return false;
            };
            if !is_window_api(p) {
                return false;
            }
            if matches!(
                p,
                "addEventListener" | "removeEventListener" | "dispatchEvent"
            ) && let Some(call) = cx.parent(parent)
                && let Node::Call { callee, args, .. } = *ast.node(call)
                && callee == parent
            {
                return ast.list(args).first().is_some_and(|&a| {
                    static_value_string(ast, src, a).is_some_and(|e| EVENTS.contains(&e.as_ref()))
                });
            }
            true
        }
        _ => false,
    }
}
