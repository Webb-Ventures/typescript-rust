//! `@typescript-eslint/use-unknown-in-catch-callback-variable`: promise rejection callbacks
//! take their error as `unknown`.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/use-unknown-in-catch-callback-variable",
        "Enforce typing arguments in Promise rejection callbacks as `unknown`",
        TS_STRICT_TC,
        Category::Suspicious,
    )
    .typed()
    .suggestions(),
    create: create_default::<UseUnknownInCatchCallbackVariable>,
};

#[derive(Debug, Default)]
struct UseUnknownInCatchCallbackVariable;

impl Rule for UseUnknownInCatchCallbackVariable {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Node::Call { callee, args, .. } = *cx.node(node) else {
            return;
        };
        let ast = cx.ast();
        let ti = cx.types();
        let callee = skip_parens(ast, callee);
        let (obj, method) = match *cx.node(callee) {
            Node::Member { obj, prop, .. } => (obj, ident_name(ast, prop).map(str::to_string)),
            Node::Index { obj, index, .. } => (
                obj,
                static_string_eval(cx, index)
                    .or_else(|| unchanged_init(cx, index))
                    .or_else(|| {
                        ti.type_at(index)
                            .filter(|&t| ti.flags(t).contains(TypeFlags::STRING_LITERAL))
                            .and_then(|t| ti.literal_text(t))
                    }),
            ),
            _ => return,
        };
        let (index, then) = match method.as_deref() {
            Some("catch") => (0, false),
            Some("then") => (1, true),
            _ => return,
        };
        let Some(ot) = ti.type_at(obj) else { return };
        if thenable(ti, ot) != Some(true) {
            return;
        }
        // Find the handler argument, counting through spreads of fixed-length tuples.
        let mut pos = 0usize;
        let mut handler = None;
        for &a in ast.list(args) {
            if let Node::Spread { arg } = *cx.node(a) {
                let Some(t) = ti.type_at(arg) else { return };
                if !ti.is_tuple(t) || ti.tuple_rest_index(t).is_some() {
                    return;
                }
                let n = ti.type_args(t).len();
                if pos + n > index {
                    return;
                }
                pos += n;
                continue;
            }
            if pos == index {
                handler = Some(a);
                break;
            }
            pos += 1;
        }
        let Some(handler) = handler else { return };
        let mut fns = Vec::new();
        collect_functions(cx, handler, &mut fns, 0);
        let msg = if then {
            "Prefer the safe `: unknown` for a `then` rejection callback variable."
        } else {
            "Prefer the safe `: unknown` for a `catch` callback variable."
        };
        for f in fns {
            check_function(cx, f, msg);
        }
    }
}

/// The string a `let` / `var` binding holds when it is never written after its initializer.
fn unchanged_init(cx: &Ctx<'_, '_>, ident: NodeId) -> Option<String> {
    use crate::semantic::DeclKind;
    let ast = cx.ast();
    let ident = skip_parens(ast, ident);
    let sem = cx.semantic();
    let s = sem.reference(sem.reference_at(ident)?).symbol()?;
    let sym = sem.symbol(s);
    let [decl] = sym.decls.as_slice() else {
        return None;
    };
    if !matches!(decl.kind, DeclKind::Let | DeclKind::Var) {
        return None;
    }
    let Node::VarDeclarator {
        name,
        init: Some(init),
        ..
    } = *cx.node(decl.node)
    else {
        return None;
    };
    if name != decl.name
        || sym.refs.iter().any(|&r| {
            let r = sem.reference(r);
            r.is_write() && !r.flags.contains(crate::semantic::RefFlags::INIT)
        })
    {
        return None;
    }
    static_string_eval(cx, init)
}

/// Function literals the handler expression may evaluate to.
fn collect_functions(cx: &Ctx<'_, '_>, e: NodeId, out: &mut Vec<NodeId>, depth: u32) {
    if depth > 32 {
        return;
    }
    let ast = cx.ast();
    let e = skip_parens(ast, e);
    match *cx.node(e) {
        Node::FunctionExpr { .. } | Node::Arrow { .. } => out.push(e),
        Node::Conditional { cons, alt, .. } => {
            collect_functions(cx, cons, out, depth + 1);
            collect_functions(cx, alt, out, depth + 1);
        }
        Node::Binary {
            left,
            op: BinaryOp::LogicalAnd | BinaryOp::LogicalOr | BinaryOp::Coalesce,
            right,
        } => {
            collect_functions(cx, left, out, depth + 1);
            collect_functions(cx, right, out, depth + 1);
        }
        Node::Seq { exprs } => {
            if let Some(&l) = ast.list(exprs).last() {
                collect_functions(cx, l, out, depth + 1);
            }
        }
        _ => {}
    }
}

