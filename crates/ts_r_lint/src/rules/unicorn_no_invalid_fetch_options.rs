//! `unicorn/no-invalid-fetch-options`: `fetch(url, {body})` and `new Request(url, {body})`
//! (any binding named `fetch` / `Request`, the options an object literal through TS
//! wrappers) may not send a body with the `GET` (the default) or `HEAD` method. The last
//! `body` property counts, unless its value is `undefined`, `null` or `void x`; the last
//! `method` counts, read as a static string, and a spread after it makes it unknown.
//! Reported at the `body` key.

use super::prelude::*;
use super::unicorn_static::{Static, static_value_control_flow};
use crate::ast_util::key_name;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-invalid-fetch-options",
        "Disallow invalid options in `fetch()` and `new Request()`.",
        NO_PRESET,
        Category::Correctness,
    ),
    create: create_default::<NoInvalidFetchOptions>,
};

#[derive(Debug, Default)]
struct NoInvalidFetchOptions;

fn unwrap_ts(ast: &Ast, mut id: NodeId) -> NodeId {
    for _ in 0..10_000 {
        match *ast.node(id) {
            Node::Paren { expr }
            | Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::NonNull { expr }
            | Node::TypeAssertion { expr, .. }
            | Node::Instantiation { expr, .. } => id = expr,
            _ => return id,
        }
    }
    id
}

/// The key and value of an object literal member that ESTree calls a `Property`.
fn property(ast: &Ast, id: NodeId) -> Option<(NodeId, NodeId)> {
    match *ast.node(id) {
        Node::Property { key, value, .. } => Some((key, value)),
        Node::ShorthandProp { name, .. } => Some((name, name)),
        Node::MethodDef { key, value, .. } => Some((key, value)),
        _ => None,
    }
}

impl Rule for NoInvalidFetchOptions {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call, NodeKind::New]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let (callee, args, name) = match *cx.node(node) {
            Node::Call {
                callee,
                optional: false,
                args,
                ..
            } => (callee, args, "fetch"),
            Node::New { callee, args, .. } => (callee, args, "Request"),
            _ => return,
        };
        let callee = skip_parens(ast, callee);
        if !matches!(ast.node(callee), Node::Ident { .. }) || ident_name(ast, callee) != Some(name)
        {
            return;
        }
        let args = ast.list(args);
        if args.len() < 2 {
            return;
        }
        let Node::ObjectLit { props } = *ast.node(unwrap_ts(ast, args[1])) else {
            return;
        };
        let props = ast.list(props);
        let src = cx.src();
        let named = |p: NodeId, want: &str| {
            property(ast, p).is_some_and(|(k, _)| key_name(ast, src, k).as_deref() == Some(want))
        };
        let Some(&body) = props.iter().rev().find(|&&p| named(p, "body")) else {
            return;
        };
        let Some((body_key, body_value)) = property(ast, body) else {
            return;
        };
        let v = skip_parens(ast, body_value);
        if is_undefined(ast, v)
            || matches!(
                ast.node(v),
                Node::NullLit {}
                    | Node::Unary {
                        op: UnaryOp::Void,
                        ..
                    }
            )
        {
            return;
        }
        let method_index = props.iter().rposition(|&p| named(p, "method"));
        let after = method_index.map_or(0, |i| i + 1);
        if props.iter().enumerate().any(|(i, &p)| {
            (method_index.is_none() || i >= after) && matches!(ast.node(p), Node::Spread { .. })
        }) {
            return;
        }
        let method = match method_index {
            None => "GET".to_owned(),
            Some(i) => {
                let Some((_, value)) = property(ast, props[i]) else {
                    return;
                };
                let Some(Static::Str(m)) = static_value_control_flow(cx, value) else {
                    return;
                };
                let m = m.to_uppercase();
                if m != "GET" && m != "HEAD" {
                    return;
                }
                m
            }
        };
        let key = match *ast.node(body_key) {
            Node::ComputedKey { expr } => expr,
            _ => body_key,
        };
        let span = cx.span(key);
        cx.report(
            span,
            format!("\"body\" is not allowed when method is \"{method}\"."),
        );
    }
}
