//! `unicorn/prefer-has-check`: `x.get(key)` (one non-spread argument, non-computed, no
//! optional chaining) used only to test whether the key exists, where the receiver is
//! known from syntax to be a map or a "null sentinel" collection. The receiver is known
//! through a TS cast on it, an inline `new Map/WeakMap/URLSearchParams/Headers/FormData`
//! of the unshadowed global, or an identifier with one declaration whose TS annotation
//! names `Map`/`ReadonlyMap`/`WeakMap` (with its value type argument) or one of the
//! sentinel types (not locally redeclared; non-generic type aliases, `readonly` and
//! parentheses are followed), or a `const` initialized with one of those.
//!
//! A comparison of the call (through TS wrappers) with `==`/`===`/`!=`/`!==`:
//! - sentinel collections report a comparison with `null`, or a loose one with `undefined`
//!   / `void 0`;
//! - maps report a comparison with `undefined` / `void 0` when no stored value can be
//!   `undefined` (`null` too for a loose comparison), and a loose one with `null` when
//!   no value can be nullish.
//!
//! Otherwise a map `get` in a boolean context reports when every stored value is truthy.
//! A map's values are judged from `new Map()` without entries or with literal `[key,
//! value]` entries (static values, or array/object/function/class/`new` expressions), the
//! map's TS value type argument (keywords, literal types, arrays, tuples, functions,
//! `object`, `symbol`, type literals and local classes/interfaces when only presence
//! matters, unions and intersections of those, followed through non-generic aliases), or
//! the declaration it comes from.
//!
//! The comparison fix writes `[!]receiver.has(argument)` (no fix when the comparison holds
//! comments); the boolean fix renames `get` to `has`. Type-checker branches of the plugin
//! are not implemented.

use super::prelude::*;
use super::unicorn_ast::method_call;
use super::unicorn_no_mismatched_map_key::binding;
use super::unicorn_static::{Static, is_global_ident, static_value_no_side_effects};
use super::unicorn_text::{member_object_text, needs_semicolon, paren_text, would_remove_comments};
use super::unicorn_types::{binding_annotation, type_definition};
use super::unicorn_util_h5::is_boolean_context;
use crate::semantic::{DeclKind, SymbolId};
use ts_r_ast::{Ast, BinaryOp, KeywordTypeKind, NodeList, TypeOperatorKind, UnaryOp};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-has-check",
        "Prefer `.has()` when checking existence.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<PreferHasCheck>,
};

#[derive(Debug, Default)]
struct PreferHasCheck;

const MAX_DEPTH: usize = 64;
const MESSAGE: &str = "Prefer `.has(…)` when checking existence.";

const MAP_TYPES: &[&str] = &["Map", "ReadonlyMap", "WeakMap"];
const MAP_CONSTRUCTORS: &[&str] = &["Map", "WeakMap"];
const SENTINEL_TYPES: &[&str] = &["FormData", "Headers", "URLSearchParams"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Collection {
    /// A map and its value type argument.
    Map(Option<NodeId>),
    /// `get` returns `null` for a missing name.
    NullSentinel,
}

/// What a stored value must never be for the check to be replaceable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Need {
    Truthy,
    NotUndefined,
    NotNullish,
}

fn from_type_name(name: &str, value: Option<NodeId>) -> Option<Collection> {
    if MAP_TYPES.contains(&name) {
        Some(Collection::Map(value))
    } else if SENTINEL_TYPES.contains(&name) {
        Some(Collection::NullSentinel)
    } else {
        None
    }
}

fn type_args_last(ast: &Ast, type_args: Option<NodeId>) -> Option<NodeId> {
    match *ast.node(type_args?) {
        Node::TypeArgs { args } => ast.list(args).last().copied(),
        _ => None,
    }
}

/// The aliased type of a non-generic `type` alias declaration.
fn non_generic_alias(ast: &Ast, decl: crate::semantic::Decl) -> Option<NodeId> {
    if decl.kind != DeclKind::TypeAlias {
        return None;
    }
    let Node::TypeAlias {
        type_params, ty, ..
    } = *ast.node(decl.node)
    else {
        return None;
    };
    let generic = type_params.is_some_and(|tp| match *ast.node(tp) {
        Node::TypeParams { params } => !ast.list(params).is_empty(),
        _ => false,
    });
    (!generic).then_some(ty)
}

