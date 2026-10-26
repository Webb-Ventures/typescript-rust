//! `unicorn/no-unnecessary-array-flat-map`: `.flatMap(cb)` (plain, non-computed, one
//! non-spread argument) on a receiver not known to be a non-indexed collection (nor, after
//! `.filter(x)`, the filter's receiver).
//!
//! - A simple callback (non-async arrow, no type parameters or return type, one plain
//!   identifier parameter, expression body) returning `[item]` is reported as `map`; after a
//!   plain one-argument `.filter(…)` (no type arguments) it is fixed to `.map(p => item)`.
//! - Returning `test ? [item] : []` (JS files only) is `filter` when `item` is the parameter
//!   (fixed to `.filter(p => test)` unless type arguments, a side effect in `test` or a lost
//!   comment), else `filter().map` with a suggestion `.filter(p => test).map(p => item)`.
//! - Otherwise each `[item]` an inline callback returns (directly, through conditional
//!   branches or a `return`) whose `item` is known not to be an array (literals, objects,
//!   functions, classes, templates, unary/binary/update expressions, primitive annotations,
//!   primitive static values) is an `array-wrapper`: fixed in JS, a suggestion in TS. The
//!   callback must not be async, a generator or have a return type.

use super::prelude::*;
use super::unicorn_ast::method_call;
use super::unicorn_no_useless_boolean_cast::concise_arrow_body_text;
use super::unicorn_prefer_spread::is_ts_file;
use super::unicorn_static::{Static, has_side_effect};
use super::unicorn_text::{
    is_parenthesized, member_object_text, paren_span, paren_text, would_remove_comments,
};
use super::unicorn_types::{Checker, Guess, is_known_non_indexed_collection};
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-unnecessary-array-flat-map",
        "Disallow unnecessary use of `Array#flatMap()`.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create_default::<NoUnnecessaryArrayFlatMap>,
};

#[derive(Debug, Default)]
struct NoUnnecessaryArrayFlatMap;

const MESSAGE_ID: &str = "no-unnecessary-array-flat-map";
const WRAPPER_ID: &str = "no-unnecessary-array-flat-map/array-wrapper";

fn non_array_node(cx: &Ctx<'_, '_>, id: NodeId, _recurse: &dyn Fn(NodeId) -> bool) -> bool {
    match *cx.ast().node(id) {
        Node::StringLit { .. }
        | Node::NumberLit { .. }
        | Node::BoolLit { .. }
        | Node::NullLit {}
        | Node::RegexLit { .. }
        | Node::BigIntLit { .. }
        | Node::ObjectLit { .. }
        | Node::FunctionExpr { .. }
        | Node::Arrow { .. }
        | Node::ClassExpr { .. }
        | Node::TemplateLit { .. }
        | Node::Unary { .. }
        | Node::Update { .. } => true,
        Node::Binary { op, .. } => !matches!(
            op,
            BinaryOp::LogicalAnd | BinaryOp::LogicalOr | BinaryOp::Coalesce
        ),
        _ => false,
    }
}

fn primitive_annotation(ast: &Ast, id: NodeId) -> bool {
    match *ast.node(id) {
        Node::KeywordType { kind } => matches!(
            kind,
            KeywordTypeKind::BigInt
                | KeywordTypeKind::Boolean
                | KeywordTypeKind::Null
                | KeywordTypeKind::Number
                | KeywordTypeKind::String
                | KeywordTypeKind::Symbol
                | KeywordTypeKind::Undefined
        ),
        Node::StringLit { .. }
        | Node::NumberLit { .. }
        | Node::BoolLit { .. }
        | Node::BigIntLit { .. }
        | Node::Unary { .. } => true,
        Node::TemplateLitType { parts } => ast.list(parts).len() <= 1,
        _ => false,
    }
}

