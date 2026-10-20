//! `@typescript-eslint/return-await`: `return await` exactly where it changes error handling.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/return-await",
        "Enforce consistent awaiting of returned promises",
        TS_STRICT_TC,
        Category::Suspicious,
    )
    .typed()
    .fixable()
    .suggestions(),
    create: create::<ReturnAwait>,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    InTryCatch,
    Always,
    Never,
    ErrorHandlingCorrectnessOnly,
}

#[derive(Debug)]
struct ReturnAwait {
    mode: Mode,
}

impl FromOptions for ReturnAwait {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let mode = match opts.first().and_then(Value::as_str) {
            Some("always") => Mode::Always,
            Some("never") => Mode::Never,
            Some("error-handling-correctness-only") => Mode::ErrorHandlingCorrectnessOnly,
            _ => Mode::InTryCatch,
        };
        Ok(ReturnAwait { mode })
    }
}

const NON_PROMISE: &str = "Returning an awaited value that is not a promise is not allowed.";
const DISALLOWED: &str = "Returning an awaited promise is not allowed in this context.";
const REQUIRED: &str = "Returning an awaited promise is required in this context.";
const REQUIRED_SUGGESTION: &str =
    "Add `await` before the expression. Use caution as this may impact control flow.";
const DISALLOWED_SUGGESTION: &str =
    "Remove `await` before the expression. Use caution as this may impact control flow.";

impl Rule for ReturnAwait {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Return, NodeKind::Arrow]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        match *cx.node(node) {
            Node::Return { arg: Some(arg) } => {
                let Some(func) = enclosing_function(cx, node) else {
                    return;
                };
                if !is_async(cx, func) {
                    return;
                }
                let affects = affects_error_handling(cx, node);
                self.check(cx, arg, affects, false, 0);
            }
            Node::Arrow { flags, body, .. }
                if flags.contains(FnFlags::ASYNC) && flags.contains(FnFlags::EXPRESSION_BODY) =>
            {
                self.check(cx, body, false, true, 0);
            }
            _ => {}
        }
    }
}

impl ReturnAwait {
    /// Checks a returned expression, descending into the branches of `?:`.
    fn check(&self, cx: &mut Ctx<'_, '_>, e: NodeId, affects: bool, arrow_body: bool, depth: u32) {
        if depth > 64 {
            return;
        }
        match *cx.node(e) {
            Node::Paren { expr } => return self.check(cx, expr, affects, false, depth + 1),
            Node::Conditional { cons, alt, .. } => {
                self.check(cx, cons, affects, false, depth + 1);
                self.check(cx, alt, affects, false, depth + 1);
                return;
            }
            _ => {}
        }
        let ti = cx.types();
        if let Node::Await { arg } = *cx.node(e) {
            let Some(t) = ti.type_at(arg) else { return };
            let Some(is_promise) = thenable(ti, t) else {
                return;
            };
            if !is_promise {
                let fix = remove_await(cx, e, arg, arrow_body);
                cx.report_node(e, NON_PROMISE)
                    .id("nonPromiseAwait")
                    .fix(fix);
                return;
            }
            let disallowed = match self.mode {
                Mode::Never => true,
                Mode::InTryCatch => !affects,
                Mode::Always | Mode::ErrorHandlingCorrectnessOnly => false,
            };
            if !disallowed {
                return;
            }
            let fix = remove_await(cx, e, arg, arrow_body);
            let r = cx.report_node(e, DISALLOWED).id("disallowedPromiseAwait");
            if affects {
                r.suggest(
                    "disallowedPromiseAwaitSuggestion",
                    DISALLOWED_SUGGESTION,
                    fix,
                );
            } else {
                r.fix(fix);
            }
            return;
        }
        let Some(t) = ti.type_at(e) else { return };
        if thenable(ti, t) != Some(true) {
            return;
        }
        let required = match self.mode {
            Mode::Never => false,
            Mode::Always => true,
            Mode::InTryCatch | Mode::ErrorHandlingCorrectnessOnly => affects,
        };
        if !required {
            return;
        }
        let text = cx.span(e).text(cx.src());
        let fix = if needs_parens(cx.node(e)) {
            Fix::replace(cx.span(e), format!("await ({text})"))
        } else {
            Fix::replace(cx.span(e), format!("await {text}"))
        };
        let r = cx.report_node(e, REQUIRED).id("requiredPromiseAwait");
        if affects {
            r.suggest("requiredPromiseAwaitSuggestion", REQUIRED_SUGGESTION, fix);
        } else {
            r.fix(fix);
        }
    }
}