fn type_ref_name<'a>(cx: &Ctx<'a, '_>, name: NodeId) -> Option<&'a str> {
    match cx.node(name) {
        Node::Ident { .. } => Some(cx.text(name)),
        _ => None,
    }
}

fn collection_from_type(
    cx: &Ctx<'_, '_>,
    ty: Option<NodeId>,
    visited: &mut Vec<NodeId>,
) -> Option<Collection> {
    let ast = cx.ast();
    let ty = ty?;
    if visited.len() > MAX_DEPTH {
        return None;
    }
    match *ast.node(ty) {
        Node::ParenType { ty } => collection_from_type(cx, Some(ty), visited),
        Node::TypeOperator {
            op: TypeOperatorKind::Readonly,
            ty,
        } => collection_from_type(cx, Some(ty), visited),
        Node::TypeRef { name, type_args } => {
            let n = type_ref_name(cx, name)?;
            let scope = cx.semantic().enclosing_scope(name);
            let definition = type_definition(cx, n, scope);
            if (MAP_TYPES.contains(&n) || SENTINEL_TYPES.contains(&n)) && definition.is_none() {
                return from_type_name(n, type_args_last(ast, type_args));
            }
            let (_, decl) = definition?;
            if visited.contains(&decl.node) {
                return None;
            }
            visited.push(decl.node);
            let alias = non_generic_alias(ast, decl)?;
            collection_from_type(cx, Some(alias), visited)
        }
        _ => None,
    }
}

/// The type of a TS cast written on the expression itself.
fn expression_annotation(cx: &Ctx<'_, '_>, id: NodeId) -> Option<Collection> {
    match *cx.node(skip_parens(cx.ast(), id)) {
        Node::As { ty, .. } | Node::Satisfies { ty, .. } | Node::TypeAssertion { ty, .. } => {
            collection_from_type(cx, Some(ty), &mut Vec::new())
        }
        _ => None,
    }
}

fn unwrap_ts(ast: &Ast, mut id: NodeId) -> NodeId {
    for _ in 0..10_000 {
        match *ast.node(id) {
            Node::Paren { expr }
            | Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::NonNull { expr }
            | Node::TypeAssertion { expr, .. } => id = expr,
            _ => break,
        }
    }
    id
}

/// `new Name(...)` of one of the unshadowed globals `names`.
fn global_new<'a>(
    cx: &Ctx<'_, '_>,
    id: NodeId,
    names: &[&'a str],
) -> Option<(&'a str, Option<NodeId>, NodeList)> {
    let Node::New {
        callee,
        type_args,
        args,
        ..
    } = *cx.node(id)
    else {
        return None;
    };
    let callee = skip_parens(cx.ast(), callee);
    let name = names
        .iter()
        .find(|n| is_global_ident(cx, callee, n))
        .copied()?;
    Some((name, type_args, args))
}

/// The variable an identifier names, when it has a single declaration.
fn single_variable(cx: &Ctx<'_, '_>, id: NodeId, visited: &[SymbolId]) -> Option<SymbolId> {
    if !matches!(cx.node(id), Node::Ident { .. }) {
        return None;
    }
    let sym = binding(cx, id)?;
    (!visited.contains(&sym) && cx.semantic().symbol(sym).decls.len() == 1).then_some(sym)
}

/// The initializer of a `const name = init` declaration.
fn const_init(cx: &Ctx<'_, '_>, decl: crate::semantic::Decl) -> Option<NodeId> {
    if decl.kind != DeclKind::Const {
        return None;
    }
    match *cx.node(decl.node) {
        Node::VarDeclarator {
            name,
            init: Some(init),
            ..
        } if name == decl.name => Some(init),
        _ => None,
    }
}

