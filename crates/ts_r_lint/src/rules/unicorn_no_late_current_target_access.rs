//! `unicorn/no-late-current-target-access`: `event.currentTarget` is only set while the event
//! is dispatched. For a parameter named like an event (`e`, `event`, `evt`, `*Event`) that is
//! not known to be something else, `<param>.currentTarget` is reported inside a function
//! nested in the handler, and in the handler itself after an `await`, `yield` or
//! `for await` of the handler has finished, or inside a loop part that runs again after one.

use super::prelude::*;
use super::unicorn_types::Checker;
use super::unicorn_util::skip_wrappers;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-late-current-target-access",
        "Disallow accessing `event.currentTarget` after the synchronous event dispatch has finished.",
        NO_PRESET,
        Category::Correctness,
    ),
    create: create_default::<NoLateCurrentTargetAccess>,
};

#[derive(Debug, Default)]
struct NoLateCurrentTargetAccess;

const REACT_EVENTS: &[&str] = &[
    "AnimationEvent",
    "BaseSyntheticEvent",
    "ChangeEvent",
    "ClipboardEvent",
    "CompositionEvent",
    "DragEvent",
    "FocusEvent",
    "FormEvent",
    "InputEvent",
    "InvalidEvent",
    "KeyboardEvent",
    "MouseEvent",
    "PointerEvent",
    "SubmitEvent",
    "SyntheticEvent",
    "TouchEvent",
    "ToggleEvent",
    "TransitionEvent",
    "UIEvent",
    "WheelEvent",
];

/// The DOM event interfaces, plus React's synthetic event names.
const EVENTS: &[&str] = &[
    "AnimationEvent",
    "AnimationPlaybackEvent",
    "AudioProcessingEvent",
    "BaseSyntheticEvent",
    "BeforeUnloadEvent",
    "BlobEvent",
    "ChangeEvent",
    "ClipboardEvent",
    "CloseEvent",
    "CommandEvent",
    "CompositionEvent",
    "ContentVisibilityAutoStateChangeEvent",
    "CookieChangeEvent",
    "CustomEvent",
    "DeviceMotionEvent",
    "DeviceOrientationEvent",
    "DragEvent",
    "ErrorEvent",
    "Event",
    "ExtendableCookieChangeEvent",
    "ExtendableEvent",
    "ExtendableMessageEvent",
    "FetchEvent",
    "FocusEvent",
    "FontFaceSetLoadEvent",
    "FormDataEvent",
    "FormEvent",
    "GPUUncapturedErrorEvent",
    "GamepadEvent",
    "HashChangeEvent",
    "IDBVersionChangeEvent",
    "InputEvent",
    "InvalidEvent",
    "KeyboardEvent",
    "MIDIConnectionEvent",
    "MIDIMessageEvent",
    "MediaEncryptedEvent",
    "MediaKeyMessageEvent",
    "MediaQueryListEvent",
    "MediaStreamTrackEvent",
    "MessageEvent",
    "MouseEvent",
    "NavigateEvent",
    "NavigationCurrentEntryChangeEvent",
    "NotificationEvent",
    "OfflineAudioCompletionEvent",
    "PageRevealEvent",
    "PageSwapEvent",
    "PageTransitionEvent",
    "PaymentMethodChangeEvent",
    "PaymentRequestUpdateEvent",
    "PictureInPictureEvent",
    "PointerEvent",
    "PopStateEvent",
    "ProgressEvent",
    "PromiseRejectionEvent",
    "PushEvent",
    "PushSubscriptionChangeEvent",
    "RTCDTMFToneChangeEvent",
    "RTCDataChannelEvent",
    "RTCErrorEvent",
    "RTCPeerConnectionIceErrorEvent",
    "RTCPeerConnectionIceEvent",
    "RTCTrackEvent",
    "RTCTransformEvent",
    "SecurityPolicyViolationEvent",
    "SpeechRecognitionErrorEvent",
    "SpeechRecognitionEvent",
    "SpeechSynthesisErrorEvent",
    "SpeechSynthesisEvent",
    "StorageEvent",
    "SubmitEvent",
    "SyntheticEvent",
    "TaskPriorityChangeEvent",
    "TextEvent",
    "ToggleEvent",
    "TouchEvent",
    "TrackEvent",
    "TransitionEvent",
    "UIEvent",
    "WebGLContextEvent",
    "WheelEvent",
];

