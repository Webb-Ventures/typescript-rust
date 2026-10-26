//! `unicorn/prefer-keyboard-event-key`: `KeyboardEvent#key` over `keyCode`, `charCode` and
//! `which`.
//!
//! Event parameters come from every function enclosing the use, innermost first: the first
//! parameter of an inline callback passed second to `x.addEventListener(type, cb)` (the only
//! context that may be autofixed), the first parameter of an inline callback in a JSX
//! `onKeyDown` / `onKeyPress` / `onKeyUp` attribute, and every parameter annotated
//! `KeyboardEvent` / `React.KeyboardEvent`. A parameter the syntax checker knows isn't a
//! `KeyboardEvent` (`MouseEvent`, `Event`, ...) doesn't count.
//!
//! Reported: `e.keyCode` where `e` refers to a plain identifier parameter of such a context;
//! a `keyCode` key in `const {keyCode} = e` (the declarator's own pattern, `e` that
//! parameter); and a `keyCode` key in an object-pattern parameter of such a context.
//!
//! `if (e.keyCode === 27)` (`==` too, a number on the right) is fixed to
//! `if (e.key === 'Escape')` when the number names a key: for `charCode` a printable
//! character, then the virtual-key table, then (not for `charCode`) a digit or capital
//! letter. A one-character key read from `keyCode` / `which` (other than space) depends on
//! the layout, so it, and every non-`addEventListener` context, gets a suggestion instead.
//!
//! "Printable" is Unicode letter, number, punctuation or symbol; outside ASCII it is
//! approximated by Rust's alphabetic / numeric classes and by excluding whitespace, control,
//! format, private-use and combining-mark ranges.

use super::prelude::*;
use super::unicorn_types::{Checker, Guess};
use super::unicorn_util_h5::{method_call_of, parent_through_parens};
use crate::semantic::SymbolId;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-keyboard-event-key",
        "Prefer `KeyboardEvent#key` over deprecated keyboard event properties.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create_default::<PreferKeyboardEventKey>,
};

#[derive(Debug, Default)]
struct PreferKeyboardEventKey;

/// unicorn's `isKnownNonKeyboardEvent`.
static KEYBOARD_EVENT: Checker = Checker {
    target_names: &["KeyboardEvent", "React.KeyboardEvent"],
    non_target_names: &[
        "Event",
        "MouseEvent",
        "PointerEvent",
        "React.MouseEvent",
        "React.PointerEvent",
    ],
    ..Checker::DEFAULT
};

const KEYS: &[&str] = &["keyCode", "charCode", "which"];

/// Virtual key codes whose `key` can't be read from the code point.
const EVENT_KEYS: &[(u32, &str)] = &[
    (8, "Backspace"),
    (9, "Tab"),
    (12, "Clear"),
    (13, "Enter"),
    (16, "Shift"),
    (17, "Control"),
    (18, "Alt"),
    (19, "Pause"),
    (20, "CapsLock"),
    (27, "Escape"),
    (32, " "),
    (33, "PageUp"),
    (34, "PageDown"),
    (35, "End"),
    (36, "Home"),
    (37, "ArrowLeft"),
    (38, "ArrowUp"),
    (39, "ArrowRight"),
    (40, "ArrowDown"),
    (45, "Insert"),
    (46, "Delete"),
    (91, "Meta"),
    (92, "Meta"),
    (93, "ContextMenu"),
    (112, "F1"),
    (113, "F2"),
    (114, "F3"),
    (115, "F4"),
    (116, "F5"),
    (117, "F6"),
    (118, "F7"),
    (119, "F8"),
    (120, "F9"),
    (121, "F10"),
    (122, "F11"),
    (123, "F12"),
    (144, "NumLock"),
    (145, "ScrollLock"),
    (186, ";"),
    (187, "="),
    (188, ","),
    (189, "-"),
    (190, "."),
    (191, "/"),
    (219, "["),
    (220, "\\"),
    (221, "]"),
    (222, "'"),
    (224, "Meta"),
];

/// One event parameter of an enclosing function.
struct EventContext {
    /// The parameter's pattern.
    pattern: NodeId,
    /// The parameter's symbol when it is a plain identifier without a default.
    symbol: Option<SymbolId>,
    autofix: bool,
}