fn check_function(cx: &mut Ctx<'_, '_>, f: NodeId, msg: &str) {
    let ast = cx.ast();
    let params = match *cx.node(f) {
        Node::FunctionExpr { params, .. } | Node::Arrow { params, .. } => params,
        _ => return,
    };
    let Some(&param) = ast.list(params).first() else {
        return;
    };
    let Node::Param {
        rest,
        pattern,
        type_ann,
        optional,
        ..
    } = *cx.node(param)
    else {
        return;
    };
    let ti = cx.types();
    let src = cx.src();
    let annotated = type_ann.and_then(|t| ti.type_of_type_node(t));
    if annotated == Some(Ty::UNRESOLVED) {
        return;
    }
    let span = cx.span(param);
    if rest {
        if let Some(t) = annotated {
            let first_unknown = if ti.is_tuple(t) {
                ti.type_args(t).first().is_some_and(|&e| is_unknown(ti, e))
            } else if ti.is_array(t) {
                ti.array_element(t).is_some_and(|e| is_unknown(ti, e))
            } else {
                false
            };
            if first_unknown {
                return;
            }
            let Some(ta) = type_ann else { return };
            let fix = Fix::replace(type_span(cx, ta), "[unknown]");
            cx.report(span, msg).id("useUnknown").suggest(
                "wrongRestTypeAnnotationSuggestion",
                "Change existing type annotation to `: [unknown]`.",
                fix,
            );
        } else {
            let fix = Fix::insert(cx.span(pattern).end, ": [unknown]");
            cx.report(span, msg).id("useUnknown").suggest(
                "addUnknownRestTypeAnnotationSuggestion",
                "Add an explicit `: [unknown]` type annotation to the rejection callback rest variable.",
                fix,
            );
        }
        return;
    }
    match cx.node(pattern) {
        Node::ArrayPattern { .. } => {
            cx.report(span, format!("{msg} The thrown error may not be iterable."))
                .id("useUnknownArrayDestructuringPattern");
            return;
        }
        Node::ObjectPattern { .. } => {
            cx.report(
                span,
                format!(
                    "{msg} The thrown error may be nullable, or may not have the expected shape."
                ),
            )
            .id("useUnknownObjectDestructuringPattern");
            return;
        }
        _ => {}
    }
    if let Some(t) = annotated {
        if is_unknown(ti, t) {
            return;
        }
        let Some(ta) = type_ann else { return };
        let fix = Fix::replace(type_span(cx, ta), "unknown");
        cx.report(span, msg).id("useUnknown").suggest(
            "wrongTypeAnnotationSuggestion",
            "Change existing type annotation to `: unknown`.",
            fix,
        );
        return;
    }
    // `err` / `err?` / `err = 2`: add `: unknown` after the name (and `?`).
    let name_end = cx.span(pattern).end;
    let at = if optional {
        find_char(src, name_end, b'?').map_or(name_end, |q| q + 1)
    } else {
        name_end
    };
    // A lone arrow parameter without parentheses (directly followed by `=>`) needs them.
    let parenthesized = matches!(cx.node(f), Node::FunctionExpr { .. })
        || !src[skip_trivia_forward(src, name_end as usize)..].starts_with("=>");
    let fix = if parenthesized {
        Fix::insert(at, ": unknown")
    } else {
        Fix::replace(
            cx.span(pattern),
            format!("({}: unknown)", cx.span(pattern).text(src)),
        )
    };
    cx.report(span, msg).id("useUnknown").suggest(
        "addUnknownTypeAnnotationSuggestion",
        "Add an explicit `: unknown` type annotation to the rejection callback variable.",
        fix,
    );
}

/// The span of a type annotation's type (without a leading `:`).
fn type_span(cx: &Ctx<'_, '_>, ta: NodeId) -> Span {
    let span = cx.span(ta);
    let src = cx.src();
    if span.text(src).starts_with(':') {
        let start = skip_trivia_forward(src, span.start as usize + 1) as u32;
        Span::new(start, span.end)
    } else {
        span
    }
}
