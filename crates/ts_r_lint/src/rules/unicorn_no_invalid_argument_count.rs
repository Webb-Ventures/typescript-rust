//! `unicorn/no-invalid-argument-count`: a call or `new` of a known global API (`Math.max`,
//! `new URL`, `crypto.subtle.digest`, ...) with an argument count the API does not take, and
//! a call of a same-file function with more arguments than it has parameters.
//!
//! Known APIs match the callee's dotted path (plain `.name` members through TS wrappers,
//! from an identifier or `this`): the built-in table matches the path with leading
//! `globalThis`/`global`/`self`/`window` dropped, and only when its root identifier is a
//! global; the `{pattern: count}` option adds patterns (`new X.y`, `*` for any one part)
//! that match the raw path without scope checks, and replaces a built-in entry of the same
//! pattern (still scope-checked). A count is a number, a list of allowed counts, or
//! `{min, max}`. With a spread argument only too many non-spread arguments are reported.
//! Every matching entry is tried in table order (built-ins, then options) and the first
//! one the count breaks is reported, on the callee.
//!
//! Otherwise a call (not `new`, no spread) of a function or arrow expression, or of an
//! identifier with one declaration (a `function` with a body, or a variable initialized with
//! one) that is never reassigned, reports more arguments than its parameters (without a
//! `this` parameter; a rest parameter allows any number).

use super::prelude::*;
use super::unicorn_conditions::unwrap_ts_and_parens;
use super::unicorn_static::is_global_ident;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-invalid-argument-count",
        "Disallow calling functions and constructors with an invalid number of arguments.",
        NO_PRESET,
        Category::Correctness,
    ),
    create: create::<NoInvalidArgumentCount>,
};

/// An expected argument count.
#[derive(Debug, Clone, PartialEq)]
enum C {
    Exact(u32),
    OneOf(&'static [u32]),
    /// `min`, `max` (`u32::MAX` for no limit).
    Range(u32, u32),
}

#[derive(Debug, Clone)]
enum Count {
    Allowed(Vec<u32>),
    Range(u32, u32),
}

impl Count {
    fn from_c(c: &C) -> Count {
        match *c {
            C::Exact(n) => Count::Allowed(vec![n]),
            C::OneOf(list) => Count::Allowed(list.to_vec()),
            C::Range(min, max) => Count::Range(min, max),
        }
    }

    fn maximum(&self) -> u32 {
        match self {
            Count::Allowed(v) => v.iter().copied().max().unwrap_or(0),
            Count::Range(_, max) => *max,
        }
    }

    fn is_invalid(&self, args: u32, spread: bool) -> bool {
        if spread {
            return args > self.maximum();
        }
        match self {
            Count::Allowed(v) => !v.contains(&args),
            Count::Range(min, max) => args < *min || args > *max,
        }
    }

    fn expected_text(&self) -> String {
        match self {
            Count::Allowed(v) => {
                let mut v = v.clone();
                v.sort_unstable();
                let parts: Vec<String> = v.iter().map(|&n| arguments(n)).collect();
                match parts.as_slice() {
                    [one] => one.clone(),
                    [a, b] => format!("{a} or {b}"),
                    [rest @ .., last] => format!("{}, or {last}", rest.join(", ")),
                    [] => String::new(),
                }
            }
            &Count::Range(min, max) => {
                if min == max {
                    arguments(min)
                } else if min == 0 {
                    format!("at most {}", arguments(max))
                } else if max == u32::MAX {
                    format!("at least {}", arguments(min))
                } else {
                    format!("between {min} and {}", arguments(max))
                }
            }
        }
    }
}

fn arguments(n: u32) -> String {
    if n == 1 {
        "1 argument".to_owned()
    } else {
        format!("{n} arguments")
    }
}

#[derive(Debug)]
struct Entry {
    is_new: bool,
    parts: Vec<String>,
    count: Count,
    check_global: bool,
}

#[derive(Debug)]
struct NoInvalidArgumentCount {
    /// Built-ins not overridden, then the options, in order.
    entries: Vec<Entry>,
}

fn parse_count(v: &Value) -> Result<Count, String> {
    let int = |v: &Value| {
        v.as_u64()
            .map(|n| u32::try_from(n).unwrap_or(u32::MAX))
            .ok_or_else(|| "argument counts must be non-negative integers".to_owned())
    };
    match v {
        Value::Number(_) => Ok(Count::Allowed(vec![int(v)?])),
        Value::Array(list) if !list.is_empty() => Ok(Count::Allowed(
            list.iter().map(int).collect::<Result<_, _>>()?,
        )),
        Value::Object(o) if !o.is_empty() => {
            let min = o.get("min").map(int).transpose()?.unwrap_or(0);
            let max = o.get("max").map(int).transpose()?.unwrap_or(u32::MAX);
            if o.keys().any(|k| k != "min" && k != "max") {
                return Err("only `min` and `max` are allowed".to_owned());
            }
            if min > max {
                return Err("`min` must be less than or equal to `max`.".to_owned());
            }
            Ok(Count::Range(min, max))
        }
        _ => Err("invalid argument count".to_owned()),
    }
}

fn split_pattern(pattern: &str) -> (bool, Vec<String>) {
    let (is_new, path) = match pattern.strip_prefix("new ") {
        Some(p) => (true, p),
        None => (false, pattern),
    };
    (is_new, path.split('.').map(str::to_owned).collect())
}

impl FromOptions for NoInvalidArgumentCount {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let custom = obj_opt(opts, 0);
        let mut entries = Vec::new();
        for (pattern, c) in DEFAULTS {
            if custom.contains_key(*pattern) {
                continue;
            }
            let (is_new, parts) = split_pattern(pattern);
            entries.push(Entry {
                is_new,
                parts,
                count: Count::from_c(c),
                check_global: true,
            });
        }
        for (pattern, v) in custom {
            let (is_new, parts) = split_pattern(pattern);
            entries.push(Entry {
                is_new,
                parts,
                count: parse_count(v)?,
                check_global: DEFAULTS.iter().any(|(p, _)| p == pattern),
            });
        }
        Ok(NoInvalidArgumentCount { entries })
    }
}

/// The callee's dotted path and its root identifier (`None` for `this`).
fn callee_path(ast: &Ast, callee: NodeId) -> Option<(Vec<&str>, Option<NodeId>)> {
    let mut parts = Vec::new();
    let mut id = unwrap_ts_and_parens(ast, callee);
    // Each step moves to the member's object: the walk ends.
    loop {
        match *ast.node(id) {
            Node::Member { obj, prop, .. } => {
                let Node::Ident { name } = *ast.node(prop) else {
                    return None;
                };
                parts.push(ast.atom(name));
                id = unwrap_ts_and_parens(ast, obj);
            }
            Node::Ident { name } => {
                parts.push(ast.atom(name));
                parts.reverse();
                return Some((parts, Some(id)));
            }
            Node::This {} => {
                parts.push("this");
                parts.reverse();
                return Some((parts, None));
            }
            _ => return None,
        }
    }
}

fn strip_global_objects<'p, 'a>(parts: &'p [&'a str]) -> &'p [&'a str] {
    let mut start = 0;
    while start + 1 < parts.len()
        && matches!(parts[start], "globalThis" | "global" | "self" | "window")
    {
        start += 1;
    }
    &parts[start..]
}

impl NoInvalidArgumentCount {
    fn configured_problem(
        &self,
        cx: &Ctx<'_, '_>,
        callee: NodeId,
        args: &[NodeId],
        is_new: bool,
    ) -> Option<String> {
        let ast = cx.ast();
        let (parts, root) = callee_path(ast, callee)?;
        let stripped = strip_global_objects(&parts);
        let mut root_is_global: Option<bool> = None;
        let spread = args
            .iter()
            .any(|&a| matches!(ast.node(a), Node::Spread { .. }));
        let count = args
            .iter()
            .filter(|&&a| !matches!(ast.node(a), Node::Spread { .. }))
            .count();
        let count = u32::try_from(count).unwrap_or(u32::MAX);
        for e in self.entries.iter().filter(|e| e.is_new == is_new) {
            let matches = if e.check_global {
                let Some(root) = root else {
                    continue;
                };
                e.parts.len() == stripped.len()
                    && e.parts.iter().zip(stripped).all(|(a, b)| a == b)
                    && *root_is_global.get_or_insert_with(|| is_global_ident(cx, root, parts[0]))
            } else {
                e.parts.len() == parts.len()
                    && e.parts.iter().zip(&parts).all(|(a, b)| a == "*" || a == b)
            };
            if matches && e.count.is_invalid(count, spread) {
                let actual = if spread {
                    format!("at least {}", arguments(count))
                } else {
                    arguments(count)
                };
                return Some(format!(
                    "Expected {}, but got {actual}.",
                    e.count.expected_text()
                ));
            }
        }
        None
    }
}

