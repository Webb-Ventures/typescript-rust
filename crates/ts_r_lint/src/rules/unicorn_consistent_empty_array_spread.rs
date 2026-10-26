//! `unicorn/consistent-empty-array-spread`: in `[...(test ? a : b)]`, an empty string
//! opposite an array (a literal, or a side-effect-free static array value) should be `[]`,
//! and an empty array opposite a static string should be `''`. The empty side is reported
//! and replaced (not when it holds a comment).

use super::prelude::*;
use super::unicorn_static::{Static, static_value_no_side_effects};
use super::unicorn_util_h1::static_string;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/consistent-empty-array-spread",
        "Prefer consistent types when spreading a ternary in an array literal.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<ConsistentEmptyArraySpread>,
};

#[derive(Debug, Default)]
struct ConsistentEmptyArraySpread;

fn is_empty_string(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    static_string(cx.ast(), id).is_some_and(|s| s.is_empty())
}

fn is_array(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    matches!(cx.node(id), Node::ArrayLit { .. })
        || matches!(static_value_no_side_effects(cx, id), Some(Static::Array(_)))
}

fn is_empty_array(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    matches!(*cx.node(id), Node::ArrayLit { elements } if cx.ast().list(elements).is_empty())
}

fn is_string(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    matches!(static_value_no_side_effects(cx, id), Some(Static::Str(_)))
}

type Pred = fn(&Ctx<'_, '_>, NodeId) -> bool;

/// (empty side, other side, other description, replacement description, replacement).
const CASES: &[(Pred, Pred, &str, &str, &str)] = &[
    (is_empty_string, is_array, "an array", "array", "[]"),
    (is_empty_array, is_string, "a string", "string", "''"),
];

impl Rule for ConsistentEmptyArraySpread {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::ArrayLit]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::ArrayLit { elements } = *cx.node(node) else {
            return;
        };
        for &el in ast.list(elements) {
            let Node::Spread { arg } = *ast.node(el) else {
                continue;
            };
            let Node::Conditional { cons, alt, .. } = *ast.node(skip_parens(ast, arg)) else {
                continue;
            };
            let (cons, alt) = (skip_parens(ast, cons), skip_parens(ast, alt));
            let found = CASES
                .iter()
                .find_map(|&(empty, other, desc, repl_desc, repl)| {
                    if empty(cx, cons) && other(cx, alt) {
                        Some((cons, "alternate", desc, repl_desc, repl))
                    } else if empty(cx, alt) && other(cx, cons) {
                        Some((alt, "consequent", desc, repl_desc, repl))
                    } else {
                        None
                    }
                });
            let Some((problem, position, desc, repl_desc, repl)) = found else {
                continue;
            };
            let span = cx.span(problem);
            let fix = ast
                .comments_in(span)
                .iter()
                .all(|c| c.span.end > span.end)
                .then(|| Fix::replace(span, repl));
            cx.report_node(
                problem,
                format!("Prefer using empty {repl_desc} since the {position} is {desc}."),
            )
            .id("consistent-empty-array-spread")
            .fix_opt(fix);
        }
    }
}
