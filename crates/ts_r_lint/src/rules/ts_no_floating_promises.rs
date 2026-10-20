//! `@typescript-eslint/no-floating-promises`: require promise-valued statements to be handled.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-floating-promises",
        "Require Promise-like statements to be handled appropriately",
        TS_REC_TC,
        Category::Correctness,
    )
    .typed()
    .suggestions(),
    create: create::<NoFloatingPromises>,
};

#[derive(Debug)]
struct NoFloatingPromises {
    check_thenables: bool,
    ignore_void: bool,
    ignore_iife: bool,
    safe_promises: Vec<Specifier>,
    safe_calls: Vec<Specifier>,
}

impl FromOptions for NoFloatingPromises {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(NoFloatingPromises {
            check_thenables: bool_field(o, "checkThenables", false),
            ignore_void: bool_field(o, "ignoreVoid", true),
            ignore_iife: bool_field(o, "ignoreIIFE", false),
            safe_promises: parse_specifiers(o.get("allowForKnownSafePromises")),
            safe_calls: parse_specifiers(o.get("allowForKnownSafeCalls")),
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Floating {
    Promise,
    PromiseArray,
    UselessRejectionHandler,
}

const BASE: &str = "Promises must be awaited, end with a call to .catch, or end with a call to .then with a rejection handler.";
const BASE_VOID: &str = "Promises must be awaited, end with a call to .catch, end with a call to .then with a rejection handler or be explicitly marked as ignored with the `void` operator.";
const USELESS: &str = " A rejection handler that is not a function will be ignored.";
const ARRAY: &str = "An array of Promises may be unintentional. Consider handling the promises' fulfillment or rejection with Promise.all or similar.";
const ARRAY_VOID: &str = "An array of Promises may be unintentional. Consider handling the promises' fulfillment or rejection with Promise.all or similar, or explicitly marking the expression as ignored with the `void` operator.";

impl Rule for NoFloatingPromises {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::ExprStmt]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Node::ExprStmt { expr } = *cx.node(node) else {
            return;
        };
        let Some(kind) = self.unhandled(cx, expr) else {
            return;
        };
        let (id, msg): (&'static str, String) = match (kind, self.ignore_void) {
            (Floating::Promise, true) => ("floatingVoid", BASE_VOID.into()),
            (Floating::Promise, false) => ("floating", BASE.into()),
            (Floating::UselessRejectionHandler, true) => (
                "floatingUselessRejectionHandlerVoid",
                format!("{BASE_VOID}{USELESS}"),
            ),
            (Floating::UselessRejectionHandler, false) => (
                "floatingUselessRejectionHandler",
                format!("{BASE}{USELESS}"),
            ),
            (Floating::PromiseArray, true) => ("floatingPromiseArrayVoid", ARRAY_VOID.into()),
            (Floating::PromiseArray, false) => ("floatingPromiseArray", ARRAY.into()),
        };
        let fixes = (kind != Floating::PromiseArray).then(|| {
            let void_fix = self.ignore_void.then(|| prefix_fix(cx, expr, "void"));
            (void_fix, await_fix(cx, expr))
        });
        let r = cx.report_node(node, msg).id(id);
        if let Some((void_fix, await_fix)) = fixes {
            let r = match void_fix {
                Some(f) => r.suggest("floatingFixVoid", "Add void operator to ignore.", f),
                None => r,
            };
            r.suggest("floatingFixAwait", "Add await operator.", await_fix);
        }
    }
}

/// Inserts `<keyword> ` before `expr`, parenthesising it when its precedence is lower than a
/// unary operator's.
fn prefix_fix(cx: &Ctx<'_, '_>, expr: NodeId, keyword: &str) -> Fix {
    let ast = cx.ast();
    let span = cx.span(expr);
    if precedence(ast, expr) < 16 || matches!(cx.node(expr), Node::TypeAssertion { .. }) {
        Fix::replace(span, format!("{keyword} ({})", span.text(cx.src())))
    } else {
        Fix::insert(span.start, format!("{keyword} "))
    }
}

/// `await <expr>`; on `void <expr>` the `void` becomes `await`.
fn await_fix(cx: &Ctx<'_, '_>, expr: NodeId) -> Fix {
    if let Node::Unary {
        op: UnaryOp::Void, ..
    } = *cx.node(expr)
    {
        let start = cx.span(expr).start;
        return Fix::replace(Span::new(start, start + 4), "await");
    }
    prefix_fix(cx, expr, "await")
}

impl NoFloatingPromises {
    fn unhandled(&self, cx: &Ctx<'_, '_>, node: NodeId) -> Option<Floating> {
        let ast = cx.ast();
        let ti = cx.types();
        let node = skip_parens(ast, node);
        match *cx.node(node) {
            Node::Seq { exprs } => {
                return ast.list(exprs).iter().find_map(|&e| self.unhandled(cx, e));
            }
            Node::Unary {
                op: UnaryOp::Void,
                arg,
            } => {
                if self.ignore_void {
                    return None;
                }
                return self.unhandled(cx, arg);
            }
            Node::Conditional { cons, alt, .. } => {
                return self.unhandled(cx, alt).or_else(|| self.unhandled(cx, cons));
            }
            // An assignment stores the promise somewhere.
            Node::Assign { .. } => return None,
            Node::Await { arg } => {
                let t = ti.type_at(arg)?;
                return self
                    .is_promise_array(ti, t)
                    .then_some(Floating::PromiseArray);
            }
            _ => {}
        }
        let t = ti.type_at(node)?;
        if self.is_promise_array(ti, t) {
            return Some(Floating::PromiseArray);
        }
        if !self.is_promise_like(ti, t) {
            return None;
        }
        match *cx.node(node) {
            Node::Binary {
                left,
                op: BinaryOp::LogicalAnd | BinaryOp::LogicalOr | BinaryOp::Coalesce,
                right,
            } => self
                .unhandled(cx, left)
                .or_else(|| self.unhandled(cx, right)),
            Node::Call { callee, args, .. } => {
                let args = ast.list(args);
                let callee = skip_parens(ast, callee);
                if let Node::Member { obj, prop, .. } = *cx.node(callee) {
                    match ident_name(ast, prop) {
                        Some("catch") if !args.is_empty() => {
                            return self.rejection_handler(cx, args[0]);
                        }
                        Some("then") if args.len() >= 2 => {
                            if args[..2]
                                .iter()
                                .any(|&a| matches!(cx.node(a), Node::Spread { .. }))
                            {
                                return Some(Floating::Promise);
                            }
                            return self.rejection_handler(cx, args[1]);
                        }
                        Some("finally") => return self.unhandled(cx, obj),
                        _ => {}
                    }
                }
                if self.ignore_iife
                    && matches!(
                        cx.node(callee),
                        Node::FunctionExpr { .. } | Node::Arrow { .. }
                    )
                {
                    return None;
                }
                if self.is_safe_call(cx, callee) {
                    return None;
                }
                Some(Floating::Promise)
            }
            _ => Some(Floating::Promise),
        }
    }

