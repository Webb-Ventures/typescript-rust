//! `unicorn/prefer-url-search-parameters`: `new URLSearchParams(query)` over
//! `query.split("&").map(part => part.split("="))`.
//!
//! The pipeline (through TS wrappers) is a plain `.map(callback)` call without type
//! arguments on a plain `.split("&")` call (one argument, a static string or template)
//! whose receiver is not known to be something other than a string. The callback is a
//! non-async, non-generator arrow or function with one plain parameter that returns
//! `<param>.split("=")` or `.split("=", 2)` (static value 2), through TS wrappers.
//!
//! `Object.fromEntries(pipeline)` (global `Object`), `new Map(pipeline)` (global `Map`) and
//! `new URLSearchParams(pipeline)` are reported as a whole and keep their wrapper; a
//! pipeline that is the first argument of anything shaped like one of them (by name, also
//! computed or optional) is otherwise left alone, unless the wrapper has type arguments.
//! Nothing is reported when `URLSearchParams` is a local runtime binding other than the
//! import from `"url"`/`"node:url"`. The suggestion is withheld when a comment would be
//! lost (comments in the query and around the wrapper's argument are kept).

use super::prelude::*;
use super::unicorn_abort::is_global_name_available;
use super::unicorn_static::{Static, static_value_no_side_effects};
use super::unicorn_text::{call_argument_text, paren_span, would_remove_comments};
use super::unicorn_types::is_known_non_string;
use super::unicorn_util::skip_wrappers;
use crate::semantic::DeclKind;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-url-search-parameters",
        "Prefer `URLSearchParams` over manually splitting query strings.",
        NO_PRESET,
        Category::Style,
    )
    .suggestions(),
    create: create_default::<PreferUrlSearchParameters>,
};

#[derive(Debug, Default)]
struct PreferUrlSearchParameters;

/// A string literal or substitution-free template's value.
fn static_string(ast: &Ast, id: NodeId) -> Option<String> {
    match *ast.node(skip_wrappers(ast, id)) {
        Node::StringLit { value } => Some(ast.atom(value).to_owned()),
        Node::TemplateLit { parts } => match ast.list(parts) {
            &[quasi] => match *ast.node(quasi) {
                Node::TemplateElement {
                    cooked,
                    invalid_escape: false,
                } => Some(ast.atom(cooked).to_owned()),
                _ => None,
            },
            _ => None,
        },
        _ => None,
    }
}

/// `<obj>.<name>(args)`: plain member, no `?.`, no spread among `max` first arguments.
fn method_call<'a>(
    ast: &'a Ast,
    id: NodeId,
    name: &str,
    min: usize,
    max: usize,
) -> Option<(NodeId, &'a [NodeId])> {
    let Node::Call {
        callee,
        optional: false,
        args,
        ..
    } = *ast.node(id)
    else {
        return None;
    };
    let Node::Member {
        obj,
        optional: false,
        prop,
    } = *ast.node(skip_parens(ast, callee))
    else {
        return None;
    };
    let args = ast.list(args);
    if ident_name(ast, prop) != Some(name)
        || args.len() < min
        || args.len() > max
        || args
            .iter()
            .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
    {
        return None;
    }
    Some((obj, args))
}

/// The query of a manual pipeline at `id` (through TS wrappers): `(map call, query)`.
fn pipeline(cx: &Ctx<'_, '_>, id: NodeId) -> Option<(NodeId, NodeId)> {
    let ast = cx.ast();
    let map = skip_wrappers(ast, id);
    if matches!(
        ast.node(map),
        Node::Call {
            type_args: Some(_),
            ..
        }
    ) {
        return None;
    }
    let (split, &[callback]) = method_call(ast, map, "map", 1, 1)? else {
        return None;
    };
    let (query, &[sep]) = method_call(ast, skip_wrappers(ast, split), "split", 1, 1)? else {
        return None;
    };
    if static_string(ast, sep).as_deref() != Some("&")
        || is_known_non_string(cx, query)
        || static_value_no_side_effects(cx, query).is_some_and(|v| !matches!(v, Static::Str(_)))
    {
        return None;
    }
    let (flags, params, body) = match *ast.node(callback) {
        Node::Arrow {
            flags,
            params,
            body,
            ..
        } => (flags, params, body),
        Node::FunctionExpr {
            flags,
            params,
            body: Some(body),
            ..
        } => (flags, params, body),
        _ => return None,
    };
    if flags.intersects(FnFlags::ASYNC | FnFlags::GENERATOR) {
        return None;
    }
    let &[param] = ast.list(params) else {
        return None;
    };
    let Node::Param {
        pattern,
        rest: false,
        init: None,
        ..
    } = *ast.node(param)
    else {
        return None;
    };
    if !matches!(ast.node(pattern), Node::Ident { .. }) {
        return None;
    }
    let returned = match *ast.node(body) {
        Node::Block { stmts } => match ast.list(stmts) {
            &[only] => match *ast.node(only) {
                Node::Return { arg: Some(arg) } => arg,
                _ => return None,
            },
            _ => return None,
        },
        _ => body,
    };
    let (part, args) = method_call(ast, skip_wrappers(ast, returned), "split", 1, 2)?;
    let part = skip_wrappers(ast, part);
    if !matches!(ast.node(part), Node::Ident { .. })
        || ident_name(ast, part) != ident_name(ast, pattern)
        || static_string(ast, args[0]).as_deref() != Some("=")
    {
        return None;
    }
    if let Some(&limit) = args.get(1)
        && !matches!(static_value_no_side_effects(cx, limit), Some(Static::Num(n)) if n == 2.0)
    {
        return None;
    }
    Some((map, query))
}