fn collection_from_syntax(
    cx: &Ctx<'_, '_>,
    id: NodeId,
    visited: &mut Vec<SymbolId>,
) -> Option<Collection> {
    if let Some(c) = expression_annotation(cx, id) {
        return Some(c);
    }
    let ast = cx.ast();
    let id = unwrap_ts(ast, id);
    let constructible = [MAP_CONSTRUCTORS, SENTINEL_TYPES].concat();
    if let Some((name, _, _)) = global_new(cx, id, &constructible) {
        return from_type_name(name, None);
    }
    if visited.len() > MAX_DEPTH {
        return None;
    }
    let sym = single_variable(cx, id, visited)?;
    visited.push(sym);
    let decl = cx.semantic().symbol(sym).decls[0];
    let (annotation, _) = binding_annotation(cx, decl.name);
    if let Some(c) = collection_from_type(cx, annotation, &mut Vec::new()) {
        return Some(c);
    }
    collection_from_syntax(cx, const_init(cx, decl)?, visited)
}

// ---- Stored values -------------------------------------------------------------------------

fn safe_static(value: &Static, need: Need) -> bool {
    match need {
        Need::Truthy => value.truthy(),
        Need::NotUndefined => !matches!(value, Static::Undefined),
        Need::NotNullish => !matches!(value, Static::Undefined | Static::Null),
    }
}

fn safe_expression(cx: &Ctx<'_, '_>, id: NodeId, need: Need) -> bool {
    let ast = cx.ast();
    let id = skip_parens(ast, id);
    if let Some(value) = static_value_no_side_effects(cx, id) {
        return safe_static(&value, need);
    }
    if matches!(ast.node(id), Node::Ident { .. }) && cx.text(id) == "undefined" {
        return false;
    }
    matches!(
        ast.node(id),
        Node::ArrayLit { .. }
            | Node::Arrow { .. }
            | Node::ClassExpr { .. }
            | Node::FunctionExpr { .. }
            | Node::New { .. }
            | Node::ObjectLit { .. }
    )
}

/// The entries of `new Map(...)`: `Some(true)` when every stored value is safe.
fn constructor_safety(cx: &Ctx<'_, '_>, id: NodeId, need: Need) -> Option<bool> {
    let ast = cx.ast();
    let (_, _, args) = global_new(cx, id, MAP_CONSTRUCTORS)?;
    let args = ast.list(args);
    match args {
        [] => Some(true),
        [entries] => {
            let Node::ArrayLit { elements } = *ast.node(skip_parens(ast, *entries)) else {
                return None;
            };
            Some(ast.list(elements).iter().all(|&e| {
                let Node::ArrayLit { elements } = *ast.node(skip_parens(ast, e)) else {
                    return false;
                };
                let pair = ast.list(elements);
                pair.len() >= 2
                    && !matches!(ast.node(pair[1]), Node::Hole {} | Node::Spread { .. })
                    && safe_expression(cx, pair[1], need)
            }))
        }
        _ => None,
    }
}

fn literal_type_truthy(ast: &Ast, id: NodeId) -> Option<bool> {
    Some(match *ast.node(id) {
        Node::StringLit { value } => !ast.atom(value).is_empty(),
        Node::NumberLit { value } => value.get() != 0.0 && !value.get().is_nan(),
        Node::BigIntLit { value } => !is_zero_bigint(ast.atom(value)),
        Node::BoolLit { value } => value,
        Node::Unary {
            op: UnaryOp::Minus,
            arg,
        } => match *ast.node(arg) {
            Node::NumberLit { value } => value.get() != 0.0 && !value.get().is_nan(),
            Node::BigIntLit { value } => !is_zero_bigint(ast.atom(value)),
            _ => return None,
        },
        Node::TemplateLitType { parts } => match ast.list(parts) {
            [] => false,
            [only] => match *ast.node(*only) {
                Node::TemplateElement { cooked, .. } => !ast.atom(cooked).is_empty(),
                _ => return None,
            },
            _ => return None,
        },
        _ => return None,
    })
}

fn is_zero_bigint(text: &str) -> bool {
    let digits = text
        .strip_prefix("0x")
        .or_else(|| text.strip_prefix("0X"))
        .or_else(|| text.strip_prefix("0b"))
        .or_else(|| text.strip_prefix("0B"))
        .or_else(|| text.strip_prefix("0o"))
        .or_else(|| text.strip_prefix("0O"))
        .unwrap_or(text);
    digits.chars().all(|c| c == '0' || c == '_')
}