fn primitive_static(v: &Static, _ast: &Ast, _id: NodeId) -> Guess {
    match v {
        Static::Undefined
        | Static::Null
        | Static::Bool(_)
        | Static::Num(_)
        | Static::Str(_)
        | Static::BigInt(_) => Guess::Target,
        _ => Guess::Unknown,
    }
}

static NON_ARRAY: Checker = Checker {
    class_heritage: false,
    nullish_in_mixed_union: true,
    target_node: Some(non_array_node),
    target_annotation: Some(primitive_annotation),
    static_type: Some(primitive_static),
    ..Checker::DEFAULT
};

fn has_type_args(ast: &Ast, id: NodeId) -> bool {
    matches!(
        *ast.node(skip_parens(ast, id)),
        Node::Call {
            type_args: Some(_),
            ..
        }
    )
}

fn has_spread(ast: &Ast, args: &[NodeId]) -> bool {
    args.iter()
        .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
}

/// `x.filter(a)` (plain, one argument): `x`.
fn filter_receiver(ast: &Ast, id: NodeId) -> Option<NodeId> {
    let call = method_call(ast, skip_parens(ast, id))?;
    (call.name == "filter" && call.plain() && call.args.len() == 1 && !has_spread(ast, call.args))
        .then_some(call.obj)
}

fn is_flat_map_call(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let Some(call) = method_call(ast, id) else {
        return false;
    };
    call.name == "flatMap"
        && call.plain()
        && call.args.len() == 1
        && !has_spread(ast, call.args)
        && !is_known_non_indexed_collection(cx, call.obj)
        && filter_receiver(ast, call.obj).is_none_or(|r| !is_known_non_indexed_collection(cx, r))
}

/// `[item]`: `item`.
fn single_element(ast: &Ast, id: NodeId) -> Option<NodeId> {
    let Node::ArrayLit { elements } = *ast.node(skip_parens(ast, id)) else {
        return None;
    };
    match *ast.list(elements) {
        [e] if !matches!(ast.node(e), Node::Hole {} | Node::Spread { .. }) => Some(e),
        _ => None,
    }
}

fn is_empty_array(ast: &Ast, id: NodeId) -> bool {
    matches!(*ast.node(skip_parens(ast, id)), Node::ArrayLit { elements } if ast.list(elements).is_empty())
}

/// A non-async arrow with one plain identifier parameter and an expression body:
/// `(param, body)`.
fn simple_callback(ast: &Ast, id: NodeId) -> Option<(NodeId, NodeId)> {
    let Node::Arrow {
        flags,
        type_params: None,
        params,
        return_type: None,
        body,
    } = *ast.node(skip_parens(ast, id))
    else {
        return None;
    };
    let [param] = *ast.list(params) else {
        return None;
    };
    let Node::Param {
        rest: false,
        pattern,
        optional: false,
        init: None,
        ..
    } = *ast.node(param)
    else {
        return None;
    };
    (!flags.contains(FnFlags::ASYNC)
        && matches!(ast.node(pattern), Node::Ident { .. })
        && !matches!(ast.node(body), Node::Block { .. }))
    .then_some((param, body))
}

enum Result {
    Map { element: NodeId, array: NodeId },
    Conditional { test: NodeId, element: NodeId },
}

fn callback_result(ast: &Ast, body: NodeId) -> Option<Result> {
    let body = skip_parens(ast, body);
    if let Some(element) = single_element(ast, body) {
        return Some(Result::Map {
            element,
            array: body,
        });
    }
    let Node::Conditional { test, cons, alt } = *ast.node(body) else {
        return None;
    };
    let element = single_element(ast, cons)?;
    is_empty_array(ast, alt).then_some(Result::Conditional { test, element })
}

fn param_text(cx: &Ctx<'_, '_>, param: NodeId) -> String {
    let text = cx.text(param);
    if matches!(
        *cx.ast().node(param),
        Node::Param {
            type_ann: Some(_),
            ..
        }
    ) {
        format!("({text})")
    } else {
        text.to_owned()
    }
}

