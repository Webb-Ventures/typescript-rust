//! `no-implied-eval`: no string code passed to `setTimeout`, `setInterval` or `execScript`,
//! called directly or through `global`, `window`, `globalThis` or `self`.

use super::prelude::*;
use super::unicorn_static::{Static, static_value};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-implied-eval",
        "Disallow the use of `eval()`-like methods",
        NO_PRESET,
        Category::Suspicious,
    ),
    create: create_default::<NoImpliedEval>,
};

#[derive(Debug, Default)]
struct NoImpliedEval;

const EVAL_LIKE: &[&str] = &["setTimeout", "setInterval", "execScript"];
const GLOBALS: &[&str] = &["global", "window", "globalThis", "self"];

/// A string literal, a template, or a `+` with one of those on either side.
fn is_evaluated_string(ast: &ts_r_ast::Ast, id: NodeId, depth: u32) -> bool {
    match *ast.node(skip_parens(ast, id)) {
        Node::StringLit { .. } | Node::TemplateLit { .. } => true,
        Node::Binary {
            op: BinaryOp::Add,
            left,
            right,
        } if depth < 100 => {
            is_evaluated_string(ast, left, depth + 1) || is_evaluated_string(ast, right, depth + 1)
        }
        _ => false,
    }
}

/// The static name of a `a.b` / `a['b']` member.
fn member_name<'a>(cx: &Ctx<'a, '_>, id: NodeId) -> Option<(NodeId, std::borrow::Cow<'a, str>)> {
    let ast = cx.ast();
    match *ast.node(id) {
        Node::Member { obj, prop, .. } => Some((obj, ident_name(ast, prop)?.into())),
        Node::Index { obj, index, .. } => Some((
            obj,
            static_value_string(ast, cx.src(), skip_parens(ast, index))?,
        )),
        _ => None,
    }
}

impl Rule for NoImpliedEval {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Call { callee, args, .. } = *cx.node(node) else {
            return;
        };
        let callee = skip_parens(ast, callee);
        let name = if let Some(n) = ident_name(ast, callee)
            && matches!(cx.node(callee), Node::Ident { .. })
        {
            if !EVAL_LIKE.contains(&n) || !cx.is_global_ref(callee, n) {
                return;
            }
            n.to_string()
        } else {
            // `global.global['setTimeout'](..)`: members of one global object's own name.
            let Some((mut obj, name)) = member_name(cx, callee) else {
                return;
            };
            if !EVAL_LIKE.contains(&name.as_ref()) {
                return;
            }
            let mut names = Vec::new();
            for _ in 0..1000 {
                obj = skip_parens(ast, obj);
                match member_name(cx, obj) {
                    Some((o, n)) => {
                        names.push(n);
                        obj = o;
                    }
                    None => break,
                }
            }
            let Some(base) = ident_name(ast, obj) else {
                return;
            };
            if !GLOBALS.contains(&base)
                || names.iter().any(|n| n != base)
                || !cx.is_global_ref(obj, base)
            {
                return;
            }
            name.into_owned()
        };
        let Some(&arg) = ast.list(args).first() else {
            return;
        };
        let is_string = matches!(static_value(cx, arg), Some(Static::Str(_)))
            || is_evaluated_string(ast, arg, 0);
        if !is_string {
            return;
        }
        if name == "execScript" {
            cx.report_node(node, "Implied eval. Do not use execScript().")
                .id("execScript");
        } else {
            cx.report_node(
                node,
                "Implied eval. Consider passing a function instead of a string.",
            )
            .id("impliedEval");
        }
    }
}