impl Rule for PreferKeyboardEventKey {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Member, NodeKind::BindingProperty]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        match *cx.node(node) {
            Node::Member { obj, prop, .. } => {
                let obj = skip_parens(ast, obj);
                if !ident_name(ast, prop).is_some_and(|n| KEYS.contains(&n))
                    || !matches!(ast.node(obj), Node::Ident { .. })
                {
                    return;
                }
                let sem = cx.semantic();
                let Some(symbol) = sem
                    .reference_at(obj)
                    .and_then(|r| sem.reference(r).symbol())
                else {
                    return;
                };
                if let Some(ctx) = event_contexts(cx, node)
                    .into_iter()
                    .find(|c| c.symbol == Some(symbol))
                {
                    report(cx, prop, Some(node), ctx.autofix);
                }
            }
            Node::BindingProperty { key, .. } => {
                if !ident_name(ast, key).is_some_and(|n| KEYS.contains(&n)) {
                    return;
                }
                let Some(pattern) = cx.parent(node) else {
                    return;
                };
                if !matches!(cx.node(pattern), Node::ObjectPattern { .. }) {
                    return;
                }
                let contexts = event_contexts(cx, node);
                let init_symbol = destructured_init_symbol(cx, node, pattern);
                let ctx = contexts
                    .iter()
                    .find(|c| init_symbol.is_some() && c.symbol == init_symbol)
                    .or_else(|| contexts.iter().find(|c| c.pattern == pattern));
                if let Some(ctx) = ctx {
                    report(cx, key, None, ctx.autofix);
                }
            }
            _ => {}
        }
    }
}

/// The symbol of `e` in `const {keyCode} = e` when the nearest declarator's own pattern
/// holds the key.
fn destructured_init_symbol(cx: &Ctx<'_, '_>, from: NodeId, pattern: NodeId) -> Option<SymbolId> {
    let ast = cx.ast();
    let mut cur = from;
    for _ in 0..10_000 {
        cur = cx.parent(cur)?;
        if let Node::VarDeclarator { name, init, .. } = *ast.node(cur) {
            if name != pattern {
                return None;
            }
            let init = skip_parens(ast, init?);
            if !matches!(ast.node(init), Node::Ident { .. }) {
                return None;
            }
            let sem = cx.semantic();
            return sem
                .reference_at(init)
                .and_then(|r| sem.reference(r).symbol());
        }
    }
    None
}

fn function_params(ast: &ts_r_ast::Ast, f: NodeId) -> Option<(ts_r_ast::NodeList, bool)> {
    match *ast.node(f) {
        Node::FunctionDecl { params, .. } => Some((params, false)),
        Node::FunctionExpr { params, .. } | Node::Arrow { params, .. } => Some((params, true)),
        _ => None,
    }
}

fn event_contexts(cx: &Ctx<'_, '_>, node: NodeId) -> Vec<EventContext> {
    let ast = cx.ast();
    let mut out = Vec::new();
    let mut cur = node;
    for _ in 0..10_000 {
        let Some(p) = cx.parent(cur) else {
            break;
        };
        cur = p;
        let Some((params, inline)) = function_params(ast, cur) else {
            continue;
        };
        let params = ast.list(params);
        let mut found: Vec<(NodeId, bool)> = Vec::new();
        if inline && let Some(&first) = params.first() {
            if is_listener_callback(cx, cur) {
                found.push((first, true));
            }
            if is_keyboard_jsx_handler(cx, cur) {
                found.push((first, false));
            }
        }
        for &param in params {
            if let Node::Param {
                type_ann: Some(t), ..
            } = *ast.node(param)
                && is_keyboard_event_annotation(ast, t)
            {
                found.push((param, false));
            }
        }
        for (param, autofix) in found {
            let Node::Param {
                pattern,
                init,
                rest,
                type_ann,
                ..
            } = *ast.node(param)
            else {
                continue;
            };
            let known_non = match type_ann {
                Some(t) => KEYBOARD_EVENT.annotation(cx, t) == Guess::NonTarget,
                None => KEYBOARD_EVENT.is_known_non_target(cx, pattern),
            };
            if known_non {
                continue;
            }
            let plain = !rest && init.is_none() && matches!(ast.node(pattern), Node::Ident { .. });
            let pattern_node = if rest || init.is_some() {
                param
            } else {
                pattern
            };
            out.push(EventContext {
                pattern: pattern_node,
                symbol: plain
                    .then(|| cx.semantic().symbol_of_decl(pattern))
                    .flatten(),
                autofix,
            });
        }
    }
    out
}

fn is_listener_callback(cx: &Ctx<'_, '_>, f: NodeId) -> bool {
    let Some((call, child)) = parent_through_parens(cx, f) else {
        return false;
    };
    method_call_of(cx.ast(), call, &["addEventListener"], 2, None, Some(false))
        .is_some_and(|(_, _, args)| args[1] == child)
}

fn is_keyboard_jsx_handler(cx: &Ctx<'_, '_>, f: NodeId) -> bool {
    let Some((container, _)) = parent_through_parens(cx, f) else {
        return false;
    };
    if !matches!(cx.node(container), Node::JsxExprContainer { .. }) {
        return false;
    }
    let Some(attr) = cx.parent(container) else {
        return false;
    };
    matches!(*cx.node(attr), Node::JsxAttr { name, value: Some(v) }
        if v == container
            && matches!(ident_name(cx.ast(), name), Some("onKeyDown" | "onKeyPress" | "onKeyUp")))
}