fn has_type_annotation(ast: &Ast, param: NodeId) -> bool {
    matches!(
        *ast.node(param),
        Node::Param {
            type_ann: Some(_),
            ..
        }
    )
}

fn node_span(cx: &Ctx<'_, '_>, id: NodeId) -> Span {
    cx.span(skip_parens(cx.ast(), id))
}

impl NoUnnecessaryArrayFlatMap {
    /// The callback-shape report the call gets, if any.
    fn method_problem(&self, cx: &Ctx<'_, '_>, id: NodeId) -> bool {
        let ast = cx.ast();
        let Some(call) = method_call(ast, id) else {
            return false;
        };
        let Some((_, body)) = simple_callback(ast, call.args[0]) else {
            return false;
        };
        match callback_result(ast, body) {
            Some(Result::Map { .. }) => true,
            Some(Result::Conditional { .. }) => !is_ts_file(cx),
            None => false,
        }
    }

    fn check_call(&self, id: NodeId, cx: &mut Ctx<'_, '_>) {
        if !is_flat_map_call(cx, id) {
            return;
        }
        let ast = cx.ast();
        let Some(call) = method_call(ast, id) else {
            return;
        };
        let Some((param, body)) = simple_callback(ast, call.args[0]) else {
            return;
        };
        let Some(result) = callback_result(ast, body) else {
            return;
        };
        let call_span = cx.span(id);
        let prop = call.prop;
        let message = |method: &str| {
            format!("Prefer `.{method}(…)` over `.flatMap(…)` for this single-item array callback.")
        };
        match result {
            Result::Map { element, array } => {
                let fix = filter_receiver(ast, call.obj).and_then(|receiver| {
                    let skip = has_type_args(ast, call.obj)
                        || has_type_args(ast, id)
                        || is_known_non_indexed_collection(cx, receiver)
                        || would_remove_comments(cx, cx.span(array), &[node_span(cx, element)]);
                    (!skip).then(|| {
                        Fix::replace(cx.span(prop), "map")
                            .and(cx.span(array), concise_arrow_body_text(cx, element))
                    })
                });
                let r = cx.report(cx.span(prop), message("map")).id(MESSAGE_ID);
                if let Some(f) = fix {
                    r.fix(f);
                }
            }
            Result::Conditional { test, element } => {
                if is_ts_file(cx) {
                    return;
                }
                let Node::Param { pattern, .. } = *ast.node(param) else {
                    return;
                };
                let element_ident = skip_parens(ast, element);
                let is_param = matches!(ast.node(element_ident), Node::Ident { .. })
                    && ident_name(ast, element_ident) == ident_name(ast, pattern);
                let array_text = || member_object_text(cx, call.obj, id);
                let test_text = concise_arrow_body_text(cx, test);
                if is_param {
                    let skip = has_type_args(ast, id)
                        || has_side_effect(ast, test)
                        || would_remove_comments(
                            cx,
                            call_span,
                            &[node_span(cx, call.obj), cx.span(param), node_span(cx, test)],
                        );
                    let fix = (!skip).then(|| {
                        Fix::replace(
                            call_span,
                            format!(
                                "{}.filter({} => {test_text})",
                                array_text(),
                                param_text(cx, param)
                            ),
                        )
                    });
                    let r = cx.report(cx.span(prop), message("filter")).id(MESSAGE_ID);
                    if let Some(f) = fix {
                        r.fix(f);
                    }
                } else {
                    let skip = has_type_args(ast, id)
                        || has_type_annotation(ast, param)
                        || would_remove_comments(
                            cx,
                            call_span,
                            &[
                                node_span(cx, call.obj),
                                node_span(cx, test),
                                node_span(cx, element),
                            ],
                        )
                        || has_side_effect(ast, test)
                        || has_side_effect(ast, element);
                    let fix = (!skip).then(|| {
                        let p = param_text(cx, param);
                        Fix::replace(
                            call_span,
                            format!(
                                "{}.filter({p} => {test_text}).map({p} => {})",
                                array_text(),
                                concise_arrow_body_text(cx, element)
                            ),
                        )
                    });
                    let r = cx
                        .report(cx.span(prop), message("filter().map"))
                        .id(MESSAGE_ID);
                    if let Some(f) = fix {
                        r.suggest(
                            "no-unnecessary-array-flat-map/filter-map-suggestion",
                            "Replace `.flatMap(…)` with `.filter(…).map(…)`.",
                            f,
                        );
                    }
                }
            }
        }
    }

