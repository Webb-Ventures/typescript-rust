//! The check behind `unicorn/no-unnecessary-slice-end` and
//! `unicorn/no-unnecessary-array-splice-count`: a plain two-argument method call whose
//! second argument is `Infinity`, `Number.POSITIVE_INFINITY` or the receiver's own
//! `.length` / `?.length` (the receiver compared with `is_same_reference`). The fix removes
//! the argument, unless it is a `.length` and the first argument assigns or updates
//! something (which could change the receiver first).

use super::prelude::*;
use super::unicorn_ast::{contains_node, method_call, remove_argument};
use super::unicorn_types::should_skip_known_non_array_receiver;
use super::unicorn_util::is_same_reference;

fn description(cx: &Ctx<'_, '_>, arg: NodeId, object: NodeId) -> Option<String> {
    let ast = cx.ast();
    match *ast.node(arg) {
        Node::Ident { .. } if ident_name(ast, arg) == Some("Infinity") => {
            return Some("Infinity".to_owned());
        }
        Node::Member {
            obj,
            optional: false,
            prop,
        } if ident_name(ast, obj) == Some("Number")
            && ident_name(ast, prop) == Some("POSITIVE_INFINITY") =>
        {
            return Some("Number.POSITIVE_INFINITY".to_owned());
        }
        _ => {}
    }
    let Node::Member {
        obj,
        optional,
        prop,
    } = *ast.node(arg)
    else {
        return None;
    };
    if ident_name(ast, prop) != Some("length") || !is_same_reference(cx, object, obj) {
        return None;
    }
    let name = match ast.node(object) {
        Node::Ident { .. } => ident_name(ast, object).unwrap_or("…"),
        _ => "…",
    };
    Some(format!("{name}{}length", if optional { "?." } else { "." }))
}

pub(crate) fn check(
    cx: &mut Ctx<'_, '_>,
    node: NodeId,
    methods: &[&str],
    check_array_receiver: bool,
    message_id: &'static str,
) {
    let ast = cx.ast();
    let Some(call) = method_call(ast, node) else {
        return;
    };
    if call.optional_call || !methods.contains(&call.name) {
        return;
    }
    let [first, second] = *call.args else {
        return;
    };
    if [first, second]
        .iter()
        .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
    {
        return;
    }
    let second_inner = skip_parens(ast, second);
    let Some(desc) = description(cx, second_inner, call.obj) else {
        return;
    };
    if check_array_receiver && should_skip_known_non_array_receiver(cx, call.obj) {
        return;
    }
    let message = match call.name {
        "splice" => format!("Passing `{desc}` as the `deleteCount` argument is unnecessary."),
        "toSpliced" => format!("Passing `{desc}` as the `skipCount` argument is unnecessary."),
        _ => format!("Passing `{desc}` as the `end` argument is unnecessary."),
    };
    let writes = desc.ends_with("length")
        && contains_node(ast, first, |n| {
            matches!(n, Node::Assign { .. } | Node::Update { .. })
        });
    let fix = if writes {
        None
    } else {
        remove_argument(cx, node, 1)
    };
    cx.report(cx.span(second_inner), message)
        .id(message_id)
        .fix_opt(fix);
}
