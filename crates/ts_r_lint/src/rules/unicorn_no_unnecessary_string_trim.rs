//! `unicorn/no-unnecessary-string-trim`: `s.trim().startsWith(x)` only needs `trimStart()`
//! (and `endsWith` only `trimEnd()`) when the search string, if given, is static and has no
//! whitespace on the side the narrower trim keeps. Receivers known not to be strings, and
//! zod's `z.string()` (whose `trim()` is a schema method), are left alone.

use super::prelude::*;
use super::unicorn_ast::{is_js_space, method_call};
use super::unicorn_static::{Static, const_definition, static_value_no_side_effects};
use super::unicorn_types::is_known_non_string;
use super::unicorn_util::{has_optional_chain, skip_wrappers};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-unnecessary-string-trim",
        "Disallow `String#trim()` before `String#startsWith()` or `String#endsWith()`.",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<NoUnnecessaryStringTrim>,
};

#[derive(Debug, Default)]
struct NoUnnecessaryStringTrim;

/// An array, object, function or class expression, or a static non-string, directly or as
/// a `const` initializer.
fn is_static_non_string(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let mut node = skip_wrappers(ast, id);
    if let Some((_, init)) = const_definition(cx, node) {
        node = skip_wrappers(ast, init);
    }
    if matches!(
        ast.node(node),
        Node::ArrayLit { .. }
            | Node::Arrow { .. }
            | Node::ClassExpr { .. }
            | Node::FunctionExpr { .. }
            | Node::ObjectLit { .. }
    ) {
        return true;
    }
    static_value_no_side_effects(cx, node).is_some_and(|v| !matches!(v, Static::Str(_)))
}

/// `z.string()` with `z` a runtime import of zod's `z` export or a namespace import of zod.
fn is_zod_string_call(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let call = skip_wrappers(ast, id);
    let Some(m) = method_call(ast, call) else {
        return false;
    };
    if m.name != "string" || !m.args.is_empty() || !m.plain() {
        return false;
    }
    if !matches!(ast.node(m.obj), Node::Ident { .. }) {
        return false;
    }
    let sem = cx.semantic();
    let Some(s) = sem.symbol_of_ident(m.obj) else {
        return false;
    };
    sem.symbol(s).decls.iter().any(|d| {
        let mut spec = d.node;
        if matches!(ast.node(spec), Node::Ident { .. }) {
            match cx.parent(spec) {
                Some(p) => spec = p,
                None => return false,
            }
        }
        let is_z = match *ast.node(spec) {
            Node::ImportNamespaceSpecifier { .. } => true,
            Node::ImportSpecifier {
                type_only: false,
                imported,
                local,
            } => {
                let name = imported.unwrap_or(local);
                match ast.node(name) {
                    Node::StringLit { value } => ast.atom(*value) == "z",
                    _ => ident_name(ast, name) == Some("z"),
                }
            }
            _ => false,
        };
        is_z && cx.parent(spec).is_some_and(|decl| {
            matches!(*ast.node(decl), Node::ImportDecl { type_only: false, source, .. }
                if matches!(ast.node(source), Node::StringLit { value } if ast.atom(*value) == "zod"))
        })
    })
}

impl Rule for NoUnnecessaryStringTrim {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some(outer) = method_call(ast, node) else {
            return;
        };
        let (method, replacement) = match outer.name {
            "startsWith" => ("startsWith", "trimStart"),
            "endsWith" => ("endsWith", "trimEnd"),
            _ => return,
        };
        if !outer.plain()
            || outer.args.len() > 1
            || outer
                .args
                .iter()
                .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
        {
            return;
        }
        let trim_call = skip_parens(ast, outer.obj);
        let Some(trim) = method_call(ast, trim_call) else {
            return;
        };
        if trim.name != "trim"
            || !trim.args.is_empty()
            || !trim.plain()
            || has_optional_chain(ast, node)
        {
            return;
        }
        let receiver = trim.obj;
        if is_static_non_string(cx, receiver)
            || is_known_non_string(cx, receiver)
            || is_zod_string_call(cx, receiver)
        {
            return;
        }
        if let Some(&search) = outer.args.first() {
            let Some(text) = static_value_no_side_effects(cx, search).and_then(|v| match v {
                Static::Undefined
                | Static::Null
                | Static::Bool(_)
                | Static::Num(_)
                | Static::Str(_)
                | Static::BigInt(_) => v.to_js_string(),
                _ => None,
            }) else {
                return;
            };
            let safe = if method == "startsWith" {
                !text.ends_with(is_js_space)
            } else {
                !text.starts_with(is_js_space)
            };
            if !safe {
                return;
            }
        }
        let prop = trim.prop;
        let fix = Fix::replace(cx.span(prop), replacement);
        cx.report_node(
            prop,
            format!("Prefer `String#{replacement}()` before `String#{method}()`."),
        )
        .id("no-unnecessary-string-trim")
        .fix(fix);
    }
}