fn safe_value_type(cx: &Ctx<'_, '_>, ty: NodeId, need: Need, visited: &mut Vec<NodeId>) -> bool {
    let ast = cx.ast();
    if visited.len() > MAX_DEPTH {
        return false;
    }
    match *ast.node(ty) {
        Node::ParenType { ty } => safe_value_type(cx, ty, need, visited),
        Node::KeywordType { kind } => match kind {
            KeywordTypeKind::Null => need == Need::NotUndefined,
            KeywordTypeKind::BigInt
            | KeywordTypeKind::Boolean
            | KeywordTypeKind::Number
            | KeywordTypeKind::String => need != Need::Truthy,
            KeywordTypeKind::Object | KeywordTypeKind::Symbol => true,
            _ => false,
        },
        Node::TypeOperator { op, ty } => {
            op == TypeOperatorKind::Readonly && safe_value_type(cx, ty, need, visited)
        }
        Node::UnionType { types } | Node::IntersectionType { types } => ast
            .list(types)
            .iter()
            .all(|&t| safe_value_type(cx, t, need, visited)),
        Node::ArrayType { .. }
        | Node::ConstructorType { .. }
        | Node::FunctionType { .. }
        | Node::TupleType { .. } => true,
        Node::TypeLiteral { .. } => need != Need::Truthy,
        Node::TypeRef { name, .. } => {
            let Some(n) = type_ref_name(cx, name) else {
                return false;
            };
            let scope = cx.semantic().enclosing_scope(name);
            let Some((_, decl)) = type_definition(cx, n, scope) else {
                return false;
            };
            if visited.contains(&decl.node) {
                return false;
            }
            visited.push(decl.node);
            let safe = if let Some(alias) = non_generic_alias(ast, decl) {
                safe_value_type(cx, alias, need, visited)
            } else if matches!(decl.kind, DeclKind::Class | DeclKind::Interface) {
                need != Need::Truthy
            } else {
                false
            };
            visited.pop();
            safe
        }
        _ => match literal_type_truthy(ast, ty) {
            Some(truthy) => need != Need::Truthy || truthy,
            None => false,
        },
    }
}

fn safe_map_values_from_syntax(
    cx: &Ctx<'_, '_>,
    id: NodeId,
    need: Need,
    visited: &mut Vec<SymbolId>,
) -> bool {
    if let Some(Collection::Map(value)) = expression_annotation(cx, id) {
        return value.is_some_and(|v| safe_value_type(cx, v, need, &mut Vec::new()));
    }
    let ast = cx.ast();
    let id = unwrap_ts(ast, id);
    if let Some((_, type_args, _)) = global_new(cx, id, MAP_CONSTRUCTORS) {
        return type_args_last(ast, type_args)
            .is_some_and(|v| safe_value_type(cx, v, need, &mut Vec::new()));
    }
    if visited.len() > MAX_DEPTH {
        return false;
    }
    let Some(sym) = single_variable(cx, id, visited) else {
        return false;
    };
    visited.push(sym);
    let decl = cx.semantic().symbol(sym).decls[0];
    let (annotation, _) = binding_annotation(cx, decl.name);
    let safe = if let Some(Collection::Map(value)) =
        collection_from_type(cx, annotation, &mut Vec::new())
    {
        value.is_some_and(|v| safe_value_type(cx, v, need, &mut Vec::new()))
    } else {
        const_init(cx, decl)
            .is_some_and(|init| safe_map_values_from_syntax(cx, init, need, visited))
    };
    visited.pop();
    safe
}

fn safe_map_values(cx: &Ctx<'_, '_>, receiver: NodeId, need: Need) -> bool {
    if let Some(safe) = constructor_safety(cx, unwrap_ts(cx.ast(), receiver), need) {
        return safe;
    }
    safe_map_values_from_syntax(cx, receiver, need, &mut Vec::new())
}

// ---- The comparison ------------------------------------------------------------------------

fn is_undefined_sentinel(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let id = skip_parens(ast, id);
    if is_global_ident(cx, id, "undefined") {
        return true;
    }
    match *ast.node(id) {
        Node::Unary {
            op: UnaryOp::Void,
            arg,
        } => {
            matches!(*ast.node(skip_parens(ast, arg)), Node::NumberLit { value } if value.get() == 0.0)
        }
        _ => false,
    }
}

