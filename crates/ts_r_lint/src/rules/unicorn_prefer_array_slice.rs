//! `unicorn/prefer-array-slice`: `x.splice(i)` (plain, one argument) whose result is only
//! read, by a non-optional computed member access that isn't written (`x.splice(1)[0]`) or
//! by `.at(n)`, on a receiver not known to be a non-array (class syntax and `extends`
//! followed; a union with a non-array member counts as non-array). Suggests `slice`.

use super::prelude::*;
use super::unicorn_ast::{is_left_hand_side, method_call};
use super::unicorn_types::{ARRAY, Checker};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-array-slice",
        "Prefer `Array#slice()` over `Array#splice()` when reading from the returned array.",
        NO_PRESET,
        Category::Style,
    )
    .suggestions(),
    create: create_default::<PreferArraySlice>,
};

#[derive(Debug, Default)]
struct PreferArraySlice;

impl Rule for PreferArraySlice {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some(call) = method_call(ast, node) else {
            return;
        };
        if call.name != "splice"
            || !call.plain()
            || call.args.len() != 1
            || matches!(ast.node(call.args[0]), Node::Spread { .. })
        {
            return;
        }
        let mut outer = node;
        while let Some(p) = cx.parent(outer) {
            if !matches!(cx.node(p), Node::Paren { .. }) {
                break;
            }
            outer = p;
        }
        let Some(parent) = cx.parent(outer) else {
            return;
        };
        let read = match *ast.node(parent) {
            Node::Index {
                obj,
                optional: false,
                ..
            } => obj == outer && !is_left_hand_side(cx, parent),
            Node::Member {
                obj,
                optional: false,
                prop,
            } if obj == outer && ident_name(ast, prop) == Some("at") => cx
                .parent(parent)
                .and_then(|g| method_call(ast, g))
                .is_some_and(|c| {
                    c.plain()
                        && c.args.len() == 1
                        && !matches!(ast.node(c.args[0]), Node::Spread { .. })
                }),
            _ => false,
        };
        let checker = Checker {
            class_heritage: true,
            class_syntax: true,
            mixed_union_non_target: true,
            ..ARRAY
        };
        if !read || checker.is_known_non_target(cx, call.obj) {
            return;
        }
        let prop = cx.span(call.prop);
        cx.report(
            prop,
            "Prefer `Array#slice()` over `Array#splice()` when reading from the returned array.",
        )
        .id("prefer-array-slice/error")
        .suggest(
            "prefer-array-slice/suggestion",
            "Use `Array#slice()`.",
            Fix::replace(prop, "slice"),
        );
    }
}