/// `URLSearchParams` at `node` is the global or the `url` module's.
fn url_search_params_available(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    if is_global_name_available(cx, "URLSearchParams", node) {
        return true;
    }
    let sem = cx.semantic();
    let ast = cx.ast();
    let Some(sym) = sem.lookup(sem.enclosing_scope(node), "URLSearchParams") else {
        return true;
    };
    sem.symbol(sym).decls.iter().any(|d| {
        if d.kind != DeclKind::ImportNamed {
            return false;
        }
        let Node::ImportDecl {
            type_only: false,
            source,
            ..
        } = *ast.node(d.parent)
        else {
            return false;
        };
        let Node::ImportSpecifier {
            type_only: false,
            imported,
            local,
        } = *ast.node(d.node)
        else {
            return false;
        };
        let imported = imported.unwrap_or(local);
        matches!(*ast.node(source), Node::StringLit { value } if matches!(ast.atom(value), "url" | "node:url"))
            && matches!(ast.node(imported), Node::Ident { .. })
            && ident_name(ast, imported) == Some("URLSearchParams")
    })
}

enum Wrapper {
    FromEntries,
    Map,
    Params,
}

/// A real wrapper at `node`: `(wrapper kind, its one argument)`.
fn wrapper(cx: &Ctx<'_, '_>, node: NodeId) -> Option<(Wrapper, NodeId)> {
    let ast = cx.ast();
    match *ast.node(node) {
        Node::Call {
            type_args: Some(_), ..
        }
        | Node::New {
            type_args: Some(_), ..
        } => None,
        Node::Call { .. } => {
            let (obj, &[arg]) = method_call(ast, node, "fromEntries", 1, 1)? else {
                return None;
            };
            (matches!(ast.node(obj), Node::Ident { .. })
                && ident_name(ast, obj) == Some("Object")
                && is_global_name_available(cx, "Object", node))
            .then_some((Wrapper::FromEntries, arg))
        }
        Node::New { callee, args, .. } => {
            let &[arg] = ast.list(args) else {
                return None;
            };
            if matches!(ast.node(arg), Node::Spread { .. })
                || !matches!(ast.node(callee), Node::Ident { .. })
            {
                return None;
            }
            match ident_name(ast, callee)? {
                "Map" if is_global_name_available(cx, "Map", node) => Some((Wrapper::Map, arg)),
                "URLSearchParams" => Some((Wrapper::Params, arg)),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Anything named like a wrapper whose first argument is `node` (with its TS wrappers).
fn is_wrapped(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let mut expr = node;
    let parent = loop {
        let Some(p) = cx.parent(expr) else {
            return false;
        };
        match *ast.node(p) {
            Node::Paren { .. }
            | Node::As { .. }
            | Node::Satisfies { .. }
            | Node::NonNull { .. }
            | Node::TypeAssertion { .. } => expr = p,
            _ => break p,
        }
    };
    match *ast.node(parent) {
        Node::Call {
            callee,
            args,
            type_args: None,
            ..
        } => {
            let args = ast.list(args);
            if args.len() != 1 || args[0] != expr {
                return false;
            }
            let (obj, named) = match *ast.node(skip_parens(ast, callee)) {
                Node::Member { obj, prop, .. } => {
                    (obj, ident_name(ast, prop) == Some("fromEntries"))
                }
                Node::Index { obj, index, .. } => (
                    obj,
                    static_string(ast, index).as_deref() == Some("fromEntries"),
                ),
                _ => return false,
            };
            named
                && matches!(ast.node(obj), Node::Ident { .. })
                && ident_name(ast, obj) == Some("Object")
        }
        Node::New {
            callee,
            args,
            type_args: None,
            ..
        } => {
            let args = ast.list(args);
            args.len() == 1
                && args[0] == expr
                && matches!(ast.node(callee), Node::Ident { .. })
                && matches!(ident_name(ast, callee), Some("Map" | "URLSearchParams"))
        }
        _ => false,
    }
}

fn report(cx: &mut Ctx<'_, '_>, node: NodeId, replacement: String, kept: &[Span]) {
    if !url_search_params_available(cx, node) {
        return;
    }
    let span = cx.span(node);
    let safe = !would_remove_comments(cx, span, kept);
    let r = cx
        .report(
            span,
            "Prefer `URLSearchParams` over manually splitting query strings.",
        )
        .id("prefer-url-search-parameters/error");
    if safe {
        r.suggest(
            "prefer-url-search-parameters/suggestion",
            format!("Use `{replacement}`."),
            Fix::replace(span, replacement),
        );
    }
}

impl Rule for PreferUrlSearchParameters {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call, NodeKind::New]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        if let Some((kind, arg)) = wrapper(cx, node)
            && let Some((_, query)) = pipeline(cx, arg)
        {
            let params = format!("new URLSearchParams({})", call_argument_text(cx, query));
            let inner = match kind {
                Wrapper::FromEntries | Wrapper::Map => params,
                Wrapper::Params => call_argument_text(cx, query),
            };
            let span = cx.span(node);
            let arg_span = paren_span(cx, arg);
            let src = cx.src();
            let replacement = format!(
                "{}{}{}",
                &src[span.start as usize..arg_span.start as usize],
                inner,
                &src[arg_span.end as usize..span.end as usize]
            );
            let kept = [
                Span::new(span.start, arg_span.start),
                Span::new(arg_span.end, span.end),
                cx.span(query),
            ];
            report(cx, node, replacement, &kept);
            return;
        }
        if !matches!(cx.node(node), Node::Call { .. }) || is_wrapped(cx, node) {
            return;
        }
        let Some((_, query)) = pipeline(cx, node) else {
            return;
        };
        let replacement = format!("new URLSearchParams({})", call_argument_text(cx, query));
        let kept = [cx.span(query)];
        report(cx, node, replacement, &kept);
    }
}
