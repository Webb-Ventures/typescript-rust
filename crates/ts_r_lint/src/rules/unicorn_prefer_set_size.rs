//! `unicorn/prefer-set-size`: `set.size` over `[...set].length` and `Array.from(set).length`.
//!
//! A non-optional, non-computed `.length` that isn't written to, whose object is
//! `[...x]` (one spread element) or `Array.from(x)` (exactly one non-spread argument,
//! nothing optional), with `x` known to be a `Set` (the type-guessing engine), is reported
//! at `length`. The fix renames it to `size` and puts `x` (as a member object, with its
//! parentheses) in place of the array; it is dropped when the array holds more comments
//! than `x`.

use super::prelude::*;
use super::unicorn_types::is_set;
use super::unicorn_util::{
    in_unparenthesized_new_callee, is_left_hand_side, member_object_needs_parens,
    member_object_text, member_on_needs_new_parens, needs_semicolon,
};
use super::unicorn_util::{space_around_keyword, with_edits};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-set-size",
        "Prefer using `Set#size` instead of `Array#length`.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<PreferSetSize>,
};

#[derive(Debug, Default)]
struct PreferSetSize;

impl Rule for PreferSetSize {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Member]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Member {
            obj,
            prop,
            optional: false,
        } = *cx.node(node)
        else {
            return;
        };
        if ident_name(ast, prop) != Some("length") || is_left_hand_side(cx, node) {
            return;
        }
        let array = skip_parens(ast, obj);
        let Some(set) = set_node(ast, array) else {
            return;
        };
        if !is_set(cx, set) {
            return;
        }
        let set = skip_parens(ast, set);
        let fix = fix(cx, node, prop, array, set);
        cx.report_node(prop, "Prefer using `Set#size` instead of `Array#length`.")
            .id("prefer-set-size")
            .fix_opt(fix);
    }
}

fn set_node(ast: &ts_r_ast::Ast, array: NodeId) -> Option<NodeId> {
    match *ast.node(array) {
        Node::ArrayLit { elements } => match *ast.list(elements) {
            [only] => match *ast.node(only) {
                Node::Spread { arg } => Some(arg),
                _ => None,
            },
            _ => None,
        },
        Node::Call {
            callee,
            optional: false,
            args,
            ..
        } => {
            let Node::Member {
                obj,
                prop,
                optional: false,
            } = *ast.node(callee)
            else {
                return None;
            };
            let &[arg] = ast.list(args) else {
                return None;
            };
            (ident_name(ast, obj) == Some("Array")
                && ident_name(ast, prop) == Some("from")
                && !matches!(ast.node(arg), Node::Spread { .. }))
            .then_some(arg)
        }
        _ => None,
    }
}

fn fix(cx: &Ctx<'_, '_>, member: NodeId, prop: NodeId, array: NodeId, set: NodeId) -> Option<Fix> {
    let ast = cx.ast();
    if ast.comments_in(cx.span(array)).len() > ast.comments_in(cx.span(set)).len() {
        return None;
    }
    let mut text = member_object_text(cx, set, array);
    if in_unparenthesized_new_callee(cx, array)
        && !member_object_needs_parens(cx, set, array)
        && member_on_needs_new_parens(cx, set)
    {
        text = format!("({text})");
    }
    let start = cx.span(array).start;
    if needs_semicolon(cx, start, &text) {
        text = format!(";{text}");
    }
    let mut fix = Fix::replace(ast.span(prop), "size").and(cx.span(array), text);
    if matches!(cx.node(array), Node::ArrayLit { .. }) {
        fix = with_edits(fix, space_around_keyword(cx, member));
    }
    Some(fix)
}