fn is_null(ast: &Ast, id: NodeId) -> bool {
    matches!(ast.node(skip_parens(ast, id)), Node::NullLit {})
}

struct Comparison {
    node: NodeId,
    op: BinaryOp,
    value: NodeId,
}

fn comparison(cx: &Ctx<'_, '_>, call: NodeId) -> Option<Comparison> {
    let ast = cx.ast();
    let mut target = call;
    for _ in 0..10_000 {
        let p = cx.parent(target)?;
        match *ast.node(p) {
            Node::Paren { .. }
            | Node::As { .. }
            | Node::Satisfies { .. }
            | Node::NonNull { .. }
            | Node::TypeAssertion { .. }
            | Node::Instantiation { .. } => target = p,
            Node::Binary { left, op, right }
                if matches!(
                    op,
                    BinaryOp::Eq | BinaryOp::StrictEq | BinaryOp::NotEq | BinaryOp::StrictNotEq
                ) =>
            {
                let value = if left == target {
                    right
                } else if right == target {
                    left
                } else {
                    return None;
                };
                return Some(Comparison { node: p, op, value });
            }
            _ => return None,
        }
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Missing {
    Undefined,
    Null,
}

fn missing_value(cx: &Ctx<'_, '_>, receiver: NodeId, cmp: &Comparison) -> Option<Missing> {
    let collection = collection_from_syntax(cx, receiver, &mut Vec::new())?;
    let loose = matches!(cmp.op, BinaryOp::Eq | BinaryOp::NotEq);
    match collection {
        Collection::NullSentinel => Some(if loose && is_undefined_sentinel(cx, cmp.value) {
            Missing::Undefined
        } else {
            Missing::Null
        }),
        Collection::Map(_) => {
            let need = if loose {
                Need::NotNullish
            } else {
                Need::NotUndefined
            };
            if is_undefined_sentinel(cx, cmp.value) && safe_map_values(cx, receiver, need) {
                Some(Missing::Undefined)
            } else if is_null(cx.ast(), cmp.value)
                && loose
                && safe_map_values(cx, receiver, Need::NotNullish)
            {
                Some(Missing::Null)
            } else {
                None
            }
        }
    }
}

impl Rule for PreferHasCheck {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, id: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some(call) = method_call(ast, id) else {
            return;
        };
        if call.name != "get"
            || call.optional_call
            || call.optional_member
            || call.args.len() != 1
            || matches!(ast.node(call.args[0]), Node::Spread { .. })
        {
            return;
        }
        let (receiver, prop, arg) = (call.obj, call.prop, call.args[0]);
        if let Some(cmp) = comparison(cx, id) {
            let Some(missing) = missing_value(cx, receiver, &cmp) else {
                return;
            };
            let matches = match missing {
                Missing::Undefined => is_undefined_sentinel(cx, cmp.value),
                Missing::Null => is_null(ast, cmp.value),
            };
            if !matches {
                return;
            }
            let span = cx.span(cmp.node);
            let fix = (!would_remove_comments(cx, span, &[])).then(|| {
                let positive = matches!(cmp.op, BinaryOp::NotEq | BinaryOp::StrictNotEq);
                let object =
                    member_object_text(cx, receiver, if positive { cmp.node } else { receiver });
                let text = format!(
                    "{}{object}.has({})",
                    if positive { "" } else { "!" },
                    paren_text(cx, skip_parens(ast, arg))
                );
                let semi = if needs_semicolon(cx, span.start, &text) {
                    ";"
                } else {
                    ""
                };
                Fix::replace(span, format!("{semi}{text}"))
            });
            let r = cx.report(cx.span(prop), MESSAGE).id("prefer-has-check");
            if let Some(fix) = fix {
                r.fix(fix);
            }
            return;
        }
        if is_boolean_context(cx, id)
            && matches!(
                collection_from_syntax(cx, receiver, &mut Vec::new()),
                Some(Collection::Map(_))
            )
            && safe_map_values(cx, receiver, Need::Truthy)
        {
            let span = cx.span(prop);
            cx.report(span, MESSAGE)
                .id("prefer-has-check")
                .fix(Fix::replace(span, "has".to_string()));
        }
    }
}
