//! `unicorn/prefer-array-from-async`: `const|let a = [];` (the global `Array` visible)
//! directly followed by `for [await] (const|let x of source)` whose body is only
//! `a.push(value)` (plain, non-computed, one argument), where `x` isn't named `a`, `source`
//! doesn't mention `a`, and no function that can run during the loop reads `a`.
//!
//! `value` is `x` itself, or `await e` where `e` doesn't mention `a`, doesn't suspend, and
//! (for a `const` binding) doesn't write `x`; that becomes the mapper `x => e`. A
//! `for await` loop gets a fix to `a = await Array.fromAsync(source[, mapper])`. A plain
//! `for…of` needs a mapper and a source known to yield primitives: a static string, or an
//! array literal (directly or as a single-use `const` initializer, TS wrappers skipped)
//! whose non-hole, non-spread elements are static primitive values; it gets a suggestion.
//! No fix when a comment is in the replaced range. The checker-based primitive iterables
//! of the plugin's typed mode aren't used.

use super::prelude::*;
use super::unicorn_ast::{method_call, no_spread_in};
use super::unicorn_iter::{
    contains_suspension_point, for_of_declaration_pattern, global_name_available, next_statement,
    only_expression, referenced_by_nested_function, references_symbol, unwrap_expression,
};
use super::unicorn_no_array_reduce::const_initializer;
use super::unicorn_no_useless_boolean_cast::concise_arrow_body_text;
use super::unicorn_static::{Static, static_value_control_flow};
use super::unicorn_text::{has_comment_in, paren_span, paren_text};
use crate::semantic::{RefFlags, Resolution};
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-array-from-async",
        "Prefer `Array.fromAsync()` over array accumulation loops.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create_default::<PreferArrayFromAsync>,
};

#[derive(Debug, Default)]
struct PreferArrayFromAsync;

fn is_named(ast: &Ast, id: NodeId, name: &str) -> bool {
    let id = skip_parens(ast, id);
    matches!(ast.node(id), Node::Ident { .. }) && ident_name(ast, id) == Some(name)
}

fn is_primitive(v: &Static) -> bool {
    matches!(
        v,
        Static::Undefined
            | Static::Null
            | Static::Bool(_)
            | Static::Num(_)
            | Static::Str(_)
            | Static::BigInt(_)
    )
}

fn is_known_primitive_iterable(cx: &Ctx<'_, '_>, source: NodeId) -> bool {
    let ast = cx.ast();
    let mut node = unwrap_expression(ast, source);
    if matches!(static_value_control_flow(cx, node), Some(Static::Str(_))) {
        return true;
    }
    if matches!(ast.node(node), Node::Ident { .. }) {
        let sem = cx.semantic();
        let other_refs = sem
            .reference_at(node)
            .and_then(|r| match sem.reference(r).resolution {
                Resolution::Symbol(s) => Some(s),
                _ => None,
            })
            .filter(|&s| {
                sem.symbol(s).decls.iter().any(|d| {
                    matches!(
                        d.kind,
                        DeclKind::Var | DeclKind::Let | DeclKind::Const | DeclKind::Using
                    )
                })
            })
            .is_some_and(|s| {
                sem.symbol(s).refs.iter().any(|&r| {
                    let r = sem.reference(r);
                    !r.flags.contains(RefFlags::INIT) && r.node != node
                })
            });
        let Some(init) = const_initializer(cx, node) else {
            return false;
        };
        if other_refs {
            return false;
        }
        node = unwrap_expression(ast, init);
    }
    let Node::ArrayLit { elements } = *ast.node(node) else {
        return false;
    };
    ast.list(elements).iter().all(|&e| match ast.node(e) {
        Node::Hole {} => true,
        Node::Spread { .. } => false,
        _ => static_value_control_flow(cx, e).is_some_and(|v| is_primitive(&v)),
    })
}

impl Rule for PreferArrayFromAsync {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::VarDecl]
    }
    fn enter(&self, decl: NodeId, cx: &mut Ctx<'_, '_>) {
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
            is_await,
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
        let Some(binding) = for_of_declaration_pattern(ast, left) else {
            return;
        };
        if !matches!(ast.node(binding), Node::Ident { .. }) {
            return;
        }
        let Some(binding_name) = ident_name(ast, binding) else {
            return;
        };
        let sem = cx.semantic();
        let Some(sym) = sem.symbol_of_decl(name) else {
            return;
        };
        if binding_name == array_name
            || references_symbol(cx, sym, right)
            || referenced_by_nested_function(cx, sym, loop_node)
        {
            return;
        }
        let Some(push) = method_call(ast, skip_parens(ast, expr)) else {
            return;
        };
        if push.name != "push"
            || push.args.len() != 1
            || !no_spread_in(ast, push.args, 1)
            || !push.plain()
            || !is_named(ast, push.obj, array_name)
        {
            return;
        }
        let value = skip_parens(ast, push.args[0]);
        let mapper = if is_named(ast, value, binding_name) {
            None
        } else {
            let Node::Await { arg } = *ast.node(value) else {
                return;
            };
            if references_symbol(cx, sym, arg) || contains_suspension_point(ast, arg) {
                return;
            }
            let is_const = matches!(
                *ast.node(left),
                Node::VarDecl {
                    kind: VarKind::Const,
                    ..
                }
            );
            let arg_span = cx.span(arg);
            let writes = sem.symbol_of_decl(binding).is_some_and(|b| {
                sem.symbol(b).refs.iter().any(|&r| {
                    let r = sem.reference(r);
                    let s = cx.span(r.node);
                    !r.flags.contains(RefFlags::INIT)
                        && r.is_write()
                        && s.start >= arg_span.start
                        && s.end <= arg_span.end
                })
            });
            if is_const && writes {
                return;
            }
            Some(arg)
        };
        if !is_await && (mapper.is_none() || !is_known_primitive_iterable(cx, right)) {
            return;
        }
        let range = Span::new(cx.span(decl).start, cx.span(loop_node).end);
        let fix = (!has_comment_in(cx, range)).then(|| {
            let mut text = format!("await Array.fromAsync({}", paren_text(cx, right));
            if let Some(m) = mapper {
                text.push_str(&format!(
                    ", {binding_name} => {}",
                    concise_arrow_body_text(cx, m)
                ));
            }
            text.push_str(");");
            Fix::replace(
                Span::new(paren_span(cx, init).start, cx.span(loop_node).end),
                text,
            )
        });
        let r = cx
            .report(
                cx.span(loop_node),
                "Prefer `Array.fromAsync()` over array accumulation loops.",
            )
            .id("prefer-array-from-async");
        if let Some(f) = fix {
            if is_await {
                r.fix(f);
            } else {
                r.suggest(
                    "prefer-array-from-async/suggestion",
                    "Replace the loop with `Array.fromAsync()`.",
                    f,
                );
            }
        }
    }
}
