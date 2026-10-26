//! `unicorn/no-invalid-response-options`: a known `status` outside 200–599 in the options of
//! `new Response(body, options)` or `Response.json(data, options)`; a body other than
//! `null`/`undefined` with a null-body status (204, 205, 304), for `new Response` on the body
//! (suggesting `undefined` for a primitive literal value without comments) and for
//! `Response.json` on the status; a `Response.redirect(url, status)` status other than 301,
//! 302, 303, 307 or 308.
//!
//! The options must be an object literal whose last property that could set `status` is a
//! plain `status: value` (a spread, accessor or method, or an unknown computed key, makes it
//! unknown). Values are only trusted when the expression (or the initializer of the `const`
//! it names) is a literal, object, array, `undefined`/`NaN`/`Infinity`, a unary operator on
//! one of those, or a template of literals; then the status converts like Web IDL's
//! `unsigned short` (non-finite to 0, truncated, modulo 65536), with `undefined` and `void`
//! giving the default (200, or 302 for redirects).

use super::prelude::*;
use super::unicorn_static::{Static, const_definition, static_value_control_flow};
use super::unicorn_text::has_comment_in;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-invalid-response-options",
        "Disallow invalid options in `new Response()`, `Response.json()`, and `Response.redirect()`.",
        NO_PRESET,
        Category::Correctness,
    )
    .suggestions(),
    create: create_default::<NoInvalidResponseOptions>,
};

#[derive(Debug, Default)]
struct NoInvalidResponseOptions;

/// `id` without parentheses and TS expression wrappers.
fn unwrap(ast: &Ast, id: NodeId) -> NodeId {
    super::unicorn_conditions::unwrap_ts_and_parens(ast, id)
}

fn is_primitive_literal(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(unwrap(ast, id)),
        Node::StringLit { .. }
            | Node::NumberLit { .. }
            | Node::BoolLit { .. }
            | Node::NullLit {}
            | Node::BigIntLit { .. }
    )
}

fn is_primitive_global(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ident_name(ast, unwrap(ast, id)),
        Some("undefined" | "NaN" | "Infinity")
    )
}

/// A static value, trusted only for simple expressions (no user-defined coercion).
fn simple_static(cx: &Ctx<'_, '_>, id: NodeId) -> Option<Static> {
    let ast = cx.ast();
    let node = unwrap(ast, id);
    let expr = unwrap(
        ast,
        const_definition(cx, node).map_or(node, |(_, init)| init),
    );
    let trusted = match *ast.node(expr) {
        Node::StringLit { .. }
        | Node::NumberLit { .. }
        | Node::BoolLit { .. }
        | Node::NullLit {}
        | Node::BigIntLit { .. }
        | Node::RegexLit { .. }
        | Node::ObjectLit { .. }
        | Node::ArrayLit { .. } => true,
        Node::Unary { arg, .. } => is_primitive_literal(ast, arg) || is_primitive_global(ast, arg),
        Node::TemplateLit { parts } => ast.list(parts).iter().all(|&p| {
            matches!(ast.node(p), Node::TemplateElement { .. }) || is_primitive_literal(ast, p)
        }),
        _ => is_primitive_global(ast, expr),
    };
    if !trusted {
        return None;
    }
    static_value_control_flow(cx, node)
}

/// The value of the effective `status` property of an inline options object.
fn status_value(cx: &Ctx<'_, '_>, options: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let Node::ObjectLit { props } = *ast.node(unwrap(ast, options)) else {
        return None;
    };
    for &p in ast.list(props).iter().rev() {
        let (name, value) = match *ast.node(p) {
            Node::Property { key, value, .. } => {
                let name = match *ast.node(key) {
                    Node::ComputedKey { expr } => match simple_static(cx, expr)? {
                        Static::Str(s) => s,
                        v @ Static::Num(_) => v.to_js_string()?,
                        _ => return None,
                    },
                    Node::Ident { name } | Node::StringLit { value: name } => {
                        ast.atom(name).to_owned()
                    }
                    _ => cx.text(key).to_owned(),
                };
                (name, value)
            }
            Node::ShorthandProp {
                name, init: None, ..
            } => (ident_name(ast, name)?.to_owned(), name),
            _ => return None,
        };
        if name == "status" {
            return Some(value);
        }
    }
    None
}

fn is_undefined_value(ast: &Ast, id: NodeId) -> bool {
    let id = unwrap(ast, id);
    ident_name(ast, id) == Some("undefined")
        || matches!(
            ast.node(id),
            Node::Unary {
                op: UnaryOp::Void,
                ..
            }
        )
}

