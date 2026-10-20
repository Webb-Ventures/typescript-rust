//! `@typescript-eslint/no-unsafe-return`: disallow returning `any` from a function.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-unsafe-return",
        "Disallow returning a value with type `any` from a function",
        TS_REC_TC,
        Category::Suspicious,
    )
    .typed(),
    create: create_default::<NoUnsafeReturn>,
};

#[derive(Debug, Default)]
struct NoUnsafeReturn;

const THIS_HINT: &str = "\nYou can try to fix this by turning on the `noImplicitThis` compiler option, or adding a `this` parameter to the function.";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Unsafe {
    Any(AnyKind),
    AnyArray,
    PromiseAny,
}

impl Rule for NoUnsafeReturn {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Return, NodeKind::Arrow]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let (value, func, report_at) = match *cx.node(node) {
            Node::Return { arg: Some(arg) } => {
                let Some(f) = ancestors_inclusive(cx.file.parents, node)
                    .skip(1)
                    .find(|&a| is_function(cx.node(a).kind()))
                else {
                    return;
                };
                (arg, f, node)
            }
            Node::Arrow { flags, body, .. } if flags.contains(FnFlags::EXPRESSION_BODY) => {
                // ESTree has no parentheses: `() => (x)` reports `x`.
                (body, node, skip_parens(cx.ast(), body))
            }
            _ => return,
        };
        let (is_async, return_type) = match *cx.node(func) {
            Node::FunctionDecl {
                flags, return_type, ..
            }
            | Node::FunctionExpr {
                flags, return_type, ..
            }
            | Node::Arrow {
                flags, return_type, ..
            } => (flags.contains(FnFlags::ASYNC), return_type),
            _ => return,
        };
        let ti = cx.types();
        let Some(t) = constrained_type_at(ti, value) else {
            return;
        };
        if t == Ty::UNRESOLVED {
            return;
        }
        // The function's type as typescript-eslint sees it (observed): a function expression's
        // contextual type where its rules see one, else the function's own type.
        let is_expr = matches!(
            cx.node(func),
            Node::FunctionExpr { .. } | Node::Arrow { .. }
        );
        let fn_type = is_expr
            .then(|| eslint_contextual_type(cx, func))
            .flatten()
            .or_else(|| ti.type_at(func));
        let Some(fn_type) = fn_type else {
            return;
        };
        if fn_type == Ty::UNRESOLVED {
            return;
        }
        let raw_returns: Vec<Ty> = parts(ti, ti.non_nullable(fn_type))
            .into_iter()
            .flat_map(|p| ti.signatures(p, false))
            .map(|s| ti.return_type(s))
            .collect();
        if raw_returns.contains(&Ty::UNRESOLVED) {
            return;
        }
        let returns: Vec<Ty> = raw_returns
            .iter()
            .map(|&r| if is_async { ti.awaited_type(r) } else { r })
            .collect();
        if returns.contains(&Ty::UNRESOLVED) {
            return;
        }
        // An annotated return type that is the returned value's type, `any` or `unknown`
        // (awaited, in an async function) is intentional. An array literal has a fresh type
        // of its own (in tsc), never the annotation's.
        let fresh = fresh_array(cx, value);
        if return_type.is_some()
            && raw_returns
                .iter()
                .chain(&returns)
                .any(|&r| (r == t && !fresh) || is_any(ti, r) || is_unknown(ti, r))
        {
            return;
        }

        let unsafe_kind = if let Some(k) = any_kind(ti, t) {
            Some(Unsafe::Any(k))
        } else if is_any_array(ti, t) {
            Some(Unsafe::AnyArray)
        } else if is_async && thenable(ti, t) == Some(true) && is_any(ti, ti.awaited_type(t)) {
            Some(Unsafe::PromiseAny)
        } else {
            None
        };

        let span = cx.span(report_at);
        if let Some(k) = unsafe_kind {
            // Returning `any` where `unknown` (or `any[]` where `unknown[]`) is expected is fine.
            let allowed = returns.iter().any(|&r| {
                (matches!(k, Unsafe::Any(_) | Unsafe::PromiseAny) && is_unknown(ti, r))
                    || (k == Unsafe::AnyArray && is_unknown_array(ti, r))
            });
            if allowed {
                return;
            }
            let ast = cx.ast();
            let msg = match k {
                Unsafe::Any(AnyKind::Any)
                    if matches!(cx.node(skip_parens(ast, value)), Node::This {}) =>
                {
                    cx.report(
                        span,
                        format!(
                            "Unsafe return of a value of type ``any``. `this` is typed as `any`.{THIS_HINT}"
                        ),
                    )
                    .id("unsafeReturnThis");
                    return;
                }
                Unsafe::Any(AnyKind::Any) => "`any`",
                Unsafe::Any(AnyKind::Error) => "error",
                Unsafe::AnyArray => "`any[]`",
                Unsafe::PromiseAny => "`Promise<any>`",
            };
            cx.report(span, format!("Unsafe return of a value of type {msg}."))
                .id("unsafeReturn");
            return;
        }

        // `new Set()` / `new Map()` without arguments take their type arguments from the
        // context.
        if let Node::New {
            args,
            type_args: None,
            ..
        } = *cx.node(skip_parens(cx.ast(), value))
            && cx.ast().list(args).is_empty()
        {
            return;
        }
        for &r in &returns {
            if is_unsafe_assignment(ti, t, r) {
                let sender = ti.type_to_string(t);
                let receiver = ti.type_to_string(r);
                cx.report(
                    span,
                    format!(
                        "Unsafe return of type `{sender}` from function with return type `{receiver}`."
                    ),
                )
                .id("unsafeReturnAssignment");
                return;
            }
        }
    }
}

/// `value` has an array literal's own type: an array literal, or a variable declared in this
/// file without a type annotation and initialized with one.
fn fresh_array(cx: &Ctx<'_, '_>, value: NodeId) -> bool {
    let ast = cx.ast();
    let v = skip_parens(ast, value);
    match cx.node(v) {
        Node::ArrayLit { .. } => true,
        Node::Ident { .. } => {
            let ti = cx.types();
            let Some(sym) = ti.symbol_at(v) else {
                return false;
            };
            let decls = ti.symbol_decls(sym);
            let [d] = decls[..] else { return false };
            if !d.in_this_file {
                return false;
            }
            let declarator = match cx.node(d.node) {
                Node::VarDeclarator { .. } => d.node,
                _ => match cx.parent(d.node) {
                    Some(p) if matches!(cx.node(p), Node::VarDeclarator { .. }) => p,
                    _ => return false,
                },
            };
            matches!(
                *cx.node(declarator),
                Node::VarDeclarator { type_ann: None, init: Some(i), .. }
                    if matches!(cx.node(skip_parens(ast, i)), Node::ArrayLit { .. })
            )
        }
        _ => false,
    }
}
