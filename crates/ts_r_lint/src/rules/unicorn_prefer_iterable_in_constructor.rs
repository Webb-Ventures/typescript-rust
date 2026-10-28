//! `unicorn/prefer-iterable-in-constructor`: a one-declarator declaration of an empty
//! `new Set/WeakSet/Map/WeakMap/URLSearchParams()` directly followed by a non-`await`
//! `for (const|let <binding> of source)` whose body is only `x.add(binding)` (sets),
//! `x.set(k, v)` with a `[k, v]` binding (maps), or `x.append/set(k, v)` over
//! `Object.entries(record)` (`URLSearchParams`, which takes `record` itself). The binding
//! must not redeclare `x`, `source` must not mention `x`, and `source` (or the `const`
//! initializer it names, TS wrappers skipped) must not be an object literal with computed
//! keys, a map array literal with non-array entries, or for `URLSearchParams` an array,
//! literal, unary, template, `new` or `undefined`/`NaN`/`Infinity`. Reported at the loop.
//! Also `new URLSearchParams(Object.entries(record))`, at `Object.entries`.
//!
//! The fix passes the source to the constructor and removes the loop (a suggestion when a
//! function that can run during the loop reads `x`); none when a comment is in the range.

use super::prelude::*;
use super::unicorn_ast::{method_call, no_spread_in, remove_statements};
use super::unicorn_iter::{
    for_of_declaration_pattern, next_statement, only_expression, pair_pattern,
    referenced_by_nested_function, references_symbol, unwrap_expression,
};
use super::unicorn_no_array_reduce::const_initializer;
use super::unicorn_text::{has_comment_in, paren_text};
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-iterable-in-constructor",
        "Prefer passing iterables directly to constructors instead of filling empty collections.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create_default::<PreferIterableInConstructor>,
};

#[derive(Debug, Default)]
struct PreferIterableInConstructor;

const SETS: &[&str] = &["Set", "WeakSet"];
const MAPS: &[&str] = &["Map", "WeakMap"];
const PARAMS: &str = "URLSearchParams";

/// `new Name()` (no arguments) with `Name` one of the supported constructors.
fn empty_constructor(ast: &Ast, id: NodeId) -> Option<&str> {
    let Node::New { callee, args, .. } = *ast.node(id) else {
        return None;
    };
    let callee = skip_parens(ast, callee);
    let name = match ast.node(callee) {
        Node::Ident { .. } => ident_name(ast, callee)?,
        _ => return None,
    };
    (ast.list(args).is_empty() && (SETS.contains(&name) || MAPS.contains(&name) || name == PARAMS))
        .then_some(name)
}

/// `Object.entries(x)`: `x`.
fn object_entries_argument(ast: &Ast, id: NodeId) -> Option<NodeId> {
    let call = method_call(ast, skip_parens(ast, id))?;
    (call.name == "entries"
        && call.plain()
        && call.args.len() == 1
        && no_spread_in(ast, call.args, 1)
        && matches!(ast.node(skip_parens(ast, call.obj)), Node::Ident { .. })
        && ident_name(ast, skip_parens(ast, call.obj)) == Some("Object"))
    .then(|| call.args[0])
}

fn directly_unsafe(ast: &Ast, name: &str, source: NodeId) -> bool {
    let computed = match *ast.node(source) {
        Node::ObjectLit { props } => ast.list(props).iter().any(|&p| {
            let key = match *ast.node(p) {
                Node::Property { key, .. } | Node::MethodDef { key, .. } => key,
                _ => return false,
            };
            matches!(ast.node(key), Node::ComputedKey { .. })
        }),
        _ => false,
    };
    let not_record = || match *ast.node(source) {
        Node::ArrayLit { .. }
        | Node::StringLit { .. }
        | Node::NumberLit { .. }
        | Node::BigIntLit { .. }
        | Node::BoolLit { .. }
        | Node::NullLit {}
        | Node::RegexLit { .. }
        | Node::Unary { .. }
        | Node::TemplateLit { .. }
        | Node::New { .. } => true,
        Node::Ident { .. } => {
            matches!(
                ident_name(ast, source),
                Some("Infinity" | "NaN" | "undefined")
            )
        }
        _ => false,
    };
    let unsafe_map_entry = || match *ast.node(source) {
        Node::ArrayLit { elements } => ast
            .list(elements)
            .iter()
            .any(|&e| !matches!(ast.node(skip_parens(ast, e)), Node::ArrayLit { .. })),
        _ => false,
    };
    computed || (name == PARAMS && not_record()) || (MAPS.contains(&name) && unsafe_map_entry())
}

fn unsafe_source(cx: &Ctx<'_, '_>, name: &str, source: NodeId) -> bool {
    let ast = cx.ast();
    let expr = unwrap_expression(ast, source);
    let value = const_initializer(cx, expr).unwrap_or(expr);
    directly_unsafe(ast, name, unwrap_expression(ast, value))
}

fn same_ident(ast: &Ast, a: NodeId, b: NodeId) -> bool {
    let (a, b) = (skip_parens(ast, a), skip_parens(ast, b));
    matches!(ast.node(a), Node::Ident { .. })
        && matches!(ast.node(b), Node::Ident { .. })
        && ident_name(ast, a) == ident_name(ast, b)
}