/// Deletes `await` and the spaces after it; an object literal that becomes an arrow body
/// gets parentheses.
fn remove_await(cx: &Ctx<'_, '_>, e: NodeId, arg: NodeId, arrow_body: bool) -> Fix {
    let src = cx.src();
    let start = cx.span(e).start as usize;
    let mut end = start + "await".len();
    while end < src.len() && src.as_bytes()[end].is_ascii_whitespace() {
        end += 1;
    }
    let fix = Fix::delete(Span::new(start as u32, end as u32));
    let arg_span = cx.span(arg);
    if arrow_body && arg_span.text(src).starts_with('{') {
        fix.and(Span::new(arg_span.start, arg_span.start), "(")
            .and(Span::new(arg_span.end, arg_span.end), ")")
    } else {
        fix
    }
}

/// Expressions that bind looser than `await x`.
fn needs_parens(n: &Node) -> bool {
    matches!(
        n,
        Node::Binary { .. }
            | Node::Assign { .. }
            | Node::Conditional { .. }
            | Node::Seq { .. }
            | Node::As { .. }
            | Node::Satisfies { .. }
            | Node::Arrow { .. }
            | Node::Yield { .. }
    )
}

fn is_function(n: &Node) -> bool {
    matches!(
        n,
        Node::FunctionDecl { .. } | Node::FunctionExpr { .. } | Node::Arrow { .. }
    )
}

fn enclosing_function(cx: &Ctx<'_, '_>, node: NodeId) -> Option<NodeId> {
    let mut cur = cx.parent(node);
    let mut depth = 0;
    while let Some(p) = cur {
        if is_function(cx.node(p)) {
            return Some(p);
        }
        depth += 1;
        if depth > 1024 {
            return None;
        }
        cur = cx.parent(p);
    }
    None
}

fn is_async(cx: &Ctx<'_, '_>, f: NodeId) -> bool {
    match *cx.node(f) {
        Node::FunctionDecl { flags, .. }
        | Node::FunctionExpr { flags, .. }
        | Node::Arrow { flags, .. } => flags.contains(FnFlags::ASYNC),
        _ => false,
    }
}

/// The return sits where awaiting changes behavior: in a `try` block with a `catch` or
/// `finally`, in a `catch` block with a `finally`, or after a `using` declaration of an
/// enclosing block.
fn affects_error_handling(cx: &Ctx<'_, '_>, ret: NodeId) -> bool {
    let ast = cx.ast();
    let mut child = ret;
    let mut cur = cx.parent(ret);
    let mut depth = 0;
    while let Some(p) = cur {
        let n = cx.node(p);
        if is_function(n) {
            return false;
        }
        match *n {
            Node::Try {
                block,
                handler,
                finalizer,
            } => {
                if child == block && (handler.is_some() || finalizer.is_some()) {
                    return true;
                }
                if Some(child) == handler && finalizer.is_some() {
                    return true;
                }
            }
            _ => {
                if let Some(list) = statement_list(ast, p)
                    && let Some(i) = list.iter().position(|&s| s == child)
                    && list[..i].iter().any(|&s| {
                        matches!(
                            cx.node(s),
                            Node::VarDecl {
                                kind: VarKind::Using | VarKind::AwaitUsing,
                                ..
                            }
                        )
                    })
                {
                    return true;
                }
            }
        }
        depth += 1;
        if depth > 1024 {
            return false;
        }
        child = p;
        cur = cx.parent(p);
    }
    false
}
