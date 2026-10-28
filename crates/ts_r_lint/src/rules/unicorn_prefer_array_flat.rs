//! `unicorn/prefer-array-flat`: `array.flat()` over legacy ways to flatten.
//!
//! The call is reported (once per matching form) and replaced by `<array>.flat()`:
//! - `array.flatMap(x => x)` (non-async arrow of one parameter returning it; not an
//!   optional call), unless the receiver is obviously not an array: a PascalCase identifier
//!   that isn't a `const` holding an array literal or `new Array`, or a `const` holding an
//!   object, literal, template, function, class or `new` of something else;
//! - `array.reduce((a, b) => a.concat(b), [])`;
//! - `[].concat(...array)`;
//! - `[].concat.apply([], array)` / `Array.prototype.concat.call([], ...array)`;
//! - `_.flatten(array)`, `lodash.flatten`, `underscore.flatten` and the `functions` option
//!   (names or dotted paths), with exactly one non-spread argument.
//!
//! An optional `?.` before `flatMap` / `reduce` is kept (`array?.flat()`). Arrays known not
//! to be arrays (`unicorn_types::is_known_non_array`) and `arguments` are skipped. The fix
//! needs the call to hold no comment outside the array, and adds a `;` / keyword space when
//! needed.

use super::prelude::*;
use super::unicorn_types::is_known_non_array;
use super::unicorn_util::{space_around_keyword, with_edits};
use super::unicorn_util_h5::{
    member_object_text, method_call_of, needs_semicolon, node_matches_path,
};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-array-flat",
        "Prefer `Array#flat()` over legacy techniques to flatten arrays.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<PreferArrayFlat>,
};

#[derive(Debug)]
struct PreferArrayFlat {
    functions: Vec<String>,
}

impl FromOptions for PreferArrayFlat {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let mut functions = str_list(obj_opt(opts, 0), "functions");
        functions.extend(
            ["_.flatten", "lodash.flatten", "underscore.flatten"]
                .iter()
                .map(|s| (*s).to_owned()),
        );
        Ok(PreferArrayFlat { functions })
    }
}

fn is_empty_array(ast: &ts_r_ast::Ast, id: NodeId) -> bool {
    matches!(*ast.node(skip_parens(ast, id)), Node::ArrayLit { elements } if ast.list(elements).is_empty())
}

fn same_ident(ast: &ts_r_ast::Ast, param: NodeId, expr: NodeId) -> bool {
    let Node::Param { pattern, .. } = *ast.node(param) else {
        return false;
    };
    let expr = skip_parens(ast, expr);
    matches!(ast.node(pattern), Node::Ident { .. })
        && matches!(ast.node(expr), Node::Ident { .. })
        && ident_name(ast, pattern) == ident_name(ast, expr)
}

/// A non-async arrow and its parameters and body.
fn arrow(ast: &ts_r_ast::Ast, id: NodeId) -> Option<(&[NodeId], NodeId)> {
    match *ast.node(skip_parens(ast, id)) {
        Node::Arrow {
            flags,
            params,
            body,
            ..
        } if !flags.contains(FnFlags::ASYNC) => Some((ast.list(params), body)),
        _ => None,
    }
}

fn member_optional(ast: &ts_r_ast::Ast, call: NodeId) -> bool {
    matches!(*ast.node(call), Node::Call { callee, .. }
        if matches!(*ast.node(skip_parens(ast, callee)), Node::Member { optional: true, .. }))
}

fn is_pascal_case(name: &str) -> bool {
    name.chars().next().is_some_and(char::is_uppercase)
}

fn obviously_non_array_receiver(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let id = skip_parens(ast, id);
    let init = if matches!(ast.node(id), Node::Ident { .. }) {
        const_init(cx, id).map(|i| skip_parens(ast, i))
    } else {
        None
    };
    let new_of = |i: NodeId| match *ast.node(i) {
        Node::New { callee, .. } => ident_name(ast, callee),
        _ => None,
    };
    let array_init = init.is_some_and(|i| {
        matches!(ast.node(i), Node::ArrayLit { .. }) || new_of(i) == Some("Array")
    });
    let non_array_init = init.is_some_and(|i| {
        matches!(
            ast.node(i),
            Node::ObjectLit { .. }
                | Node::StringLit { .. }
                | Node::NumberLit { .. }
                | Node::BigIntLit { .. }
                | Node::BoolLit { .. }
                | Node::NullLit {}
                | Node::RegexLit { .. }
                | Node::TemplateLit { .. }
                | Node::Arrow { .. }
                | Node::FunctionExpr { .. }
                | Node::ClassExpr { .. }
        ) || new_of(i).is_some_and(|n| n != "Array")
    });
    (ident_name(ast, id).is_some_and(is_pascal_case) && !array_init) || non_array_init
}