fn is_keyboard_event_annotation(ast: &ts_r_ast::Ast, t: NodeId) -> bool {
    let Node::TypeRef { name, .. } = *ast.node(t) else {
        return false;
    };
    match *ast.node(name) {
        Node::Ident { .. } => ident_name(ast, name) == Some("KeyboardEvent"),
        Node::QualifiedName { left, right } => {
            ident_name(ast, left) == Some("React")
                && ident_name(ast, right) == Some("KeyboardEvent")
        }
        _ => false,
    }
}

fn is_printable(c: char) -> bool {
    if c.is_ascii() {
        return c.is_ascii_alphanumeric() || c.is_ascii_punctuation();
    }
    if c.is_alphabetic() || c.is_numeric() {
        return true;
    }
    let u = c as u32;
    let excluded = c.is_whitespace()
        || c.is_control()
        || matches!(u,
            0x00AD
            | 0x0300..=0x036F
            | 0x0483..=0x0489
            | 0x0591..=0x05BD
            | 0x0610..=0x061A
            | 0x064B..=0x065F
            | 0x1AB0..=0x1AFF
            | 0x1DC0..=0x1DFF
            | 0x200B..=0x200F
            | 0x202A..=0x202E
            | 0x2060..=0x206F
            | 0x20D0..=0x20FF
            | 0xD800..=0xF8FF
            | 0xFE00..=0xFE0F
            | 0xFE20..=0xFE2F
            | 0xFEFF
            | 0xFFF9..=0xFFFB
            | 0xE0000..=0xE007F
            | 0xE0100..=0xE01EF
            | 0xF0000..);
    !excluded
}

/// The `key` a number names, read as `name`.
fn key_for(value: f64, name: &str) -> Option<String> {
    let character = (value.fract() == 0.0 && (0.0..=1_114_111.0).contains(&value))
        .then(|| char::from_u32(value as u32))
        .flatten();
    if name == "charCode"
        && let Some(c) = character
        && is_printable(c)
    {
        return Some(c.to_string());
    }
    if value.fract() == 0.0
        && let Some((_, key)) = EVENT_KEYS
            .iter()
            .find(|(code, _)| f64::from(*code) == value)
    {
        return Some((*key).to_owned());
    }
    match character {
        Some(c) if name != "charCode" && (c.is_ascii_digit() || c.is_ascii_uppercase()) => {
            Some(c.to_string())
        }
        _ => None,
    }
}

fn quote(s: &str) -> String {
    let mut out = String::from("'");
    for c in s.chars() {
        match c {
            '\'' => out.push_str("\\'"),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            _ => out.push(c),
        }
    }
    out.push('\'');
    out
}

/// For `if (e.keyCode === 27)`: the number node and the key it names.
fn replacement(cx: &Ctx<'_, '_>, member: NodeId, name: &str) -> Option<(NodeId, String)> {
    let ast = cx.ast();
    let (binary, left) = parent_through_parens(cx, member)?;
    let Node::Binary {
        left: l,
        op: BinaryOp::Eq | BinaryOp::StrictEq,
        right,
    } = *ast.node(binary)
    else {
        return None;
    };
    if l != left {
        return None;
    }
    let (stmt, _) = parent_through_parens(cx, binary)?;
    if !matches!(ast.node(stmt), Node::If { .. }) {
        return None;
    }
    let right = skip_parens(ast, right);
    let Node::NumberLit { value } = *ast.node(right) else {
        return None;
    };
    Some((right, key_for(value.get(), name)?))
}

fn report(cx: &mut Ctx<'_, '_>, prop: NodeId, member: Option<NodeId>, autofix: bool) {
    let name = ident_name(cx.ast(), prop).unwrap_or_default().to_owned();
    let found = member.and_then(|m| replacement(cx, m, &name));
    let message = format!("Use `.key` instead of `.{name}`.");
    let Some((right, key)) = found else {
        cx.report_node(prop, message)
            .id("prefer-keyboard-event-key");
        return;
    };
    let fix = Fix::replace(cx.span(prop), "key").and(cx.span(right), quote(&key));
    let layout_dependent = name != "charCode" && key.chars().count() == 1 && key != " ";
    let r = cx
        .report_node(prop, message)
        .id("prefer-keyboard-event-key");
    if autofix && !layout_dependent {
        r.fix(fix);
    } else {
        r.suggest(
            "prefer-keyboard-event-key/suggestion",
            format!("Use `.key` with `{key}`."),
            fix,
        );
    }
}