    fn check_array(&self, id: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some(element) = single_element(ast, id) else {
            return;
        };
        let parents = cx.file.parents;
        let mut returned = id;
        for _ in 0..100_000 {
            let Some(p) = parent_skip_parens(ast, parents, returned) else {
                return;
            };
            match *ast.node(p) {
                Node::Conditional { test, .. } if skip_parens(ast, test) != returned => {
                    returned = p;
                }
                _ => break,
            }
        }
        let Some(mut callback) = parent_skip_parens(ast, parents, returned) else {
            return;
        };
        if matches!(ast.node(callback), Node::Return { .. }) {
            let mut found = None;
            let mut cur = callback;
            for _ in 0..100_000 {
                let Some(p) = cx.parent(cur) else {
                    break;
                };
                if crate::ast_util::is_function(ast.kind(p)) {
                    found = Some(p);
                    break;
                }
                cur = p;
            }
            let Some(f) = found else {
                return;
            };
            callback = f;
        }
        let (flags, return_type) = match *ast.node(callback) {
            Node::Arrow {
                flags, return_type, ..
            }
            | Node::FunctionExpr {
                flags, return_type, ..
            }
            | Node::FunctionDecl {
                flags, return_type, ..
            } => (flags, return_type),
            _ => return,
        };
        if flags.contains(FnFlags::ASYNC)
            || flags.contains(FnFlags::GENERATOR)
            || return_type.is_some()
        {
            return;
        }
        let Some(call) = parent_skip_parens(ast, parents, callback) else {
            return;
        };
        let Node::Call { args, .. } = *ast.node(call) else {
            return;
        };
        if ast.list(args).first().map(|&a| skip_parens(ast, a)) != Some(callback)
            || !is_flat_map_call(cx, call)
            || self.method_problem(cx, call)
            || !NON_ARRAY.is_target(cx, element)
        {
            return;
        }
        let span = cx.span(id);
        let fix = (!would_remove_comments(cx, span, &[paren_span(cx, element)])).then(|| {
            let direct_arrow_body = cx
                .parent(id)
                .is_some_and(|p| matches!(ast.node(p), Node::Arrow { .. }))
                && !is_parenthesized(cx, id);
            let mut text = if direct_arrow_body {
                concise_arrow_body_text(cx, element)
            } else {
                paren_text(cx, element).to_owned()
            };
            if cx.src()[..span.start as usize].ends_with("return") {
                text.insert(0, ' ');
            }
            Fix::replace(span, text)
        });
        let ts = is_ts_file(cx);
        let r = cx
            .report(span, "Wrapping this value in an array is unnecessary.")
            .id(WRAPPER_ID);
        if let Some(f) = fix {
            if ts {
                r.suggest(
                    "no-unnecessary-array-flat-map/array-wrapper-suggestion",
                    "Remove the array wrapper.",
                    f,
                );
            } else {
                r.fix(f);
            }
        }
    }
}

impl Rule for NoUnnecessaryArrayFlatMap {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call, NodeKind::ArrayLit]
    }
    fn enter(&self, id: NodeId, cx: &mut Ctx<'_, '_>) {
        match cx.node(id) {
            Node::Call { .. } => self.check_call(id, cx),
            _ => self.check_array(id, cx),
        }
    }
}