/// The status after `unsigned short` conversion, when known.
fn status_of(cx: &Ctx<'_, '_>, id: NodeId, default: u32) -> Option<u32> {
    let ast = cx.ast();
    if is_undefined_value(ast, id) {
        return Some(default);
    }
    let n = match simple_static(cx, id)? {
        Static::Undefined => return Some(default),
        v @ (Static::Null | Static::Bool(_) | Static::Num(_) | Static::Str(_)) => v.to_number()?,
        _ => return None,
    };
    if !n.is_finite() {
        return Some(0);
    }
    let t = n.trunc().rem_euclid(65_536.0);
    // `t` is a whole number in 0..65536.
    Some(t as u32)
}

fn body_problem(cx: &mut Ctx<'_, '_>, body: NodeId, status: u32) {
    let ast = cx.ast();
    let inner = unwrap(ast, body);
    let value = simple_static(cx, inner);
    let non_nullish = matches!(
        ast.node(inner),
        Node::TemplateLit { .. }
            | Node::ObjectLit { .. }
            | Node::ArrayLit { .. }
            | Node::New { .. }
    );
    if !non_nullish && value.as_ref().is_none_or(Static::is_nullish) {
        return;
    }
    let reported = skip_parens(ast, body);
    let span = cx.span(reported);
    let suggest = matches!(
        value,
        Some(Static::Str(_) | Static::Num(_) | Static::Bool(_) | Static::BigInt(_))
    ) && !has_comment_in(cx, span);
    let r = cx
        .report(
            span,
            format!("A response with status {status} cannot have a body."),
        )
        .id("body-with-null-body-status");
    if suggest {
        r.suggest(
            "remove-body",
            "Replace the response body with `undefined`.",
            Fix::replace(span, "undefined"),
        );
    }
}

fn response_problem(cx: &mut Ctx<'_, '_>, args: &[NodeId], is_new: bool) {
    let ast = cx.ast();
    let Some(value) = status_value(cx, args[1]) else {
        return;
    };
    let Some(status) = status_of(cx, value, 200) else {
        return;
    };
    let reported = skip_parens(ast, value);
    if !(200..=599).contains(&status) {
        cx.report_node(reported, "Response status must be in the range 200 to 599.")
            .id("invalid-status");
        return;
    }
    if !matches!(status, 204 | 205 | 304) {
        return;
    }
    if is_new {
        body_problem(cx, args[0], status);
    } else {
        cx.report_node(
            reported,
            format!("A response with status {status} cannot have a body."),
        )
        .id("body-with-null-body-status");
    }
}

impl Rule for NoInvalidResponseOptions {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::New, NodeKind::Call]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let has_spread = |args: &[NodeId]| {
            args.iter()
                .take(2)
                .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
        };
        match *cx.node(node) {
            Node::New { callee, args, .. } => {
                let args = ast.list(args);
                if ident_name(ast, skip_parens(ast, callee)) != Some("Response")
                    || args.len() < 2
                    || has_spread(args)
                {
                    return;
                }
                response_problem(cx, args, true);
            }
            Node::Call {
                callee,
                optional: false,
                args,
                ..
            } => {
                let Node::Member {
                    obj,
                    optional: false,
                    prop,
                } = *ast.node(skip_parens(ast, callee))
                else {
                    return;
                };
                let args = ast.list(args);
                if ident_name(ast, skip_parens(ast, obj)) != Some("Response")
                    || args.len() < 2
                    || has_spread(args)
                {
                    return;
                }
                match ident_name(ast, prop) {
                    Some("json") if matches!(ast.node(prop), Node::Ident { .. }) => {
                        response_problem(cx, args, false);
                    }
                    Some("redirect") if matches!(ast.node(prop), Node::Ident { .. }) => {
                        let Some(status) = status_of(cx, args[1], 302) else {
                            return;
                        };
                        if !matches!(status, 301 | 302 | 303 | 307 | 308) {
                            let reported = skip_parens(ast, args[1]);
                            cx.report_node(
                                reported,
                                "`Response.redirect()` status must be 301, 302, 303, 307, or 308.",
                            )
                            .id("invalid-redirect-status");
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::config::{ConfigFile, ConfigKind, ResolveOptions, objects_from_eslint_json};

    fn lint(src: &str) -> usize {
        let reg = crate::Registry::global();
        let json = serde_json::json!([{ "files": ["**/*.js"], "rules": {
            "unicorn/no-invalid-response-options": "error" } }]);
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
    fn statuses_and_malformed_input() {
        assert_eq!(lint("new Response('', {status: 1e400});"), 1);
        assert_eq!(lint("new Response('', {status: -65332});"), 1);
        assert_eq!(lint("new Response(undefined, {status: 65740});"), 0);
        assert_eq!(lint("Response.redirect(url, 1e300);"), 1);
        for src in [
            "new Response('', {status:",
            "Response.json(x, {",
            "Response.redirect(u,",
        ] {
            let _ = lint(src);
        }
    }
}
