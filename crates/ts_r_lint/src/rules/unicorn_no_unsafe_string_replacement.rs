//! `unicorn/no-unsafe-string-replacement`: the replacement of `x.replace(a, b)` /
//! `x.replaceAll(a, b)` (exactly two non-spread arguments) is read for `$` patterns, so a
//! non-literal one may inject them. Allowed (through TS wrappers): static strings and
//! templates, `` String.raw`...` `` without substitutions (the global `String`),
//! `"lit".repeat(n)` of a `$`-free literal, functions, and object literals (directly or as a
//! `const` initializer) of plain non-computed `key: value` / shorthand members other than
//! `__proto__`, `toString` and `valueOf`. Receivers known not to be strings are skipped.

use super::prelude::*;
use super::unicorn_ast::method_call;
use super::unicorn_static::const_definition;
use super::unicorn_text::static_string_value;
use super::unicorn_types::is_known_non_string;
use crate::ast_util::key_name;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-unsafe-string-replacement",
        "Disallow non-literal replacement values in `String#replace()` and `String#replaceAll()`.",
        NO_PRESET,
        Category::Correctness,
    ),
    create: create_default::<NoUnsafeStringReplacement>,
};

#[derive(Debug, Default)]
struct NoUnsafeStringReplacement;

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

fn is_allowed(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let id = unwrap_ts(ast, id);
    if static_string_value(ast, id).is_some() {
        return true;
    }
    match *ast.node(id) {
        Node::Arrow { .. } | Node::FunctionExpr { .. } => true,
        Node::TaggedTemplate { tag, quasi, .. } => {
            let tag = skip_parens(ast, tag);
            static_string_value(ast, quasi).is_some()
                && matches!(*ast.node(tag), Node::Member { obj, optional: false, prop }
                    if ident_name(ast, prop) == Some("raw")
                        && matches!(ast.node(skip_parens(ast, obj)), Node::Ident { .. })
                        && ident_name(ast, skip_parens(ast, obj)) == Some("String")
                        && cx.semantic().refers_to_global(skip_parens(ast, obj)))
        }
        _ => method_call(ast, id).is_some_and(|m| {
            m.name == "repeat"
                && m.plain()
                && static_string_value(ast, unwrap_ts(ast, m.obj)).is_some_and(|s| !s.contains('$'))
        }),
    }
}

fn is_plain_object(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let id = unwrap_ts(ast, id);
    let value = const_definition(cx, id).map_or(id, |(_, init)| init);
    let Node::ObjectLit { props } = *ast.node(unwrap_ts(ast, value)) else {
        return false;
    };
    let src = cx.src();
    ast.list(props).iter().all(|&p| {
        let key = match *ast.node(p) {
            Node::Property { key, .. } if !matches!(ast.node(key), Node::ComputedKey { .. }) => key,
            Node::ShorthandProp { name, .. } => name,
            _ => return false,
        };
        !matches!(
            key_name(ast, src, key).as_deref(),
            Some("__proto__" | "toString" | "valueOf")
        )
    })
}

impl Rule for NoUnsafeStringReplacement {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some(call) = method_call(ast, node) else {
            return;
        };
        if !matches!(call.name, "replace" | "replaceAll")
            || call.args.len() != 2
            || call
                .args
                .iter()
                .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
        {
            return;
        }
        let replacement = call.args[1];
        let method = call.name;
        if is_allowed(cx, replacement)
            || is_plain_object(cx, replacement)
            || is_known_non_string(cx, call.obj)
        {
            return;
        }
        let span = cx.span(skip_parens(ast, replacement));
        cx.report(
            span,
            format!("Do not use a non-literal replacement value with `String#{method}()`."),
        );
    }
}
