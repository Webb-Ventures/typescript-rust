//! `unicorn/prefer-add-event-listener`: `addEventListener` over `on*` handler properties.
//!
//! An assignment (any operator) to a non-computed member `x.on<type>`, where `<type>` is a
//! known DOM event name, is reported at the property. Assigning `null`, `undefined` or
//! `void x` (through TS wrappers) asks for `removeEventListener`. `beforeunload` handlers
//! that are not block-bodied functions without a value-returning `return` get a note about
//! `returnValue`; `message` and `error` get a note about the differences and no fix.
//!
//! Otherwise `x.onclick = f;` is fixed to `x.addEventListener('click', f)` when it is a
//! plain `=` whose parent is an expression statement, `f` can be a function (not a literal,
//! array, object, class, template, unary/update/binary expression, assignment, `await`,
//! `new`, tagged template, `this`, `undefined`, or a call other than `.bind(...)`), no
//! comment outside the receiver (with its parentheses) and the handler is lost, and no
//! other assignment anywhere in the file targets the same reference (a later `= null`
//! would no longer remove the listener).
//!
//! A `require('<pkg>')` call or `import ... from '<pkg>'` for an excluded package (option
//! `excludedPackages`, default `koa` and `sax`) turns the rule off for every assignment
//! that ends after it.

use super::prelude::*;
use super::unicorn_util::{
    is_parenthesized, is_same_reference, is_value_not_function, member_object_text, static_string,
    would_remove_comments,
};
use super::unicorn_util::{parenthesized_span, skip_wrappers, space_around_keyword, with_edits};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-add-event-listener",
        "Prefer `.addEventListener()` and `.removeEventListener()` over `on`-functions.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<PreferAddEventListener>,
};

#[derive(Debug)]
struct PreferAddEventListener {
    excluded: Vec<String>,
}

impl FromOptions for PreferAddEventListener {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        let excluded = if o.contains_key("excludedPackages") {
            str_list(o, "excludedPackages")
        } else {
            vec!["koa".to_owned(), "sax".to_owned()]
        };
        Ok(PreferAddEventListener { excluded })
    }
}

