//! `unicorn/prefer-array-from-map`: two shapes that should pass the mapping function to
//! `Array.from()`.
//!
//! - `Array.from(x).map(cb)` on the global `Array` (one non-spread argument to `from`, one
//!   or two to `map`, no optional chaining or type arguments) where `cb` is an arrow with at
//!   most two parameters and no rest, unless `.flat()` / `.flat(1)` follows. Reported at
//!   `map`; the suggestion merges the argument lists (none when either call is
//!   parenthesized or a comment sits between the two lists).
//! - `const|let a = [];` directly followed by `for (const|let <binding> of source)` whose
//!   body is only `a.push(value)` (one non-spread argument, not the bare element) or, with an
//!   `[index, element]` binding over `array.entries()`, `a[index] = value`. The binding must
//!   not be named `a`, `source` and `value` must not mention `a`, `value` must not suspend
//!   (`await`/`yield`), and the global `Array` must be visible. A `[index, element]` binding
//!   needs `source` to be `x.entries()`: for a known array `x` the callback becomes
//!   `(element, index)` over `x`, otherwise `([index, element])` over `source`. Reported at
//!   the loop; the fix (a suggestion when a function that can run during the loop reads `a`)
//!   rewrites the initializer and removes the loop, unless a comment would be lost.

use super::prelude::*;
use super::unicorn_ast::{method_call, no_spread_in};
use super::unicorn_iter::{
    contains_suspension_point, for_of_declaration_pattern, global_name_available, next_statement,
    only_expression, pair_pattern, referenced_by_nested_function, references_symbol,
};
use super::unicorn_no_useless_boolean_cast::concise_arrow_body_text;
use super::unicorn_static::is_global_ident;
use super::unicorn_text::{has_comment_in, is_parenthesized, paren_span, paren_text};
use super::unicorn_types::is_array;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-array-from-map",
        "Prefer using the `Array.from()` mapping function argument.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create_default::<PreferArrayFromMap>,
};

#[derive(Debug, Default)]
struct PreferArrayFromMap;

const MESSAGE: &str = "Pass the mapping function to `Array.from()` directly.";
const ERROR: &str = "prefer-array-from-map/error";
const SUGGESTION: &str = "prefer-array-from-map/suggestion";
const SUGGESTION_MESSAGE: &str = "Use the `Array.from()` mapping function argument.";

fn has_type_args(ast: &Ast, call: NodeId) -> bool {
    matches!(
        *ast.node(call),
        Node::Call {
            type_args: Some(_),
            ..
        }
    )
}

fn is_named(ast: &Ast, id: NodeId, name: &str) -> bool {
    let id = skip_parens(ast, id);
    matches!(ast.node(id), Node::Ident { .. }) && ident_name(ast, id) == Some(name)
}

/// `.flat()` or `.flat(1)` is called on the result of `call`.
fn followed_by_default_flat(cx: &Ctx<'_, '_>, call: NodeId) -> bool {
    let ast = cx.ast();
    let Some(member) = parent_skip_parens(ast, cx.file.parents, call) else {
        return false;
    };
    let Some(outer) = parent_skip_parens(ast, cx.file.parents, member) else {
        return false;
    };
    let Some(flat) = method_call(ast, outer) else {
        return false;
    };
    flat.name == "flat"
        && skip_parens(ast, flat.obj) == call
        && flat.plain()
        && match flat.args {
            [] => true,
            [a] => matches!(ast.node(*a), Node::NumberLit { .. }) && cx.text(*a) == "1",
            _ => false,
        }
}

/// An arrow with at most two parameters and no rest parameter.
fn is_supported_arrow(ast: &Ast, id: NodeId) -> bool {
    let Node::Arrow { params, .. } = *ast.node(skip_parens(ast, id)) else {
        return false;
    };
    let params = ast.list(params);
    params.len() <= 2
        && params
            .iter()
            .all(|&p| !matches!(ast.node(p), Node::Param { rest: true, .. }))
}

fn arguments_fix(cx: &Ctx<'_, '_>, from: NodeId, map: NodeId, map_callee: NodeId) -> Option<Fix> {
    if is_parenthesized(cx, from) || is_parenthesized(cx, map_callee) {
        return None;
    }
    let ast = cx.ast();
    let (_, from_close, from_comma) = super::unicorn_ast::call_tokens(cx, from)?;
    let (_, _, map_comma) = super::unicorn_ast::call_tokens(cx, map)?;
    let Node::Call { args, .. } = *ast.node(map) else {
        return None;
    };
    let first = *ast.list(args).first()?;
    let range = Span::new(
        from_comma.unwrap_or(from_close),
        paren_span(cx, first).start,
    );
    if has_comment_in(cx, range) {
        return None;
    }
    let mut fix = Fix::replace(range, ", ");
    if let Some(c) = map_comma {
        fix = fix.and(Span::new(c, c + 1), "");
    }
    Some(fix)
}

impl PreferArrayFromMap {
    fn check_map(&self, cx: &mut Ctx<'_, '_>, id: NodeId) {
        let ast = cx.ast();
        let Some(map) = method_call(ast, id) else {
            return;
        };
        if map.name != "map"
            || !(1..=2).contains(&map.args.len())
            || !no_spread_in(ast, map.args, 2)
            || !map.plain()
            || has_type_args(ast, id)
        {
            return;
        }
        let from_id = skip_parens(ast, map.obj);
        let Some(from) = method_call(ast, from_id) else {
            return;
        };
        let array = skip_parens(ast, from.obj);
        if from.name != "from"
            || from.args.len() != 1
            || !no_spread_in(ast, from.args, 1)
            || !from.plain()
            || has_type_args(ast, from_id)
            || !matches!(ast.node(array), Node::Ident { .. })
            || !is_global_ident(cx, array, "Array")
            || followed_by_default_flat(cx, id)
            || !is_supported_arrow(ast, map.args[0])
        {
            return;
        }
        let Some(callee) = cx.parent(map.prop) else {
            return;
        };
        let fix = arguments_fix(cx, from_id, id, callee);
        let r = cx.report(cx.span(map.prop), MESSAGE).id(ERROR);
        if let Some(f) = fix {
            r.suggest(SUGGESTION, SUGGESTION_MESSAGE, f);
        }
    }