impl PreferIterableInConstructor {
    fn check_loop(&self, cx: &mut Ctx<'_, '_>, decl: NodeId) {
        let ast = cx.ast();
        let Node::VarDecl {
            modifiers, decls, ..
        } = *ast.node(decl)
        else {
            return;
        };
        if modifiers.contains(Modifiers::EXPORT) {
            return;
        }
        let [declarator] = *ast.list(decls) else {
            return;
        };
        let Node::VarDeclarator {
            name: id,
            init: Some(init),
            ..
        } = *ast.node(declarator)
        else {
            return;
        };
        let new = skip_parens(ast, init);
        if !matches!(ast.node(id), Node::Ident { .. }) {
            return;
        }
        let Some(ctor) = empty_constructor(ast, new) else {
            return;
        };
        let Some(var) = ident_name(ast, id) else {
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
        let Some(call) =
            only_expression(ast, body).and_then(|e| method_call(ast, skip_parens(ast, e)))
        else {
            return;
        };
        let source = if ctor == PARAMS {
            match object_entries_argument(ast, right) {
                Some(s) => s,
                None => return,
            }
        } else {
            right
        };
        let Some(binding) = for_of_declaration_pattern(ast, left) else {
            return;
        };
        let mut bound = Vec::new();
        binding_idents(ast, binding, &mut bound);
        if bound.iter().any(|&b| ident_name(ast, b) == Some(var)) || unsafe_source(cx, ctor, source)
        {
            return;
        }
        let sem = cx.semantic();
        let sym = sem.symbol_of_decl(id);
        if sym.is_some_and(|s| references_symbol(cx, s, right)) {
            return;
        }
        let on_var = call.plain() && is_named(ast, call.obj, var);
        let matches = if SETS.contains(&ctor) {
            on_var
                && call.name == "add"
                && call.args.len() == 1
                && no_spread_in(ast, call.args, 1)
                && matches!(ast.node(binding), Node::Ident { .. })
                && same_ident(ast, binding, call.args[0])
        } else {
            let methods_ok = if ctor == PARAMS {
                matches!(call.name, "append" | "set")
            } else {
                call.name == "set"
            };
            on_var
                && methods_ok
                && call.args.len() == 2
                && no_spread_in(ast, call.args, 2)
                && pair_pattern(ast, binding).is_some_and(|(k, v)| {
                    same_ident(ast, k, call.args[0]) && same_ident(ast, v, call.args[1])
                })
        };
        if !matches {
            return;
        }
        let range = Span::new(cx.span(decl).start, cx.span(loop_node).end);
        let fix = (!has_comment_in(cx, range)).then(|| {
            let Node::New {
                callee, type_args, ..
            } = *ast.node(new)
            else {
                return Fix::default();
            };
            let end = type_args.map_or(cx.span(callee).end, |t| cx.span(t).end);
            let ctor_text = cx.src()[cx.span(new).start as usize..end as usize].trim_end();
            remove_statements(cx, loop_node, loop_node, false).and(
                cx.span(new),
                format!("{ctor_text}({})", paren_text(cx, source)),
            )
        });
        let message = format!(
            "Pass `{}` directly to `new {ctor}()`.",
            cx.text(skip_parens(ast, source))
        );
        let nested = sym.is_some_and(|s| referenced_by_nested_function(cx, s, loop_node));
        let r = cx
            .report(cx.span(loop_node), message)
            .id("prefer-iterable-in-constructor");
        if let Some(f) = fix {
            if nested {
                r.suggest(
                    "prefer-iterable-in-constructor/suggestion",
                    format!("Pass the iterable directly to `new {ctor}()`."),
                    f,
                );
            } else {
                r.fix(f);
            }
        }
    }

    fn check_new(&self, cx: &mut Ctx<'_, '_>, new: NodeId) {
        let ast = cx.ast();
        let Node::New { callee, args, .. } = *ast.node(new) else {
            return;
        };
        let callee = skip_parens(ast, callee);
        let [arg] = *ast.list(args) else {
            return;
        };
        if !matches!(ast.node(callee), Node::Ident { .. })
            || ident_name(ast, callee) != Some(PARAMS)
            || matches!(ast.node(arg), Node::Spread { .. })
        {
            return;
        }
        let Some(source) = object_entries_argument(ast, arg) else {
            return;
        };
        if unsafe_source(cx, PARAMS, source) {
            return;
        }
        let entries = skip_parens(ast, arg);
        let Node::Call {
            callee: entries_callee,
            ..
        } = *ast.node(entries)
        else {
            return;
        };
        let span = cx.span(entries);
        let message = format!(
            "Pass `{}` directly to `new {PARAMS}()`.",
            cx.text(skip_parens(ast, source))
        );
        let fix = (!has_comment_in(cx, span)).then(|| Fix::replace(span, paren_text(cx, source)));
        let r = cx
            .report(cx.span(skip_parens(ast, entries_callee)), message)
            .id("prefer-iterable-in-constructor");
        if let Some(f) = fix {
            r.fix(f);
        }
    }
}

fn is_named(ast: &Ast, id: NodeId, name: &str) -> bool {
    let id = skip_parens(ast, id);
    matches!(ast.node(id), Node::Ident { .. }) && ident_name(ast, id) == Some(name)
}

impl Rule for PreferIterableInConstructor {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::VarDecl, NodeKind::New]
    }
    fn enter(&self, id: NodeId, cx: &mut Ctx<'_, '_>) {
        if matches!(cx.node(id), Node::VarDecl { .. }) {
            self.check_loop(cx, id);
        } else {
            self.check_new(cx, id);
        }
    }
}