const EVENTS: &[&str] = &[
    "click",
    "rightclick",
    "dblclick",
    "auxclick",
    "mousedown",
    "mouseup",
    "mouseover",
    "mouseout",
    "mousemove",
    "mouseenter",
    "mouseleave",
    "mousecancel",
    "selectionchange",
    "selectstart",
    "wheel",
    "keypress",
    "keydown",
    "keyup",
    "blur",
    "focus",
    "deactivate",
    "focusin",
    "focusout",
    "change",
    "reset",
    "select",
    "submit",
    "input",
    "propertychange",
    "dragstart",
    "drag",
    "dragenter",
    "dragover",
    "dragleave",
    "drop",
    "dragend",
    "touchstart",
    "touchmove",
    "touchend",
    "touchcancel",
    "beforeunload",
    "consolemessage",
    "contextmenu",
    "devicechange",
    "devicemotion",
    "deviceorientation",
    "DOMContentLoaded",
    "error",
    "help",
    "load",
    "losecapture",
    "orientationchange",
    "readystatechange",
    "resize",
    "scroll",
    "unload",
    "canplay",
    "canplaythrough",
    "durationchange",
    "emptied",
    "ended",
    "loadeddata",
    "loadedmetadata",
    "pause",
    "play",
    "playing",
    "progress",
    "ratechange",
    "seeked",
    "seeking",
    "stalled",
    "suspend",
    "timeupdate",
    "volumechange",
    "waiting",
    "sourceopen",
    "sourceended",
    "sourceclosed",
    "abort",
    "update",
    "updatestart",
    "updateend",
    "hashchange",
    "pagehide",
    "pageshow",
    "popstate",
    "copy",
    "paste",
    "cut",
    "beforecopy",
    "beforecut",
    "beforepaste",
    "online",
    "offline",
    "message",
    "connect",
    "install",
    "activate",
    "fetch",
    "foreignfetch",
    "messageerror",
    "statechange",
    "updatefound",
    "controllerchange",
    "webkitAnimationStart",
    "oanimationstart",
    "animationstart",
    "webkitAnimationEnd",
    "oanimationend",
    "animationend",
    "webkitAnimationIteration",
    "oanimationiteration",
    "animationiteration",
    "webkitTransitionEnd",
    "otransitionend",
    "transitionend",
    "pointerdown",
    "pointerup",
    "pointercancel",
    "pointermove",
    "pointerover",
    "pointerout",
    "pointerenter",
    "pointerleave",
    "gotpointercapture",
    "lostpointercapture",
    "MSGestureChange",
    "MSGestureEnd",
    "MSGestureHold",
    "MSGestureStart",
    "MSGestureTap",
    "MSGotPointerCapture",
    "MSInertiaStart",
    "MSLostPointerCapture",
    "MSPointerCancel",
    "MSPointerDown",
    "MSPointerEnter",
    "MSPointerHover",
    "MSPointerLeave",
    "MSPointerMove",
    "MSPointerOut",
    "MSPointerOver",
    "MSPointerUp",
    "text",
    "textinput",
    "textInput",
    "compositionstart",
    "compositionupdate",
    "compositionend",
    "beforeinput",
    "exit",
    "loadabort",
    "loadcommit",
    "loadredirect",
    "loadstart",
    "loadstop",
    "responsive",
    "sizechanged",
    "unresponsive",
    "visibilitychange",
    "storage",
    "DOMSubtreeModified",
    "DOMNodeInserted",
    "DOMNodeRemoved",
    "DOMNodeRemovedFromDocument",
    "DOMNodeInsertedIntoDocument",
    "DOMAttrModified",
    "DOMCharacterDataModified",
    "beforeprint",
    "afterprint",
    "beforeinstallprompt",
    "appinstalled",
    "afterblur",
    "beforeblur",
    "cancel",
    "close",
    "dragexit",
    "encrypted",
    "fullscreenchange",
    "invalid",
    "toggle",
    "search",
    "open",
    "show",
];

impl Rule for PreferAddEventListener {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let mut assigns = Vec::new();
        let mut disabled_from = u32::MAX;
        for raw in 0..ast.node_count() as u32 {
            let Some(id) = NodeId::from_raw(raw) else {
                continue;
            };
            if cx.parent(id).is_none() {
                continue;
            }
            match *ast.node(id) {
                Node::Assign { .. } => assigns.push(id),
                Node::Call {
                    callee,
                    optional: false,
                    args,
                    ..
                } if ident_name(ast, callee) == Some("require") => {
                    if let &[arg] = ast.list(args)
                        && self.is_excluded(static_string(ast, arg))
                    {
                        disabled_from = disabled_from.min(cx.span(id).start);
                    }
                }
                Node::ImportDecl { source, .. } => {
                    if let Node::StringLit { value } = *ast.node(source)
                        && self.is_excluded(Some(ast.atom(value)))
                    {
                        disabled_from = disabled_from.min(cx.span(source).start);
                    }
                }
                _ => {}
            }
        }
        assigns.sort_by_key(|&id| cx.span(id).start);
        for &node in &assigns {
            if disabled_from < cx.span(node).end {
                continue;
            }
            check(cx, node, &assigns);
        }
    }
}

impl PreferAddEventListener {
    fn is_excluded(&self, name: Option<&str>) -> bool {
        name.is_some_and(|n| self.excluded.iter().any(|e| e == n))
    }
}