impl PreferArrayFlat {
    /// Each form `node` matches: the flattened array, whether to keep a `?.`, and the
    /// message's description.
    fn matches(&self, cx: &Ctx<'_, '_>, node: NodeId) -> Vec<(NodeId, bool, String)> {
        let ast = cx.ast();
        let mut out = Vec::new();
        // `array.flatMap(x => x)`
        if let Some((obj, _, &[cb])) = method_call_of(ast, node, &["flatMap"], 1, Some(1), None)
            && let Some((&[param], body)) = arrow(ast, cb)
            && same_ident(ast, param, body)
            && !obviously_non_array_receiver(cx, obj)
        {
            out.push((
                obj,
                member_optional(ast, node),
                "Array#flatMap()".to_owned(),
            ));
        }
        // `array.reduce((a, b) => a.concat(b), [])`
        if let Some((obj, _, &[cb, init])) =
            method_call_of(ast, node, &["reduce"], 2, Some(2), None)
            && let Some((&[a, b], body)) = arrow(ast, cb)
            && is_empty_array(ast, init)
            && let Some((concat_obj, _, &[concat_arg])) = method_call_of(
                ast,
                skip_parens(ast, body),
                &["concat"],
                1,
                Some(1),
                Some(false),
            )
            && same_ident(ast, a, concat_obj)
            && same_ident(ast, b, concat_arg)
        {
            out.push((obj, member_optional(ast, node), "Array#reduce()".to_owned()));
        }
        // `[].concat(...array)`
        if let Some((obj, _, &[spread])) =
            method_call_of(ast, node, &["concat"], 1, None, Some(false))
            && is_empty_array(ast, obj)
            && let Node::Spread { arg } = *ast.node(spread)
        {
            out.push((arg, false, "[].concat()".to_owned()));
        }
        // `[].concat.apply([], array)`, `Array.prototype.concat.call([], ...array)`
        if let Some((obj, prop, &[first, second])) =
            method_call_of(ast, node, &["apply", "call"], 2, None, Some(false))
            && is_array_prototype_concat(ast, skip_parens(ast, obj))
            && is_empty_array(ast, first)
        {
            let spread = match *ast.node(second) {
                Node::Spread { arg } => Some(arg),
                _ => None,
            };
            match (ident_name(ast, prop), spread) {
                (Some("apply"), None) => {
                    out.push((second, false, "Array.prototype.concat()".to_owned()));
                }
                (Some("call"), Some(arg)) => {
                    out.push((arg, false, "Array.prototype.concat()".to_owned()));
                }
                _ => {}
            }
        }
        // Configured functions.
        if let Node::Call {
            callee,
            optional: false,
            args,
            ..
        } = *ast.node(node)
            && let &[arg] = ast.list(args)
            && !matches!(ast.node(arg), Node::Spread { .. })
            && let Some(path) = self.functions.iter().find(|p| path_matches(ast, callee, p))
        {
            out.push((arg, false, format!("{}()", path.trim())));
        }
        out
    }
}

fn path_matches(ast: &ts_r_ast::Ast, callee: NodeId, path: &str) -> bool {
    let path = path.trim();
    !path.split('.').skip(1).any(str::is_empty) && node_matches_path(ast, callee, path)
}

/// `[].concat` or `Array.prototype.concat` (non-optional, non-computed).
fn is_array_prototype_concat(ast: &ts_r_ast::Ast, id: NodeId) -> bool {
    let Node::Member {
        obj,
        prop,
        optional: false,
    } = *ast.node(id)
    else {
        return false;
    };
    if ident_name(ast, prop) != Some("concat") {
        return false;
    }
    let obj = skip_parens(ast, obj);
    is_empty_array(ast, obj)
        || matches!(*ast.node(obj), Node::Member { obj: o, prop: p, optional: false }
            if ident_name(ast, p) == Some("prototype") && ident_name(ast, skip_parens(ast, o)) == Some("Array"))
}

impl Rule for PreferArrayFlat {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        for (array, optional, description) in self.matches(cx, node) {
            let ast = cx.ast();
            let array = skip_parens(ast, array);
            if is_known_non_array(cx, array) || ident_name(ast, array) == Some("arguments") {
                continue;
            }
            let fix = (ast.comments_in(cx.span(node)).len()
                == ast.comments_in(cx.span(array)).len())
            .then(|| {
                let mut text = format!(
                    "{}{}.flat()",
                    member_object_text(cx, array, node),
                    if optional { "?" } else { "" }
                );
                if needs_semicolon(cx, cx.span(node).start, &text) {
                    text = format!(";{text}");
                }
                with_edits(
                    Fix::replace(cx.span(node), text),
                    space_around_keyword(cx, node),
                )
            });
            cx.report_node(
                node,
                format!("Prefer `Array#flat()` over `{description}` to flatten an array."),
            )
            .id("prefer-array-flat")
            .fix_opt(fix);
        }
    }
}