    /// `.catch(h)` / `.then(_, h)`: handled when `h` is a function.
    fn rejection_handler(&self, cx: &Ctx<'_, '_>, h: NodeId) -> Option<Floating> {
        if matches!(cx.node(h), Node::Spread { .. }) {
            return Some(Floating::Promise);
        }
        let ti = cx.types();
        let t = ti.type_at(h)?;
        let mut unknown = false;
        let mut callable = true;
        for p in parts(ti, constrained(ti, t)) {
            if p == Ty::UNRESOLVED || is_any(ti, p) || is_unknown(ti, p) {
                unknown = true;
            } else if ti.signatures(p, false).is_empty() {
                callable = false;
            }
        }
        if callable || unknown {
            None
        } else {
            Some(Floating::UselessRejectionHandler)
        }
    }

    fn is_safe_call(&self, cx: &Ctx<'_, '_>, callee: NodeId) -> bool {
        if self.safe_calls.is_empty() {
            return false;
        }
        let ast = cx.ast();
        let ti = cx.types();
        let name_node = match *cx.node(callee) {
            Node::Ident { .. } => callee,
            Node::Member { prop, .. } => prop,
            _ => return false,
        };
        let Some(name) = ident_name(ast, name_node) else {
            return false;
        };
        let Some(mut s) = ti.symbol_at(callee) else {
            return self
                .safe_calls
                .iter()
                .any(|sp| sp.from == SpecFrom::Any && sp.names.iter().any(|n| n == name));
        };
        if let Some(r) = ti.resolve_alias(s) {
            s = r;
        }
        symbol_matches(ti, s, name, &self.safe_calls)
    }

    /// Some union/intersection member is a `Promise` (or, with `checkThenables`, any
    /// thenable), and the type is not a known-safe one.
    fn is_promise_like(&self, ti: &dyn TypeInfo, t: Ty) -> bool {
        if t == Ty::UNRESOLVED || is_any(ti, t) {
            return false;
        }
        if type_matches(ti, t, &self.safe_promises) {
            return false;
        }
        let t = constrained(ti, t);
        for p in parts(ti, t) {
            let p = constrained(ti, p);
            if p == Ty::UNRESOLVED || type_matches(ti, p, &self.safe_promises) {
                continue;
            }
            let promise = if self.check_thenables {
                is_thenable(ti, p)
            } else {
                is_builtin_symbol_like(ti, p, &["Promise"])
            };
            if promise {
                return true;
            }
        }
        false
    }

    /// Some union member is an array or tuple with a promise-like element type.
    fn is_promise_array(&self, ti: &dyn TypeInfo, t: Ty) -> bool {
        if t == Ty::UNRESOLVED {
            return false;
        }
        // A member's constraint can be a union again (`S[0]` with `S extends (A | B)[]`).
        let members = parts(ti, constrained(ti, t))
            .into_iter()
            .flat_map(|p| parts(ti, constrained(ti, p)));
        for p in members {
            if p == Ty::UNRESOLVED || !(ti.is_array(p) || ti.is_tuple(p)) {
                continue;
            }
            let elems = if ti.is_tuple(p) {
                ti.type_args(p)
            } else {
                ti.array_element(p).into_iter().collect()
            };
            if elems.into_iter().any(|e| self.is_promise_like(ti, e)) {
                return true;
            }
        }
        false
    }
}
