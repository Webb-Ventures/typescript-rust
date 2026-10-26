//! `unicorn/prefer-flat-math-min-max`: `Math.max(Math.max(a, b), c)` is `Math.max(a, b, c)`.
//! The outermost call of a nest of plain `Math.min` (or `Math.max`) calls is reported; the
//! fix inlines the arguments of every nested call of the same method (each argument keeps
//! its parentheses) unless a comment would be lost.

use super::prelude::*;
use super::unicorn_ast::method_call;
use super::unicorn_text::{paren_span, paren_text, would_remove_comments};
use super::unicorn_util::{space_around_keyword, with_edits};
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-flat-math-min-max",
        "Prefer flat `Math.min()` and `Math.max()` calls over nested calls.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<PreferFlatMathMinMax>,
};

#[derive(Debug, Default)]
struct PreferFlatMathMinMax;

/// `Math.min(...)` or `Math.max(...)` without optional chaining: the method.
fn math_method(ast: &Ast, id: NodeId) -> Option<&'static str> {
    let call = method_call(ast, skip_parens(ast, id)).filter(|m| m.plain())?;
    if ident_name(ast, skip_parens(ast, call.obj)) != Some("Math")
        || !matches!(ast.node(skip_parens(ast, call.obj)), Node::Ident { .. })
    {
        return None;
    }
    match call.name {
        "min" => Some("min"),
        "max" => Some("max"),
        _ => None,
    }
}

fn args(ast: &Ast, call: NodeId) -> &[NodeId] {
    match *ast.node(skip_parens(ast, call)) {
        Node::Call { args, .. } => ast.list(args),
        _ => &[],
    }
}

fn flatten(ast: &Ast, call: NodeId, method: &str, out: &mut Vec<NodeId>, depth: u32) {
    for &a in args(ast, call) {
        if depth < 64 && math_method(ast, a) == Some(method) {
            flatten(ast, skip_parens(ast, a), method, out, depth + 1);
        } else {
            out.push(a);
        }
    }
}

impl Rule for PreferFlatMathMinMax {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some(method) = math_method(ast, node) else {
            return;
        };
        if !args(ast, node)
            .iter()
            .any(|&a| math_method(ast, a) == Some(method))
        {
            return;
        }
        let parent = parent_skip_parens(ast, cx.file.parents, node);
        if parent.is_some_and(|p| {
            math_method(ast, p) == Some(method)
                && args(ast, p).iter().any(|&a| skip_parens(ast, a) == node)
        }) {
            return;
        }
        let Node::Call { callee, .. } = *ast.node(node) else {
            return;
        };
        let callee = skip_parens(ast, callee);
        let mut flat = Vec::new();
        flatten(ast, node, method, &mut flat, 0);
        let span = cx.span(node);
        let mut kept = vec![cx.span(callee)];
        kept.extend(flat.iter().map(|&a| paren_span(cx, a)));
        let fix = (!would_remove_comments(cx, span, &kept)).then(|| {
            let text: Vec<&str> = flat.iter().map(|&a| paren_text(cx, a)).collect();
            with_edits(
                Fix::replace(span, format!("{}({})", cx.text(callee), text.join(", "))),
                space_around_keyword(cx, node),
            )
        });
        cx.report(
            span,
            format!("Prefer a flat `Math.{method}()` call instead of nested calls."),
        )
        .id("prefer-flat-math-min-max")
        .fix_opt(fix);
    }
}