    fn check_loop(&self, cx: &mut Ctx<'_, '_>, decl: NodeId) {
        let ast = cx.ast();
        let Node::VarDecl { kind, decls, .. } = *ast.node(decl) else {
            return;
        };
        if !matches!(kind, VarKind::Const | VarKind::Let) {
            return;
        }
        let [declarator] = *ast.list(decls) else {
            return;
        };
        let Node::VarDeclarator {
            name,
            init: Some(init),
            ..
        } = *ast.node(declarator)
        else {
            return;
        };
        if !matches!(ast.node(name), Node::Ident { .. })
            || !matches!(*ast.node(skip_parens(ast, init)), Node::ArrayLit { elements } if ast.list(elements).is_empty())
            || !global_name_available(cx, "Array", decl)
        {
            return;
        }
        let Some(array_name) = ident_name(ast, name) else {
            return;
        };
        let Some(loop_node) = next_statement(cx, decl) else {
            return;
        };
        let Node::ForOf {
            is_await: false,
            left,
            right,
            body,
        } = *ast.node(loop_node)
        else {
            return;
        };
        let Some(expr) = only_expression(ast, body) else {
            return;
        };
        let Some(pattern) = for_of_declaration_pattern(ast, left) else {
            return;
        };
        let (index, element) = if matches!(ast.node(pattern), Node::Ident { .. }) {
            (None, pattern)
        } else if let Some((i, e)) = pair_pattern(ast, pattern) {
            (Some(i), e)
        } else {
            return;
        };
        let Some(sym) = cx.semantic().symbol_of_decl(name) else {
            return;
        };
        if ident_name(ast, element) == Some(array_name)
            || index.is_some_and(|i| ident_name(ast, i) == Some(array_name))
            || references_symbol(cx, sym, right)
        {
            return;
        }
        let usable = |value: NodeId| {
            !references_symbol(cx, sym, value) && !contains_suspension_point(ast, value)
        };
        let array_from = |iterable: NodeId, params: String, value: NodeId| {
            format!(
                "Array.from({}, {params} => {})",
                paren_text(cx, iterable),
                concise_arrow_body_text(cx, value)
            )
        };
        let entries_receiver = || {
            method_call(ast, skip_parens(ast, right))
                .filter(|c| c.name == "entries" && c.args.is_empty() && c.plain())
                .map(|c| c.obj)
        };
        let element_text = cx.text(element);
        let push = method_call(ast, skip_parens(ast, expr)).filter(|c| {
            c.name == "push"
                && c.args.len() == 1
                && no_spread_in(ast, c.args, 1)
                && c.plain()
                && is_named(ast, c.obj, array_name)
                && usable(c.args[0])
        });
        let replacement = if let Some(push) = push {
            let value = push.args[0];
            match index {
                None => {
                    if is_named(ast, value, element_text) {
                        return;
                    }
                    array_from(right, element_text.to_owned(), value)
                }
                Some(i) => {
                    let Some(receiver) = entries_receiver() else {
                        return;
                    };
                    let index_text = cx.text(i);
                    if is_array(cx, receiver) {
                        array_from(receiver, format!("({element_text}, {index_text})"), value)
                    } else {
                        array_from(right, format!("([{index_text}, {element_text}])"), value)
                    }
                }
            }
        } else {
            let Some(i) = index else {
                return;
            };
            let Some(receiver) = entries_receiver().filter(|&r| is_array(cx, r)) else {
                return;
            };
            let Node::Assign {
                left: target,
                op: AssignOp::Assign,
                right: value,
            } = *ast.node(skip_parens(ast, expr))
            else {
                return;
            };
            let Node::Index {
                obj,
                index: key,
                optional: false,
            } = *ast.node(skip_parens(ast, target))
            else {
                return;
            };
            let Some(index_name) = ident_name(ast, i) else {
                return;
            };
            if !is_named(ast, obj, array_name) || !is_named(ast, key, index_name) || !usable(value)
            {
                return;
            }
            array_from(receiver, format!("({element_text}, {})", cx.text(i)), value)
        };
        let range = Span::new(cx.span(decl).start, cx.span(loop_node).end);
        let fix = (!has_comment_in(cx, range)).then(|| {
            Fix::replace(
                Span::new(paren_span(cx, init).start, cx.span(loop_node).end),
                format!("{replacement};"),
            )
        });
        let nested = referenced_by_nested_function(cx, sym, loop_node);
        let r = cx.report(cx.span(loop_node), MESSAGE).id(ERROR);
        if let Some(f) = fix {
            if nested {
                r.suggest(SUGGESTION, SUGGESTION_MESSAGE, f);
            } else {
                r.fix(f);
            }
        }
    }
}

impl Rule for PreferArrayFromMap {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::VarDecl, NodeKind::Call]
    }
    fn enter(&self, id: NodeId, cx: &mut Ctx<'_, '_>) {
        if matches!(cx.node(id), Node::VarDecl { .. }) {
            self.check_loop(cx, id);
        } else {
            self.check_map(cx, id);
        }
    }
}