fn function_node(cx: &Ctx<'_, '_>, callee: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let callee = unwrap_ts_and_parens(ast, callee);
    match ast.node(callee) {
        Node::FunctionExpr { .. } | Node::Arrow { .. } => return Some(callee),
        Node::Ident { .. } => {}
        _ => return None,
    }
    let sem = cx.semantic();
    let s = sem.symbol_of_ident(callee)?;
    let sym = sem.symbol(s);
    let [decl] = sym.decls.as_slice() else {
        return None;
    };
    if sym.refs.iter().any(|&r| {
        let r = sem.reference(r);
        r.is_write() && !r.flags.contains(RefFlags::INIT)
    }) {
        return None;
    }
    match *ast.node(decl.node) {
        Node::FunctionDecl { body: Some(_), .. } => Some(decl.node),
        Node::VarDeclarator {
            name,
            init: Some(init),
            ..
        } if name == decl.name => {
            let init = unwrap_ts_and_parens(ast, init);
            matches!(
                ast.node(init),
                Node::FunctionExpr { .. } | Node::Arrow { .. }
            )
            .then_some(init)
        }
        _ => None,
    }
}

/// `(minimum, maximum)` arguments, `None` for a rest parameter.
fn arity(ast: &Ast, function: NodeId) -> (u32, Option<u32>) {
    let params = match *ast.node(function) {
        Node::FunctionDecl { params, .. }
        | Node::FunctionExpr { params, .. }
        | Node::Arrow { params, .. } => ast.list(params),
        _ => &[],
    };
    let params = match params.first().map(|&p| ast.node(p)) {
        Some(&Node::Param { pattern, .. }) if ident_name(ast, pattern) == Some("this") => {
            &params[1..]
        }
        _ => params,
    };
    let len = u32::try_from(params.len()).unwrap_or(u32::MAX);
    let mut minimum = len;
    let mut rest = false;
    for &p in params.iter().rev() {
        let Node::Param {
            rest: is_rest,
            optional,
            init,
            ..
        } = *ast.node(p)
        else {
            break;
        };
        rest |= is_rest;
        if !(is_rest || optional || init.is_some()) {
            break;
        }
        minimum -= 1;
    }
    (minimum, (!rest).then_some(len))
}

impl Rule for NoInvalidArgumentCount {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call, NodeKind::New]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let (callee, args, is_new) = match *cx.node(node) {
            Node::Call { callee, args, .. } => (callee, ast.list(args), false),
            Node::New { callee, args, .. } => (callee, ast.list(args), true),
            _ => return,
        };
        let reported = skip_parens(ast, callee);
        if let Some(msg) = self.configured_problem(cx, callee, args, is_new) {
            cx.report_node(reported, msg)
                .id("no-invalid-argument-count");
            return;
        }
        if is_new
            || args
                .iter()
                .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
        {
            return;
        }
        let Some(function) = function_node(cx, callee) else {
            return;
        };
        let (minimum, Some(maximum)) = arity(ast, function) else {
            return;
        };
        let count = u32::try_from(args.len()).unwrap_or(u32::MAX);
        if count <= maximum {
            return;
        }
        let expected = if minimum == maximum {
            arguments(maximum)
        } else {
            format!("at most {}", arguments(maximum))
        };
        cx.report_node(
            reported,
            format!("Expected {expected}, but got {}.", arguments(count)),
        )
        .id("no-invalid-argument-count");
    }
}