static EVENT: Checker = Checker {
    target_names: EVENTS,
    target_imports: &[("react", REACT_EVENTS)],
    target_namespace_imports: &[("react", REACT_EVENTS)],
    prefer_definitions: true,
    ..Checker::DEFAULT
};

/// `e`, `event`, `evt` or a lowercase-first identifier ending in `Event`.
fn is_event_name(name: &str) -> bool {
    if matches!(name, "e" | "event" | "evt") {
        return true;
    }
    let Some(head) = name.strip_suffix("Event") else {
        return false;
    };
    let mut chars = head.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase()) && chars.all(|c| c.is_ascii_alphanumeric())
}

fn is_function(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(id),
        Node::FunctionDecl { .. } | Node::FunctionExpr { .. } | Node::Arrow { .. }
    )
}

fn enclosing_function(cx: &Ctx<'_, '_>, node: NodeId) -> Option<NodeId> {
    let mut cur = cx.parent(node)?;
    while !is_function(cx.ast(), cur) {
        cur = cx.parent(cur)?;
    }
    Some(cur)
}

/// The suspension points (`await`, `yield`, `for await`) in `root`, outside nested functions.
fn suspension_points(ast: &Ast, root: NodeId, out: &mut Vec<NodeId>) {
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        match ast.node(id) {
            Node::Await { .. } | Node::Yield { .. } | Node::ForOf { is_await: true, .. } => {
                out.push(id);
            }
            _ => {}
        }
        if id != root && is_function(ast, id) {
            continue;
        }
        ast.for_each_child(id, |c| {
            if !is_function(ast, c) {
                stack.push(c);
            }
        });
    }
}

/// `child` is a part of `loop` that runs on every iteration.
fn is_repeated_part(ast: &Ast, lp: NodeId, child: NodeId) -> bool {
    match *ast.node(lp) {
        Node::For {
            test, update, body, ..
        } => test == Some(child) || update == Some(child) || body == child,
        Node::ForIn { right, .. } | Node::ForOf { right, .. } => child != right,
        Node::While { .. } | Node::DoWhile { .. } => true,
        _ => false,
    }
}

impl Rule for NoLateCurrentTargetAccess {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Member]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Member { obj, prop, .. } = *ast.node(node) else {
            return;
        };
        let object = skip_wrappers(ast, obj);
        if ident_name(ast, prop) != Some("currentTarget")
            || !matches!(ast.node(object), Node::Ident { .. })
        {
            return;
        }
        let Some(name) = ident_name(ast, object).filter(|n| is_event_name(n)) else {
            return;
        };
        let sem = cx.semantic();
        let Some(sym) = sem
            .reference_at(object)
            .and_then(|r| sem.reference(r).symbol())
        else {
            return;
        };
        let Some(decl) = sem
            .symbol(sym)
            .decls
            .iter()
            .find(|d| d.kind == DeclKind::Param)
        else {
            return;
        };
        let Some(handler) = enclosing_function(cx, decl.name) else {
            return;
        };
        if EVENT.is_known_non_target(cx, object) {
            return;
        }
        let nested = enclosing_function(cx, node) != Some(handler);
        if !nested {
            let start = cx.span(node).start;
            let mut points = Vec::new();
            suspension_points(ast, handler, &mut points);
            let suspended = points.iter().any(|&p| cx.span(p).end <= start);
            let in_loop = || {
                let mut child = node;
                while let Some(cur) = cx.parent(child) {
                    if cur == handler {
                        break;
                    }
                    if is_repeated_part(ast, cur, child) {
                        let mut inner = Vec::new();
                        suspension_points(ast, cur, &mut inner);
                        if !inner.is_empty() {
                            return true;
                        }
                    }
                    child = cur;
                }
                false
            };
            if !suspended && !in_loop() {
                return;
            }
        }
        let (id, message) = if nested {
            (
                "in-nested-function",
                format!(
                    "`{name}.currentTarget` is `null` inside this nested function. It is only set during synchronous event dispatch; save it to a variable in the outer function."
                ),
            )
        } else {
            (
                "after-suspension",
                format!(
                    "`{name}.currentTarget` is `null` after the handler suspends. It is only set during synchronous event dispatch; save it to a variable beforehand."
                ),
            )
        };
        cx.report(cx.span(node), message).id(id);
    }
}