fn check(cx: &mut Ctx<'_, '_>, node: NodeId, assigns: &[NodeId]) {
    let ast = cx.ast();
    let Node::Assign { left, op, right } = *ast.node(node) else {
        return;
    };
    let member = skip_parens(ast, left);
    let Node::Member { obj, prop, .. } = *ast.node(member) else {
        return;
    };
    let obj = skip_parens(ast, obj);
    let method = match *ast.node(prop) {
        Node::Ident { name } | Node::PrivateName { name } => {
            let n = ast.atom(name);
            n.strip_prefix('#').unwrap_or(n)
        }
        _ => return,
    };
    let Some(event) = method.strip_prefix("on") else {
        return;
    };
    if !EVENTS.contains(&event) {
        return;
    }
    let mut replacement = "addEventListener";
    let mut extra = "";
    let mut fix = None;
    let value = skip_parens(ast, right);
    if is_clearing(ast, right) {
        replacement = "removeEventListener";
    } else if event == "beforeunload" && !fixable_before_unload(ast, value) {
        extra = " Use `event.preventDefault(); event.returnValue = 'foo'` to trigger the prompt.";
    } else if event == "message" {
        extra = " Note that there is difference between `SharedWorker#onmessage` and `SharedWorker#addEventListener('message')`.";
    } else if event == "error" {
        extra = " Note that there is difference between `{window,element}.onerror` and `{window,element}.addEventListener('error')`.";
    } else if op == AssignOp::Assign
        && parent_skip_parens(ast, cx.file.parents, node)
            .is_some_and(|p| matches!(cx.node(p), Node::ExprStmt { .. }))
        && !is_value_not_function(ast, value)
        && !would_remove_comments(
            cx,
            cx.span(node),
            &[parenthesized_span(cx, obj), cx.span(value)],
        )
        && !assigns
            .iter()
            .any(|&other| other != node && assigned_same(cx, other, member))
    {
        let mut handler = cx.text(value).to_owned();
        if is_parenthesized(cx, value) {
            handler = format!("({handler})");
        }
        let text = format!(
            "{}.addEventListener('{event}', {handler})",
            member_object_text(cx, obj, node)
        );
        fix = Some(with_edits(
            Fix::replace(cx.span(node), text),
            space_around_keyword(cx, node),
        ));
    }
    cx.report_node(
        prop,
        format!("Prefer `{replacement}` over `{method}`.{extra}"),
    )
    .id("prefer-add-event-listener")
    .fix_opt(fix);
}

fn assigned_same(cx: &Ctx<'_, '_>, other: NodeId, member: NodeId) -> bool {
    match *cx.node(other) {
        Node::Assign { left, .. } => is_same_reference(cx, left, member),
        _ => false,
    }
}

/// `null`, `undefined` or `void x` under TS wrappers.
fn is_clearing(ast: &ts_r_ast::Ast, id: NodeId) -> bool {
    let id = skip_wrappers(ast, id);
    matches!(
        ast.node(id),
        Node::NullLit {}
            | Node::Unary {
                op: UnaryOp::Void,
                ..
            }
    ) || ident_name(ast, id) == Some("undefined")
}

/// A block-bodied function that never returns a value (nested functions aside).
fn fixable_before_unload(ast: &ts_r_ast::Ast, id: NodeId) -> bool {
    let body = match *ast.node(id) {
        Node::Arrow { body, .. } => body,
        Node::FunctionExpr {
            body: Some(body), ..
        } => body,
        _ => return false,
    };
    matches!(ast.node(body), Node::Block { .. }) && !returns_value(ast, body, 0)
}

fn returns_value(ast: &ts_r_ast::Ast, id: NodeId, depth: u32) -> bool {
    if depth > 500 {
        return false;
    }
    match *ast.node(id) {
        Node::Return { arg } => return arg.is_some(),
        Node::FunctionDecl { .. }
        | Node::FunctionExpr { .. }
        | Node::Arrow { .. }
        | Node::MethodDef { .. }
        | Node::PropertyDef { .. }
        | Node::StaticBlock { .. } => return false,
        _ => {}
    }
    let mut found = false;
    ast.for_each_child(id, |c| {
        found = found || returns_value(ast, c, depth + 1);
    });
    found
}