/// The API arities eslint-plugin-unicorn's rule ships, as data.
const DEFAULTS: &[(&str, C)] = &[
    ("AggregateError", C::Range(1, 3)),
    ("BigInt", C::Exact(1)),
    ("Boolean", C::Range(0, 1)),
    ("Date", C::Exact(0)),
    ("Error", C::Range(0, 3)),
    ("EvalError", C::Range(0, 3)),
    ("Number", C::Range(0, 1)),
    ("RangeError", C::Range(0, 3)),
    ("ReferenceError", C::Range(0, 3)),
    ("RegExp", C::Range(0, 2)),
    ("String", C::Range(0, 1)),
    ("Symbol", C::Range(0, 1)),
    ("SyntaxError", C::Range(0, 3)),
    ("TypeError", C::Range(0, 3)),
    ("URIError", C::Range(0, 3)),
    ("addEventListener", C::Range(2, 3)),
    ("decodeURI", C::Exact(1)),
    ("decodeURIComponent", C::Exact(1)),
    ("encodeURI", C::Exact(1)),
    ("encodeURIComponent", C::Exact(1)),
    ("escape", C::Exact(1)),
    ("eval", C::Range(0, 1)),
    ("fetch", C::Range(1, 2)),
    ("isFinite", C::Exact(1)),
    ("isNaN", C::Exact(1)),
    ("parseFloat", C::Exact(1)),
    ("parseInt", C::Range(1, 2)),
    ("queueMicrotask", C::Exact(1)),
    ("structuredClone", C::Range(1, 2)),
    ("SuppressedError", C::Range(2, 3)),
    ("atob", C::Exact(1)),
    ("blur", C::Exact(0)),
    ("btoa", C::Exact(1)),
    ("clearImmediate", C::Exact(1)),
    ("close", C::Exact(0)),
    ("dispatchEvent", C::Exact(1)),
    ("focus", C::Exact(0)),
    ("moveBy", C::Exact(2)),
    ("moveTo", C::Exact(2)),
    ("open", C::Range(0, 3)),
    ("postMessage", C::Range(1, 3)),
    ("print", C::Exact(0)),
    ("removeEventListener", C::Range(2, 3)),
    ("resizeBy", C::Exact(2)),
    ("resizeTo", C::Exact(2)),
    ("scroll", C::Range(0, 2)),
    ("scrollBy", C::Range(0, 2)),
    ("scrollTo", C::Range(0, 2)),
    ("setImmediate", C::Range(1, u32::MAX)),
    ("stop", C::Exact(0)),
    ("unescape", C::Exact(1)),
    ("alert", C::Range(0, 1)),
    ("cancelAnimationFrame", C::Exact(1)),
    ("cancelIdleCallback", C::Exact(1)),
    ("clearInterval", C::Range(0, 1)),
    ("clearTimeout", C::Range(0, 1)),
    ("confirm", C::Range(0, 1)),
    ("createImageBitmap", C::OneOf(&[1, 2, 5, 6])),
    ("matchMedia", C::Exact(1)),
    ("prompt", C::Range(0, 2)),
    ("reportError", C::Exact(1)),
    ("requestAnimationFrame", C::Exact(1)),
    ("requestIdleCallback", C::Range(1, 2)),
    ("setInterval", C::Range(1, u32::MAX)),
    ("setTimeout", C::Range(1, u32::MAX)),
    ("Buffer.alloc", C::Range(1, 3)),
    ("Buffer.allocUnsafe", C::Exact(1)),
    ("Buffer.allocUnsafeSlow", C::Exact(1)),
    ("Buffer.byteLength", C::Range(1, 2)),
    ("Buffer.compare", C::Exact(2)),
    ("Buffer.concat", C::Range(1, 2)),
    ("Buffer.copyBytesFrom", C::Range(1, 3)),
    ("Buffer.from", C::Range(1, 3)),
    ("Buffer.isBuffer", C::Exact(1)),
    ("Buffer.isEncoding", C::Exact(1)),
    ("AbortSignal.abort", C::Range(0, 1)),
    ("AbortSignal.any", C::Exact(1)),
    ("AbortSignal.timeout", C::Exact(1)),
    ("AudioDecoder.isConfigSupported", C::Exact(1)),
    ("AudioEncoder.isConfigSupported", C::Exact(1)),
    ("caches.delete", C::Exact(1)),
    ("caches.has", C::Exact(1)),
    ("caches.keys", C::Exact(0)),
    ("caches.match", C::Range(1, 2)),
    ("caches.open", C::Exact(1)),
    ("crypto.getRandomValues", C::Exact(1)),
    ("crypto.randomUUID", C::Exact(0)),
    ("crypto.subtle.decrypt", C::Exact(3)),
    ("crypto.subtle.deriveBits", C::Range(2, 3)),
    ("crypto.subtle.deriveKey", C::Exact(5)),
    ("crypto.subtle.digest", C::Exact(2)),
    ("crypto.subtle.encrypt", C::Exact(3)),
    ("crypto.subtle.exportKey", C::Exact(2)),
    ("crypto.subtle.generateKey", C::Exact(3)),
    ("crypto.subtle.importKey", C::Exact(5)),
    ("crypto.subtle.sign", C::Exact(3)),
    ("crypto.subtle.unwrapKey", C::Exact(7)),
    ("crypto.subtle.verify", C::Exact(4)),
    ("crypto.subtle.wrapKey", C::Exact(4)),
    ("CSS.escape", C::Exact(1)),
    ("CSSNumericValue.parse", C::Exact(1)),
    ("CSS.registerProperty", C::Exact(1)),
    ("CSSStyleValue.parse", C::Exact(2)),
    ("CSSStyleValue.parseAll", C::Exact(2)),
    ("CSS.supports", C::OneOf(&[1, 2])),
    ("console.clear", C::Exact(0)),
    ("console.count", C::Range(0, 1)),
    ("console.countReset", C::Range(0, 1)),
    ("console.dir", C::Range(0, 2)),
    ("console.groupEnd", C::Exact(0)),
    ("console.table", C::Range(0, 2)),
    ("console.time", C::Range(0, 1)),
    ("console.timeEnd", C::Range(0, 1)),
    ("console.timeStamp", C::Range(0, 1)),
    ("customElements.define", C::Range(2, 3)),
    ("customElements.get", C::Exact(1)),
    ("customElements.getName", C::Exact(1)),
    ("customElements.upgrade", C::Exact(1)),
    ("customElements.whenDefined", C::Exact(1)),
    ("document.adoptNode", C::Exact(1)),
    ("document.captureEvents", C::Exact(0)),
    ("document.caretPositionFromPoint", C::Range(2, 3)),
    ("document.caretRangeFromPoint", C::Exact(2)),
    ("document.clear", C::Exact(0)),
    ("document.close", C::Exact(0)),
    ("document.createAttribute", C::Exact(1)),
    ("document.createAttributeNS", C::Exact(2)),
    ("document.createCDATASection", C::Exact(1)),
    ("document.createComment", C::Exact(1)),
    ("document.createDocumentFragment", C::Exact(0)),
    ("document.createElement", C::Range(1, 2)),
    ("document.createElementNS", C::Range(2, 3)),
    ("document.createEvent", C::Exact(1)),
    ("document.createNodeIterator", C::Range(1, 3)),
    ("document.createNSResolver", C::Exact(1)),
    ("document.createProcessingInstruction", C::Exact(2)),
    ("document.createRange", C::Exact(0)),
    ("document.createTextNode", C::Exact(1)),
    ("document.createTreeWalker", C::Range(1, 3)),
    ("document.elementFromPoint", C::Exact(2)),
    ("document.elementsFromPoint", C::Exact(2)),
    ("document.execCommand", C::Range(1, 3)),
    ("document.exitFullscreen", C::Exact(0)),
    ("document.exitPictureInPicture", C::Exact(0)),
    ("document.exitPointerLock", C::Exact(0)),
    ("document.getElementById", C::Exact(1)),
    ("document.getElementsByClassName", C::Exact(1)),
    ("document.getElementsByName", C::Exact(1)),
    ("document.getElementsByTagName", C::Exact(1)),
    ("document.getElementsByTagNameNS", C::Exact(2)),
    ("document.getSelection", C::Exact(0)),
    ("document.hasFocus", C::Exact(0)),
    ("document.hasStorageAccess", C::Exact(0)),
    ("document.importNode", C::Range(1, 2)),
    ("document.open", C::Range(0, 3)),
    ("document.queryCommandEnabled", C::Exact(1)),
    ("document.queryCommandIndeterm", C::Exact(1)),
    ("document.queryCommandState", C::Exact(1)),
    ("document.queryCommandSupported", C::Exact(1)),
    ("document.queryCommandValue", C::Exact(1)),
    ("document.querySelector", C::Exact(1)),
    ("document.querySelectorAll", C::Exact(1)),
    ("document.releaseEvents", C::Exact(0)),
    ("document.requestStorageAccess", C::Exact(0)),
    ("document.startViewTransition", C::Range(0, 1)),
    ("getSelection", C::Exact(0)),
    ("cookieStore.delete", C::Exact(1)),
    ("cookieStore.get", C::Range(0, 1)),
    ("cookieStore.getAll", C::Range(0, 1)),
    ("cookieStore.set", C::OneOf(&[1, 2])),
    ("history.back", C::Exact(0)),
    ("history.forward", C::Exact(0)),
    ("history.go", C::Range(0, 1)),
    ("history.pushState", C::Range(2, 3)),
    ("history.replaceState", C::Range(2, 3)),
    ("ImageDecoder.isTypeSupported", C::Exact(1)),
    ("IDBKeyRange.bound", C::Range(2, 4)),
    ("IDBKeyRange.lowerBound", C::Range(1, 2)),
    ("IDBKeyRange.only", C::Exact(1)),
    ("IDBKeyRange.upperBound", C::Range(1, 2)),
    ("indexedDB.cmp", C::Exact(2)),
    ("indexedDB.databases", C::Exact(0)),
    ("indexedDB.deleteDatabase", C::Range(1, 2)),
    ("indexedDB.open", C::Range(1, 2)),
    ("location.assign", C::Exact(1)),
    ("location.reload", C::Exact(0)),
    ("location.replace", C::Exact(1)),
    ("MediaRecorder.isTypeSupported", C::Exact(1)),
    ("MediaSource.isTypeSupported", C::Exact(1)),
    ("navigation.back", C::Range(0, 1)),
    ("navigation.entries", C::Exact(0)),
    ("navigation.forward", C::Range(0, 1)),
    ("navigation.navigate", C::Range(1, 2)),
    ("navigation.reload", C::Range(0, 1)),
    ("navigation.traverseTo", C::Range(1, 2)),
    ("navigation.updateCurrentEntry", C::Exact(1)),
    ("navigator.canShare", C::Range(0, 1)),
    ("navigator.clearAppBadge", C::Exact(0)),
    ("navigator.clipboard.read", C::Range(0, 1)),
    ("navigator.clipboard.readText", C::Exact(0)),
    ("navigator.clipboard.write", C::Exact(1)),
    ("navigator.clipboard.writeText", C::Exact(1)),
    ("navigator.credentials.create", C::Range(0, 1)),
    ("navigator.credentials.get", C::Range(0, 1)),
    ("navigator.credentials.preventSilentAccess", C::Exact(0)),
    ("navigator.credentials.store", C::Exact(1)),
    ("navigator.geolocation.clearWatch", C::Exact(1)),
    ("navigator.geolocation.getCurrentPosition", C::Range(1, 3)),
    ("navigator.geolocation.watchPosition", C::Range(1, 3)),
    ("navigator.getGamepads", C::Exact(0)),
    ("navigator.gpu.getPreferredCanvasFormat", C::Exact(0)),
    ("navigator.gpu.requestAdapter", C::Range(0, 1)),
    ("navigator.javaEnabled", C::Exact(0)),
    ("navigator.locks.query", C::Exact(0)),
    ("navigator.mediaSession.setActionHandler", C::Exact(2)),
    ("navigator.mediaSession.setCameraActive", C::Exact(1)),
    ("navigator.mediaSession.setMicrophoneActive", C::Exact(1)),
    ("navigator.mediaSession.setPositionState", C::Range(0, 1)),
    ("navigator.mediaSession.setScreenshareActive", C::Exact(1)),
    ("navigator.locks.request", C::OneOf(&[2, 3])),
    ("navigator.mediaDevices.enumerateDevices", C::Exact(0)),
    ("navigator.mediaDevices.getDisplayMedia", C::Range(0, 1)),
    ("navigator.mediaDevices.getUserMedia", C::Exact(1)),
    ("navigator.permissions.query", C::Exact(1)),
    ("navigator.registerProtocolHandler", C::Exact(2)),
    ("navigator.requestMediaKeySystemAccess", C::Exact(2)),
    ("navigator.requestMIDIAccess", C::Range(0, 1)),
    ("navigator.scheduling.isInputPending", C::Range(0, 1)),
    ("navigator.serviceWorker.getRegistration", C::Range(0, 1)),
    ("navigator.serviceWorker.getRegistrations", C::Exact(0)),
    ("navigator.serviceWorker.register", C::Range(1, 2)),
    ("navigator.setAppBadge", C::Range(0, 1)),
    ("navigator.share", C::Range(0, 1)),
    ("navigator.storage.estimate", C::Exact(0)),
    ("navigator.storage.getDirectory", C::Exact(0)),
    ("navigator.storage.persist", C::Exact(0)),
    ("navigator.storage.persisted", C::Exact(0)),
    ("navigator.vibrate", C::Exact(1)),
    ("navigator.wakeLock.request", C::Range(0, 1)),
    ("Notification.requestPermission", C::Range(0, 1)),
    ("PublicKeyCredential.getClientCapabilities", C::Exact(0)),
    (
        "PublicKeyCredential.isConditionalMediationAvailable",
        C::Exact(0),
    ),
    (
        "PublicKeyCredential.isUserVerifyingPlatformAuthenticatorAvailable",
        C::Exact(0),
    ),
    (
        "PublicKeyCredential.parseCreationOptionsFromJSON",
        C::Exact(1),
    ),
    (
        "PublicKeyCredential.parseRequestOptionsFromJSON",
        C::Exact(1),
    ),
    (
        "PublicKeyCredential.signalAllAcceptedCredentials",
        C::Exact(1),
    ),
    ("PublicKeyCredential.signalCurrentUserDetails", C::Exact(1)),
    ("PublicKeyCredential.signalUnknownCredential", C::Exact(1)),
    ("trustedTypes.createPolicy", C::Range(1, 2)),
    ("trustedTypes.getAttributeType", C::Range(2, 4)),
    ("trustedTypes.getPropertyType", C::Range(2, 3)),
    ("trustedTypes.isHTML", C::Exact(1)),
    ("trustedTypes.isScript", C::Exact(1)),
    ("trustedTypes.isScriptURL", C::Exact(1)),
    ("Array.from", C::Range(1, 3)),
    ("Array.fromAsync", C::Range(1, 3)),
    ("Array.isArray", C::Exact(1)),
    ("ArrayBuffer.isView", C::Exact(1)),
    ("Atomics.add", C::Exact(3)),
    ("Atomics.and", C::Exact(3)),
    ("Atomics.compareExchange", C::Exact(4)),
    ("Atomics.exchange", C::Exact(3)),
    ("Atomics.isLockFree", C::Exact(1)),
    ("Atomics.load", C::Exact(2)),
    ("Atomics.notify", C::Range(2, 3)),
    ("Atomics.or", C::Exact(3)),
    ("Atomics.pause", C::Exact(0)),
    ("Atomics.store", C::Exact(3)),
    ("Atomics.sub", C::Exact(3)),
    ("Atomics.wait", C::Range(3, 4)),
    ("Atomics.waitAsync", C::Range(3, 4)),
    ("Atomics.xor", C::Exact(3)),
    ("BigInt.asIntN", C::Exact(2)),
    ("BigInt.asUintN", C::Exact(2)),
    ("Date.now", C::Exact(0)),
    ("Date.parse", C::Exact(1)),
    ("Date.UTC", C::Range(1, 7)),
    ("Intl.Collator", C::Range(0, 2)),
    ("Intl.DateTimeFormat", C::Range(0, 2)),
    ("Intl.Collator.supportedLocalesOf", C::Range(0, 2)),
    ("Intl.DateTimeFormat.supportedLocalesOf", C::Range(0, 2)),
    ("Intl.DisplayNames.supportedLocalesOf", C::Range(0, 2)),
    ("Intl.DurationFormat.supportedLocalesOf", C::Range(0, 2)),
    ("Intl.ListFormat.supportedLocalesOf", C::Range(0, 2)),
    ("Intl.NumberFormat.supportedLocalesOf", C::Range(0, 2)),
    ("Intl.PluralRules.supportedLocalesOf", C::Range(0, 2)),
    ("Intl.RelativeTimeFormat.supportedLocalesOf", C::Range(0, 2)),
    ("Intl.Segmenter.supportedLocalesOf", C::Range(0, 2)),
    ("Intl.getCanonicalLocales", C::Range(0, 1)),
    ("Intl.NumberFormat", C::Range(0, 2)),
    ("Intl.supportedValuesOf", C::Exact(1)),
    ("Iterator.from", C::Exact(1)),
    ("Iterator.zip", C::Range(1, 2)),
    ("Iterator.zipKeyed", C::Range(1, 2)),
    ("JSON.isRawJSON", C::Exact(1)),
    ("JSON.parse", C::Range(1, 2)),
    ("JSON.rawJSON", C::Exact(1)),
    ("JSON.stringify", C::Range(1, 3)),
    ("Map.groupBy", C::Exact(2)),
    ("Math.abs", C::Exact(1)),
    ("Math.acos", C::Exact(1)),
    ("Math.acosh", C::Exact(1)),
    ("Math.asin", C::Exact(1)),
    ("Math.asinh", C::Exact(1)),
    ("Math.atan", C::Exact(1)),
    ("Math.atan2", C::Exact(2)),
    ("Math.atanh", C::Exact(1)),
    ("Math.cbrt", C::Exact(1)),
    ("Math.ceil", C::Exact(1)),
    ("Math.clz32", C::Exact(1)),
    ("Math.cos", C::Exact(1)),
    ("Math.cosh", C::Exact(1)),
    ("Math.exp", C::Exact(1)),
    ("Math.expm1", C::Exact(1)),
    ("Math.floor", C::Exact(1)),
    ("Math.f16round", C::Exact(1)),
    ("Math.fround", C::Exact(1)),
    ("Math.imul", C::Exact(2)),
    ("Math.log", C::Exact(1)),
    ("Math.log1p", C::Exact(1)),
    ("Math.log10", C::Exact(1)),
    ("Math.log2", C::Exact(1)),
    ("Math.pow", C::Exact(2)),
    ("Math.random", C::Exact(0)),
    ("Math.round", C::Exact(1)),
    ("Math.sign", C::Exact(1)),
    ("Math.sin", C::Exact(1)),
    ("Math.sinh", C::Exact(1)),
    ("Math.sqrt", C::Exact(1)),
    ("Math.sumPrecise", C::Exact(1)),
    ("Math.tan", C::Exact(1)),
    ("Math.tanh", C::Exact(1)),
    ("Math.trunc", C::Exact(1)),
    ("navigator.sendBeacon", C::Range(1, 2)),
    ("Number.isFinite", C::Exact(1)),
    ("Number.isInteger", C::Exact(1)),
    ("Number.isNaN", C::Exact(1)),
    ("Number.isSafeInteger", C::Exact(1)),
    ("Number.parseFloat", C::Exact(1)),
    ("Number.parseInt", C::Range(1, 2)),
    ("Object.assign", C::Range(1, u32::MAX)),
    ("Object.create", C::Range(1, 2)),
    ("Object.defineProperties", C::Exact(2)),
    ("Object.defineProperty", C::Exact(3)),
    ("Object.entries", C::Exact(1)),
    ("Object.freeze", C::Exact(1)),
    ("Object.fromEntries", C::Exact(1)),
    ("Object.getOwnPropertyDescriptor", C::Exact(2)),
    ("Object.getOwnPropertyDescriptors", C::Exact(1)),
    ("Object.getOwnPropertyNames", C::Exact(1)),
    ("Object.getOwnPropertySymbols", C::Exact(1)),
    ("Object.getPrototypeOf", C::Exact(1)),
    ("Object.groupBy", C::Exact(2)),
    ("Object.hasOwn", C::Exact(2)),
    ("Object.is", C::Exact(2)),
    ("Object.isExtensible", C::Exact(1)),
    ("Object.isFrozen", C::Exact(1)),
    ("Object.isSealed", C::Exact(1)),
    ("Object.keys", C::Exact(1)),
    ("Object.preventExtensions", C::Exact(1)),
    ("Object.seal", C::Exact(1)),
    ("Object.setPrototypeOf", C::Exact(2)),
    ("Object.values", C::Exact(1)),
    ("performance.clearMarks", C::Range(0, 1)),
    ("performance.clearMeasures", C::Range(0, 1)),
    ("performance.clearResourceTimings", C::Exact(0)),
    ("performance.getEntries", C::Exact(0)),
    ("performance.getEntriesByName", C::Range(1, 2)),
    ("performance.getEntriesByType", C::Exact(1)),
    ("performance.mark", C::Range(1, 2)),
    ("performance.measure", C::Range(1, 3)),
    ("performance.now", C::Exact(0)),
    ("performance.setResourceTimingBufferSize", C::Exact(1)),
    ("performance.toJSON", C::Exact(0)),
    ("Promise.all", C::Exact(1)),
    ("Promise.allSettled", C::Exact(1)),
    ("Promise.any", C::Exact(1)),
    ("Promise.race", C::Exact(1)),
    ("Promise.reject", C::Range(0, 1)),
    ("Promise.resolve", C::Range(0, 1)),
    ("Promise.try", C::Range(1, u32::MAX)),
    ("Promise.withResolvers", C::Exact(0)),
    ("process.abort", C::Exact(0)),
    ("process.availableMemory", C::Exact(0)),
    ("process.chdir", C::Exact(1)),
    ("process.constrainedMemory", C::Exact(0)),
    ("process.cpuUsage", C::Range(0, 1)),
    ("process.cwd", C::Exact(0)),
    ("process.disconnect", C::Exact(0)),
    ("process.dlopen", C::Range(2, 3)),
    ("process.emitWarning", C::Range(1, 4)),
    ("process.execve", C::Range(1, 3)),
    ("process.exit", C::Range(0, 1)),
    ("process.finalization.register", C::Exact(2)),
    ("process.finalization.registerBeforeExit", C::Exact(2)),
    ("process.finalization.unregister", C::Exact(1)),
    ("process.getActiveResourcesInfo", C::Exact(0)),
    ("process.getBuiltinModule", C::Exact(1)),
    ("process.getegid", C::Exact(0)),
    ("process.geteuid", C::Exact(0)),
    ("process.getgid", C::Exact(0)),
    ("process.getgroups", C::Exact(0)),
    ("process.getuid", C::Exact(0)),
    ("process.hasUncaughtExceptionCaptureCallback", C::Exact(0)),
    ("process.hrtime", C::Range(0, 1)),
    ("process.hrtime.bigint", C::Exact(0)),
    ("process.initgroups", C::Exact(2)),
    ("process.kill", C::Range(1, 2)),
    ("process.loadEnvFile", C::Range(0, 1)),
    ("process.memoryUsage", C::Exact(0)),
    ("process.memoryUsage.rss", C::Exact(0)),
    ("process.nextTick", C::Range(1, u32::MAX)),
    ("process.permission.has", C::Range(1, 2)),
    ("process.ref", C::Exact(1)),
    ("process.report.getReport", C::Range(0, 1)),
    ("process.report.writeReport", C::Range(0, 2)),
    ("process.resourceUsage", C::Exact(0)),
    ("process.send", C::Range(1, 4)),
    ("process.setegid", C::Exact(1)),
    ("process.seteuid", C::Exact(1)),
    ("process.setgid", C::Exact(1)),
    ("process.setgroups", C::Exact(1)),
    ("process.setSourceMapsEnabled", C::Exact(1)),
    ("process.setuid", C::Exact(1)),
    ("process.setUncaughtExceptionCaptureCallback", C::Exact(1)),
    ("process.threadCpuUsage", C::Range(0, 1)),
    ("process.umask", C::Range(0, 1)),
    ("process.unref", C::Exact(1)),
    ("process.uptime", C::Exact(0)),
    ("Reflect.apply", C::Exact(3)),
    ("Reflect.construct", C::Range(2, 3)),
    ("Reflect.defineProperty", C::Exact(3)),
    ("Reflect.deleteProperty", C::Exact(2)),
    ("Reflect.get", C::Range(2, 3)),
    ("Reflect.getOwnPropertyDescriptor", C::Exact(2)),
    ("Reflect.getPrototypeOf", C::Exact(1)),
    ("Reflect.has", C::Exact(2)),
    ("Reflect.isExtensible", C::Exact(1)),
    ("Reflect.ownKeys", C::Exact(1)),
    ("Reflect.preventExtensions", C::Exact(1)),
    ("Reflect.set", C::Range(3, 4)),
    ("Reflect.setPrototypeOf", C::Exact(2)),
    ("RegExp.escape", C::Exact(1)),
    ("scheduler.postTask", C::Range(1, 2)),
    ("scheduler.yield", C::Exact(0)),
    ("screen.orientation.lock", C::Exact(1)),
    ("screen.orientation.unlock", C::Exact(0)),
    ("speechSynthesis.cancel", C::Exact(0)),
    ("speechSynthesis.getVoices", C::Exact(0)),
    ("speechSynthesis.pause", C::Exact(0)),
    ("speechSynthesis.resume", C::Exact(0)),
    ("speechSynthesis.speak", C::Exact(1)),
    ("Symbol.for", C::Exact(1)),
    ("Symbol.keyFor", C::Exact(1)),
    ("String.raw", C::Range(1, u32::MAX)),
    ("Response.error", C::Exact(0)),
    ("Response.json", C::Range(1, 2)),
    ("Response.redirect", C::Range(1, 2)),
    ("Temporal.Instant.fromEpochMilliseconds", C::Exact(1)),
    ("Temporal.Instant.fromEpochNanoseconds", C::Exact(1)),
    ("Temporal.Now.instant", C::Exact(0)),
    ("Temporal.Now.plainDateISO", C::Range(0, 1)),
    ("Temporal.Now.plainDateTimeISO", C::Range(0, 1)),
    ("Temporal.Now.plainTimeISO", C::Range(0, 1)),
    ("Temporal.Now.timeZoneId", C::Exact(0)),
    ("Temporal.Now.zonedDateTimeISO", C::Range(0, 1)),
    ("URL.canParse", C::Range(1, 2)),
    ("URL.createObjectURL", C::Exact(1)),
    ("URL.parse", C::Range(1, 2)),
    ("URL.revokeObjectURL", C::Exact(1)),
    ("VideoDecoder.isConfigSupported", C::Exact(1)),
    ("VideoEncoder.isConfigSupported", C::Exact(1)),
    ("WebAssembly.compile", C::Range(1, 2)),
    ("WebAssembly.compileStreaming", C::Range(1, 2)),
    ("WebAssembly.instantiate", C::Range(1, 3)),
    ("WebAssembly.instantiateStreaming", C::Range(1, 3)),
    ("WebAssembly.Module.customSections", C::Exact(2)),
    ("WebAssembly.Module.exports", C::Exact(1)),
    ("WebAssembly.Module.imports", C::Exact(1)),
    ("WebAssembly.validate", C::Range(1, 2)),
    ("CSS.number", C::Exact(1)),
    ("CSS.percent", C::Exact(1)),
    ("CSS.cap", C::Exact(1)),
    ("CSS.ch", C::Exact(1)),
    ("CSS.em", C::Exact(1)),
    ("CSS.ex", C::Exact(1)),
    ("CSS.ic", C::Exact(1)),
    ("CSS.lh", C::Exact(1)),
    ("CSS.rcap", C::Exact(1)),
    ("CSS.rch", C::Exact(1)),
    ("CSS.rem", C::Exact(1)),
    ("CSS.rex", C::Exact(1)),
    ("CSS.ric", C::Exact(1)),
    ("CSS.rlh", C::Exact(1)),
    ("CSS.vw", C::Exact(1)),
    ("CSS.vh", C::Exact(1)),
    ("CSS.vi", C::Exact(1)),
    ("CSS.vb", C::Exact(1)),
    ("CSS.vmin", C::Exact(1)),
    ("CSS.vmax", C::Exact(1)),
    ("CSS.svw", C::Exact(1)),
    ("CSS.svh", C::Exact(1)),
    ("CSS.svi", C::Exact(1)),
    ("CSS.svb", C::Exact(1)),
    ("CSS.svmin", C::Exact(1)),
    ("CSS.svmax", C::Exact(1)),
    ("CSS.lvw", C::Exact(1)),
    ("CSS.lvh", C::Exact(1)),
    ("CSS.lvi", C::Exact(1)),
    ("CSS.lvb", C::Exact(1)),
    ("CSS.lvmin", C::Exact(1)),
    ("CSS.lvmax", C::Exact(1)),
    ("CSS.dvw", C::Exact(1)),
    ("CSS.dvh", C::Exact(1)),
    ("CSS.dvi", C::Exact(1)),
    ("CSS.dvb", C::Exact(1)),
    ("CSS.dvmin", C::Exact(1)),
    ("CSS.dvmax", C::Exact(1)),
    ("CSS.cqw", C::Exact(1)),
    ("CSS.cqh", C::Exact(1)),
    ("CSS.cqi", C::Exact(1)),
    ("CSS.cqb", C::Exact(1)),
    ("CSS.cqmin", C::Exact(1)),
    ("CSS.cqmax", C::Exact(1)),
    ("CSS.cm", C::Exact(1)),
    ("CSS.mm", C::Exact(1)),
    ("CSS.Q", C::Exact(1)),
    ("CSS.in", C::Exact(1)),
    ("CSS.pt", C::Exact(1)),
    ("CSS.pc", C::Exact(1)),
    ("CSS.px", C::Exact(1)),
    ("CSS.deg", C::Exact(1)),
    ("CSS.grad", C::Exact(1)),
    ("CSS.rad", C::Exact(1)),
    ("CSS.turn", C::Exact(1)),
    ("CSS.s", C::Exact(1)),
    ("CSS.ms", C::Exact(1)),
    ("CSS.Hz", C::Exact(1)),
    ("CSS.kHz", C::Exact(1)),
    ("CSS.dpi", C::Exact(1)),
    ("CSS.dpcm", C::Exact(1)),
    ("CSS.dppx", C::Exact(1)),
    ("CSS.fr", C::Exact(1)),
    ("DOMPoint.fromPoint", C::Range(0, 1)),
    ("DOMPointReadOnly.fromPoint", C::Range(0, 1)),
    ("DOMMatrix.fromFloat32Array", C::Exact(1)),
    ("DOMMatrix.fromFloat64Array", C::Exact(1)),
    ("DOMMatrix.fromMatrix", C::Range(0, 1)),
    ("DOMMatrixReadOnly.fromFloat32Array", C::Exact(1)),
    ("DOMMatrixReadOnly.fromFloat64Array", C::Exact(1)),
    ("DOMMatrixReadOnly.fromMatrix", C::Range(0, 1)),
    ("DOMRect.fromRect", C::Range(0, 1)),
    ("DOMRectReadOnly.fromRect", C::Range(0, 1)),
    ("localStorage.clear", C::Exact(0)),
    ("localStorage.getItem", C::Exact(1)),
    ("localStorage.key", C::Exact(1)),
    ("localStorage.removeItem", C::Exact(1)),
    ("localStorage.setItem", C::Exact(2)),
    ("sessionStorage.clear", C::Exact(0)),
    ("sessionStorage.getItem", C::Exact(1)),
    ("sessionStorage.key", C::Exact(1)),
    ("sessionStorage.removeItem", C::Exact(1)),
    ("sessionStorage.setItem", C::Exact(2)),
    ("Temporal.Duration.from", C::Exact(1)),
    ("Temporal.Instant.from", C::Exact(1)),
    ("Temporal.PlainDate.from", C::Range(1, 2)),
    ("Temporal.PlainDateTime.from", C::Range(1, 2)),
    ("Temporal.PlainTime.from", C::Range(1, 2)),
    ("Temporal.PlainYearMonth.from", C::Range(1, 2)),
    ("Temporal.ZonedDateTime.from", C::Range(1, 2)),
    ("Temporal.PlainMonthDay.from", C::Range(1, 2)),
    ("Temporal.Duration.compare", C::Range(2, 3)),
    ("Temporal.Instant.compare", C::Exact(2)),
    ("Temporal.PlainDate.compare", C::Exact(2)),
    ("Temporal.PlainDateTime.compare", C::Exact(2)),
    ("Temporal.PlainTime.compare", C::Exact(2)),
    ("Temporal.PlainYearMonth.compare", C::Exact(2)),
    ("Temporal.ZonedDateTime.compare", C::Exact(2)),
    ("Int8Array.from", C::Range(1, 3)),
    ("Uint8Array.from", C::Range(1, 3)),
    ("Uint8ClampedArray.from", C::Range(1, 3)),
    ("Int16Array.from", C::Range(1, 3)),
    ("Uint16Array.from", C::Range(1, 3)),
    ("Int32Array.from", C::Range(1, 3)),
    ("Uint32Array.from", C::Range(1, 3)),
    ("Float16Array.from", C::Range(1, 3)),
    ("Float32Array.from", C::Range(1, 3)),
    ("Float64Array.from", C::Range(1, 3)),
    ("BigInt64Array.from", C::Range(1, 3)),
    ("BigUint64Array.from", C::Range(1, 3)),
    ("new AbortController", C::Exact(0)),
    ("new AggregateError", C::Range(1, 3)),
    ("new Animation", C::Range(0, 2)),
    ("new ArrayBuffer", C::Range(1, 2)),
    ("new AsyncDisposableStack", C::Exact(0)),
    ("new Audio", C::Range(0, 1)),
    ("new AudioBuffer", C::Exact(1)),
    ("new AudioContext", C::Range(0, 1)),
    ("new AudioData", C::Exact(1)),
    ("new AudioDecoder", C::Exact(1)),
    ("new AudioEncoder", C::Exact(1)),
    ("new AudioWorkletNode", C::Range(2, 3)),
    ("new Blob", C::Range(0, 2)),
    ("new Boolean", C::Range(0, 1)),
    ("new Buffer", C::Range(1, 3)),
    ("new BroadcastChannel", C::Exact(1)),
    ("new ByteLengthQueuingStrategy", C::Exact(1)),
    ("new ClipboardItem", C::Range(1, 2)),
    ("new CompressionStream", C::Exact(1)),
    ("new Comment", C::Range(0, 1)),
    ("new CountQueuingStrategy", C::Exact(1)),
    ("new CustomEvent", C::Range(1, 2)),
    ("new CSSKeywordValue", C::Exact(1)),
    ("new CSSMathClamp", C::Exact(3)),
    ("new CSSMathInvert", C::Exact(1)),
    ("new CSSMathNegate", C::Exact(1)),
    ("new CSSMatrixComponent", C::Range(1, 2)),
    ("new CSSPerspective", C::Exact(1)),
    ("new CSSRotate", C::OneOf(&[1, 4])),
    ("new CSSScale", C::Range(2, 3)),
    ("new CSSSkew", C::Exact(2)),
    ("new CSSSkewX", C::Exact(1)),
    ("new CSSSkewY", C::Exact(1)),
    ("new CSSStyleSheet", C::Range(0, 1)),
    ("new CSSTransformValue", C::Exact(1)),
    ("new CSSTranslate", C::Range(2, 3)),
    ("new CSSUnitValue", C::Exact(2)),
    ("new CSSUnparsedValue", C::Exact(1)),
    ("new CSSVariableReferenceValue", C::Range(1, 2)),
    ("new DataView", C::Range(1, 3)),
    ("new DataTransfer", C::Exact(0)),
    ("new DecompressionStream", C::Exact(1)),
    ("new Date", C::Range(0, 7)),
    ("new DocumentFragment", C::Exact(0)),
    ("new DocumentTimeline", C::Range(0, 1)),
    ("new DOMMatrix", C::Range(0, 1)),
    ("new DOMMatrixReadOnly", C::Range(0, 1)),
    ("new DOMException", C::Range(0, 2)),
    ("new DOMPoint", C::Range(0, 4)),
    ("new DOMPointReadOnly", C::Range(0, 4)),
    ("new DOMQuad", C::Range(0, 4)),
    ("new DOMRect", C::Range(0, 4)),
    ("new DOMRectReadOnly", C::Range(0, 4)),
    ("new DOMParser", C::Exact(0)),
    ("new DisposableStack", C::Exact(0)),
    ("new EncodedAudioChunk", C::Exact(1)),
    ("new EncodedVideoChunk", C::Exact(1)),
    ("new Error", C::Range(0, 3)),
    ("new Event", C::Range(1, 2)),
    ("new EventSource", C::Range(1, 2)),
    ("new EventTarget", C::Exact(0)),
    ("new EvalError", C::Range(0, 3)),
    ("new File", C::Range(2, 3)),
    ("new FileReader", C::Exact(0)),
    ("new FinalizationRegistry", C::Exact(1)),
    ("new FontFace", C::Range(2, 3)),
    ("new FormData", C::Range(0, 2)),
    ("new GPUInternalError", C::Exact(1)),
    ("new GPUOutOfMemoryError", C::Exact(1)),
    ("new GPUPipelineError", C::Exact(2)),
    ("new GPUValidationError", C::Exact(1)),
    ("new Headers", C::Range(0, 1)),
    ("new Image", C::Range(0, 2)),
    ("new ImageCapture", C::Exact(1)),
    ("new ImageData", C::OneOf(&[2, 3, 4])),
    ("new ImageDecoder", C::Exact(1)),
    ("new IIRFilterNode", C::Exact(2)),
    ("new IntersectionObserver", C::Range(1, 2)),
    ("new Intl.Collator", C::Range(0, 2)),
    ("new Intl.DateTimeFormat", C::Range(0, 2)),
    ("new Intl.DisplayNames", C::Exact(2)),
    ("new Intl.DurationFormat", C::Range(0, 2)),
    ("new Intl.ListFormat", C::Range(0, 2)),
    ("new Intl.Locale", C::Range(1, 2)),
    ("new Intl.NumberFormat", C::Range(0, 2)),
    ("new Intl.PluralRules", C::Range(0, 2)),
    ("new Intl.RelativeTimeFormat", C::Range(0, 2)),
    ("new Intl.Segmenter", C::Range(0, 2)),
    ("new KeyframeEffect", C::Range(1, 3)),
    ("new Map", C::Range(0, 1)),
    ("new MessageChannel", C::Exact(0)),
    ("new MediaElementAudioSourceNode", C::Exact(2)),
    ("new MediaMetadata", C::Range(0, 1)),
    ("new MediaRecorder", C::Range(1, 2)),
    ("new MutationObserver", C::Exact(1)),
    ("new MediaSource", C::Exact(0)),
    ("new MediaStream", C::Range(0, 1)),
    ("new MediaStreamAudioSourceNode", C::Exact(2)),
    ("new Notification", C::Range(1, 2)),
    ("new OfflineAudioContext", C::OneOf(&[1, 3])),
    ("new OffscreenCanvas", C::Exact(2)),
    ("new Option", C::Range(0, 4)),
    ("new OverconstrainedError", C::Range(1, 2)),
    ("new Number", C::Range(0, 1)),
    ("new Path2D", C::Range(0, 1)),
    ("new PaymentRequest", C::Range(2, 3)),
    ("new PerformanceMark", C::Range(1, 2)),
    ("new PerformanceObserver", C::Exact(1)),
    ("new Promise", C::Exact(1)),
    ("new Proxy", C::Exact(2)),
    ("new Range", C::Exact(0)),
    ("new RangeError", C::Range(0, 3)),
    ("new ReadableStream", C::Range(0, 2)),
    ("new ReadableStreamBYOBReader", C::Exact(1)),
    ("new ReferenceError", C::Range(0, 3)),
    ("new RegExp", C::Range(0, 2)),
    ("new ReportingObserver", C::Range(1, 2)),
    ("new Request", C::Range(1, 2)),
    ("new ResizeObserver", C::Exact(1)),
    ("new Response", C::Range(0, 2)),
    ("new RTCError", C::Range(1, 2)),
    ("new RTCIceCandidate", C::Range(0, 1)),
    ("new RTCPeerConnection", C::Range(0, 1)),
    ("new RTCRtpScriptTransform", C::Range(1, 3)),
    ("new RTCSessionDescription", C::Exact(1)),
    ("new Sanitizer", C::Range(0, 1)),
    ("new ScrollTimeline", C::Range(0, 1)),
    ("new Set", C::Range(0, 1)),
    ("new SharedArrayBuffer", C::Range(1, 2)),
    ("new SpeechSynthesisUtterance", C::Range(0, 1)),
    ("new String", C::Range(0, 1)),
    ("new StaticRange", C::Exact(1)),
    ("new SuppressedError", C::Range(2, 3)),
    ("new SyntaxError", C::Range(0, 3)),
    ("new TaskController", C::Range(0, 1)),
    ("new Temporal.Duration", C::Range(0, 10)),
    ("new Temporal.Instant", C::Exact(1)),
    ("new Temporal.PlainDate", C::Range(3, 4)),
    ("new Temporal.PlainDateTime", C::Range(3, 10)),
    ("new Temporal.PlainMonthDay", C::Range(2, 4)),
    ("new Temporal.PlainTime", C::Range(0, 6)),
    ("new Temporal.PlainYearMonth", C::Range(2, 4)),
    ("new Temporal.ZonedDateTime", C::Range(2, 3)),
    ("new TextDecoder", C::Range(0, 2)),
    ("new TextDecoderStream", C::Range(0, 2)),
    ("new TextEncoder", C::Exact(0)),
    ("new TextEncoderStream", C::Exact(0)),
    ("new Text", C::Range(0, 1)),
    ("new Touch", C::Exact(1)),
    ("new TransformStream", C::Range(0, 3)),
    ("new TypeError", C::Range(0, 3)),
    ("new URIError", C::Range(0, 3)),
    ("new URL", C::Range(1, 2)),
    ("new URLPattern", C::Range(0, 3)),
    ("new URLSearchParams", C::Range(0, 1)),
    ("new VideoColorSpace", C::Range(0, 1)),
    ("new VideoDecoder", C::Exact(1)),
    ("new VideoEncoder", C::Exact(1)),
    ("new VideoFrame", C::Range(1, 2)),
    ("new ViewTimeline", C::Range(0, 1)),
    ("new VTTCue", C::Exact(3)),
    ("new VTTRegion", C::Exact(0)),
    ("new WebSocket", C::Range(1, 2)),
    ("new WebTransport", C::Range(1, 2)),
    ("new WebTransportError", C::Range(0, 2)),
    ("new WebAssembly.Exception", C::Range(2, 3)),
    ("new WebAssembly.Global", C::Range(1, 2)),
    ("new WebAssembly.Instance", C::Range(1, 2)),
    ("new WebAssembly.Memory", C::Exact(1)),
    ("new WebAssembly.Module", C::Range(1, 2)),
    ("new WebAssembly.Table", C::Range(1, 2)),
    ("new WebAssembly.Tag", C::Exact(1)),
    ("new WeakMap", C::Range(0, 1)),
    ("new WeakRef", C::Exact(1)),
    ("new WeakSet", C::Range(0, 1)),
    ("new Worker", C::Range(1, 2)),
    ("new WritableStream", C::Range(0, 2)),
    ("new XMLHttpRequest", C::Exact(0)),
    ("new XPathEvaluator", C::Exact(0)),
    ("new XMLSerializer", C::Exact(0)),
    ("new XSLTProcessor", C::Exact(0)),
    ("new SharedWorker", C::Range(1, 2)),
    ("new AnimationPlaybackEvent", C::Range(1, 2)),
    ("new AnimationEvent", C::Range(1, 2)),
    ("new ClipboardEvent", C::Range(1, 2)),
    ("new CloseEvent", C::Range(1, 2)),
    ("new CommandEvent", C::Range(1, 2)),
    ("new CompositionEvent", C::Range(1, 2)),
    ("new ContentVisibilityAutoStateChangeEvent", C::Range(1, 2)),
    ("new CookieChangeEvent", C::Range(1, 2)),
    ("new DeviceMotionEvent", C::Range(1, 2)),
    ("new DeviceOrientationEvent", C::Range(1, 2)),
    ("new DragEvent", C::Range(1, 2)),
    ("new ErrorEvent", C::Range(1, 2)),
    ("new FocusEvent", C::Range(1, 2)),
    ("new FontFaceSetLoadEvent", C::Range(1, 2)),
    ("new GamepadEvent", C::Range(1, 2)),
    ("new HashChangeEvent", C::Range(1, 2)),
    ("new IDBVersionChangeEvent", C::Range(1, 2)),
    ("new InputEvent", C::Range(1, 2)),
    ("new KeyboardEvent", C::Range(1, 2)),
    ("new MIDIConnectionEvent", C::Range(1, 2)),
    ("new MIDIMessageEvent", C::Range(1, 2)),
    ("new MediaEncryptedEvent", C::Range(1, 2)),
    ("new MediaQueryListEvent", C::Range(1, 2)),
    ("new MessageEvent", C::Range(1, 2)),
    ("new MouseEvent", C::Range(1, 2)),
    ("new PageRevealEvent", C::Range(1, 2)),
    ("new PageSwapEvent", C::Range(1, 2)),
    ("new PageTransitionEvent", C::Range(1, 2)),
    ("new PaymentMethodChangeEvent", C::Range(1, 2)),
    ("new PaymentRequestUpdateEvent", C::Range(1, 2)),
    ("new PointerEvent", C::Range(1, 2)),
    ("new PopStateEvent", C::Range(1, 2)),
    ("new ProgressEvent", C::Range(1, 2)),
    ("new RTCDTMFToneChangeEvent", C::Range(1, 2)),
    ("new RTCPeerConnectionIceEvent", C::Range(1, 2)),
    ("new SecurityPolicyViolationEvent", C::Range(1, 2)),
    ("new StorageEvent", C::Range(1, 2)),
    ("new SubmitEvent", C::Range(1, 2)),
    ("new TouchEvent", C::Range(1, 2)),
    ("new TransitionEvent", C::Range(1, 2)),
    ("new ToggleEvent", C::Range(1, 2)),
    ("new TrackEvent", C::Range(1, 2)),
    ("new UIEvent", C::Range(1, 2)),
    ("new WebGLContextEvent", C::Range(1, 2)),
    ("new WheelEvent", C::Range(1, 2)),
    ("new AudioProcessingEvent", C::Exact(2)),
    ("new BlobEvent", C::Exact(2)),
    ("new FormDataEvent", C::Exact(2)),
    ("new GPUUncapturedErrorEvent", C::Exact(2)),
    ("new MediaKeyMessageEvent", C::Exact(2)),
    ("new MediaStreamTrackEvent", C::Exact(2)),
    ("new NavigateEvent", C::Exact(2)),
    ("new NavigationCurrentEntryChangeEvent", C::Exact(2)),
    ("new OfflineAudioCompletionEvent", C::Exact(2)),
    ("new PictureInPictureEvent", C::Exact(2)),
    ("new PromiseRejectionEvent", C::Exact(2)),
    ("new RTCDataChannelEvent", C::Exact(2)),
    ("new RTCErrorEvent", C::Exact(2)),
    ("new RTCPeerConnectionIceErrorEvent", C::Exact(2)),
    ("new RTCTrackEvent", C::Exact(2)),
    ("new SpeechRecognitionErrorEvent", C::Exact(2)),
    ("new SpeechRecognitionEvent", C::Exact(2)),
    ("new SpeechSynthesisErrorEvent", C::Exact(2)),
    ("new SpeechSynthesisEvent", C::Exact(2)),
    ("new TaskPriorityChangeEvent", C::Exact(2)),
    ("new AnalyserNode", C::Range(1, 2)),
    ("new AudioBufferSourceNode", C::Range(1, 2)),
    ("new BiquadFilterNode", C::Range(1, 2)),
    ("new ChannelMergerNode", C::Range(1, 2)),
    ("new ChannelSplitterNode", C::Range(1, 2)),
    ("new ConstantSourceNode", C::Range(1, 2)),
    ("new ConvolverNode", C::Range(1, 2)),
    ("new DelayNode", C::Range(1, 2)),
    ("new DynamicsCompressorNode", C::Range(1, 2)),
    ("new GainNode", C::Range(1, 2)),
    ("new MediaStreamAudioDestinationNode", C::Range(1, 2)),
    ("new OscillatorNode", C::Range(1, 2)),
    ("new PannerNode", C::Range(1, 2)),
    ("new PeriodicWave", C::Range(1, 2)),
    ("new StereoPannerNode", C::Range(1, 2)),
    ("new WaveShaperNode", C::Range(1, 2)),
    ("new Int8Array", C::Range(0, 3)),
    ("new Uint8Array", C::Range(0, 3)),
    ("new Uint8ClampedArray", C::Range(0, 3)),
    ("new Int16Array", C::Range(0, 3)),
    ("new Uint16Array", C::Range(0, 3)),
    ("new Int32Array", C::Range(0, 3)),
    ("new Uint32Array", C::Range(0, 3)),
    ("new Float16Array", C::Range(0, 3)),
    ("new Float32Array", C::Range(0, 3)),
    ("new Float64Array", C::Range(0, 3)),
    ("new BigInt64Array", C::Range(0, 3)),
    ("new BigUint64Array", C::Range(0, 3)),
    ("new WebAssembly.CompileError", C::Range(0, 3)),
    ("new WebAssembly.LinkError", C::Range(0, 3)),
    ("new WebAssembly.RuntimeError", C::Range(0, 3)),
];

#[cfg(test)]
mod tests {
    use crate::config::{ConfigFile, ConfigKind, ResolveOptions, objects_from_eslint_json};

    fn lint(src: &str, options: serde_json::Value) -> usize {
        let reg = crate::Registry::global();
        let json = serde_json::json!([{ "files": ["**/*.js"], "rules": {
            "unicorn/no-invalid-argument-count": ["error", options] } }]);
        let objs = objects_from_eslint_json(&json, reg).unwrap();
        let file = ConfigFile::new(ConfigKind::Eslint, None, "/p".into(), objs);
        let fc = file.file_config("a.js", &ResolveOptions::new()).unwrap();
        crate::lint_source(
            "/p/a.js",
            src,
            &fc,
            &crate::NoTypeInfo,
            crate::types::FileKey(0),
        )
        .diagnostics
        .len()
    }

    #[test]
    fn long_paths_many_arguments_and_malformed_input() {
        let path = vec!["a"; 2_000].join(".");
        assert_eq!(lint(&format!("{path}();"), serde_json::json!({})), 0);
        let args = vec!["1"; 3_000].join(", ");
        assert_eq!(
            lint(&format!("Math.abs({args});"), serde_json::json!({})),
            1
        );
        assert_eq!(lint("function f(a) {}\nf(1, 2);", serde_json::json!({})), 1);
        assert_eq!(
            lint(
                "x.y(1, 2, 3);",
                serde_json::json!({"*.y": {"max": 4294967296u64}})
            ),
            0
        );
        for src in ["Math.abs(", "new URL(", "f(...", "globalThis.("] {
            let _ = lint(src, serde_json::json!({}));
        }
    }
}
