//! `unicorn/prefer-direct-iteration`: `x.entries()` / `x.keys()` / `x.values()` (plain, no
//! arguments) whose result is consumed as an iterable (a `for…of` iterable, `yield*`, a
//! spread in an array, call or `new`, the only argument of `new Map/WeakMap/Set/WeakSet/
//! <TypedArray>(..)`, the first of `Array.from`, `<TypedArray>.from`, `Object.fromEntries`,
//! or the only one of `Promise.all/allSettled/any/race`), when the method is the default
//! iterator of every type `x` can be: `values` for arrays, typed arrays and sets, `keys` for
//! sets, `entries` for maps, `FormData` and `URLSearchParams`.
//!
//! `x`'s types come from syntax first: array literals, `[new] Array(..)`, `Array.from/of`,
//! `new <TypedArray|Map|Set|FormData|URLSearchParams>(..)`, `<TypedArray>.from/of`, a TS
//! assertion's type (else its operand), `!`, and a variable with one declaration whose
//! binding is annotated (array, tuple, `readonly`, the built-in names and their `Readonly`
//! forms, unions of those) or is a plain `const` with an initializer. Otherwise the checker
//! decides (not `any`/`unknown`; unions member by member; arrays and tuples; default-library
//! symbols by name). The fix removes the method call unless a comment is inside it.

use super::prelude::*;
use super::unicorn_ast::method_call;
use super::unicorn_text::has_comment_in;
use super::unicorn_util_h2::remove_method_call;
use crate::semantic::{Resolution, SymbolId};
use crate::types::{Ty, TypeFlags};
use ts_r_ast::{Ast, TypeOperatorKind};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-direct-iteration",
        "Prefer direct iteration over default iterator method calls.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<PreferDirectIteration>,
};

#[derive(Debug, Default)]
struct PreferDirectIteration;

const TYPED_ARRAYS: &[&str] = &[
    "Int8Array",
    "Uint8Array",
    "Uint8ClampedArray",
    "Int16Array",
    "Uint16Array",
    "Int32Array",
    "Uint32Array",
    "Float16Array",
    "Float32Array",
    "Float64Array",
    "BigInt64Array",
    "BigUint64Array",
];

const ARRAY: u32 = 1;
const MAP: u32 = 1 << 1;
const SET: u32 = 1 << 2;
const FORM_DATA: u32 = 1 << 3;
const URL_SEARCH_PARAMS: u32 = 1 << 4;
const TYPED: u32 = 1 << 5;

/// A set of built-in types as a bit mask (never empty when `Some`).
type Types = Option<u32>;

fn type_of_name(name: &str) -> Types {
    match name {
        "Array" | "ReadonlyArray" => Some(ARRAY),
        "Map" | "ReadonlyMap" => Some(MAP),
        "Set" | "ReadonlySet" => Some(SET),
        "FormData" => Some(FORM_DATA),
        "URLSearchParams" => Some(URL_SEARCH_PARAMS),
        _ if TYPED_ARRAYS.contains(&name) => Some(TYPED),
        _ => None,
    }
}

fn allowed(method: &str) -> u32 {
    match method {
        "values" => ARRAY | SET | TYPED,
        "keys" => SET,
        "entries" => MAP | FORM_DATA | URL_SEARCH_PARAMS,
        _ => 0,
    }
}

fn merge(parts: impl Iterator<Item = Types>) -> Types {
    let mut all = 0;
    for p in parts {
        all |= p?;
    }
    (all != 0).then_some(all)
}

fn annotation_types(ast: &Ast, ty: Option<NodeId>, depth: u32) -> Types {
    let ty = ty?;
    if depth > 200 {
        return None;
    }
    match *ast.node(ty) {
        Node::ParenType { ty } => annotation_types(ast, Some(ty), depth + 1),
        Node::ArrayType { .. } | Node::TupleType { .. } => Some(ARRAY),
        Node::TypeOperator {
            op: TypeOperatorKind::Readonly,
            ty,
        } => annotation_types(ast, Some(ty), depth + 1),
        Node::TypeRef { name, .. } => match ast.node(name) {
            Node::Ident { .. } => type_of_name(ident_name(ast, name)?),
            _ => None,
        },
        Node::UnionType { types } => merge(
            ast.list(types)
                .iter()
                .map(|&t| annotation_types(ast, Some(t), depth + 1)),
        ),
        _ => None,
    }
}

fn checker_types(cx: &Ctx<'_, '_>, t: Ty, depth: u32) -> Types {
    let ti = cx.types();
    let flags = ti.flags(t);
    if depth > 50 || flags.intersects(TypeFlags::ANY | TypeFlags::UNKNOWN) {
        return None;
    }
    if flags.contains(TypeFlags::UNION) {
        return merge(
            ti.union_members(t)
                .into_iter()
                .map(|m| checker_types(cx, m, depth + 1)),
        );
    }
    if ti.is_array(t) || ti.is_tuple(t) {
        return Some(ARRAY);
    }
    let sym = ti.type_symbol(t).or_else(|| ti.alias_symbol(t))?;
    if !ti.symbol_decls(sym).iter().any(|d| d.in_lib) {
        return None;
    }
    type_of_name(&ti.symbol_name(sym))
}

/// `new Name(..)` with `Name` one of `names` or a typed array: the name.
fn is_new_of<'a>(ast: &'a Ast, id: NodeId, names: &[&str]) -> Option<&'a str> {
    let Node::New { callee, .. } = *ast.node(id) else {
        return None;
    };
    let callee = skip_parens(ast, callee);
    if !matches!(ast.node(callee), Node::Ident { .. }) {
        return None;
    }
    let name = ident_name(ast, callee)?;
    (names.contains(&name) || TYPED_ARRAYS.contains(&name)).then_some(name)
}

fn is_named(ast: &Ast, id: NodeId, name: &str) -> bool {
    let id = skip_parens(ast, id);
    matches!(ast.node(id), Node::Ident { .. }) && ident_name(ast, id) == Some(name)
}

/// `Name.from/of(..)` with `Name` one of `names`: the name.
fn static_from_of<'a>(ast: &'a Ast, id: NodeId, names: &[&str]) -> Option<&'a str> {
    let call = method_call(ast, id)?;
    if !matches!(call.name, "from" | "of") || !call.plain() {
        return None;
    }
    let obj = skip_parens(ast, call.obj);
    if !matches!(ast.node(obj), Node::Ident { .. }) {
        return None;
    }
    let name = ident_name(ast, obj)?;
    names.contains(&name).then_some(name)
}

fn binding_annotation(cx: &Ctx<'_, '_>, name: NodeId) -> Option<NodeId> {
    let parent = cx.parent(name)?;
    match *cx.node(parent) {
        Node::VarDeclarator {
            name: n, type_ann, ..
        } if n == name => type_ann,
        Node::Param {
            pattern, type_ann, ..
        } if pattern == name => type_ann,
        _ => None,
    }
}

fn variable_types(cx: &Ctx<'_, '_>, ident: NodeId, visited: &mut Vec<SymbolId>) -> Types {
    let sem = cx.semantic();
    let Resolution::Symbol(sym) = sem.reference(sem.reference_at(ident)?).resolution else {
        return None;
    };
    if visited.contains(&sym) {
        return None;
    }
    visited.push(sym);
    let symbol = sem.symbol(sym);
    let [decl] = symbol.decls[..] else {
        return None;
    };
    if let Some(t) = annotation_types(cx.ast(), binding_annotation(cx, decl.name), 0) {
        return Some(t);
    }
    let ast = cx.ast();
    let Node::VarDeclarator {
        name,
        init: Some(init),
        ..
    } = *ast.node(decl.node)
    else {
        return None;
    };
    let is_const = matches!(
        *ast.node(decl.parent),
        Node::VarDecl {
            kind: VarKind::Const,
            ..
        }
    );
    if !is_const || name != decl.name {
        return None;
    }
    types_of(cx, init, visited)
}

fn syntax_types(cx: &Ctx<'_, '_>, id: NodeId, visited: &mut Vec<SymbolId>) -> Types {
    let ast = cx.ast();
    if matches!(ast.node(id), Node::ArrayLit { .. }) {
        return Some(ARRAY);
    }
    if let Some(name) = is_new_of(
        ast,
        id,
        &["Array", "Map", "Set", "FormData", "URLSearchParams"],
    ) {
        return type_of_name(name);
    }
    if let Node::Call {
        callee,
        optional: false,
        ..
    } = *ast.node(id)
        && is_named(ast, callee, "Array")
    {
        return Some(ARRAY);
    }
    if static_from_of(ast, id, &["Array"]).is_some() {
        return Some(ARRAY);
    }
    if let Some(name) = static_from_of(ast, id, TYPED_ARRAYS) {
        return type_of_name(name);
    }
    match *ast.node(id) {
        Node::Paren { expr } => types_of(cx, expr, visited),
        Node::Ident { .. } => variable_types(cx, id, visited),
        Node::As { expr, ty } | Node::Satisfies { expr, ty } | Node::TypeAssertion { ty, expr } => {
            annotation_types(ast, Some(ty), 0).or_else(|| types_of(cx, expr, visited))
        }
        Node::NonNull { expr } => types_of(cx, expr, visited),
        _ => None,
    }
}

fn types_of(cx: &Ctx<'_, '_>, id: NodeId, visited: &mut Vec<SymbolId>) -> Types {
    if visited.len() > 100 {
        return None;
    }
    syntax_types(cx, id, visited).or_else(|| {
        let ti = cx.types();
        if !ti.is_available() {
            return None;
        }
        checker_types(cx, ti.type_at(skip_parens(cx.ast(), id))?, 0)
    })
}

fn is_iterable_consumer(cx: &Ctx<'_, '_>, call: NodeId) -> bool {
    let ast = cx.ast();
    let Some(parent) = parent_skip_parens(ast, cx.file.parents, call) else {
        return false;
    };
    let only_arg = |args: ts_r_ast::NodeList| {
        let args = ast.list(args);
        args.len() == 1 && skip_parens(ast, args[0]) == call
    };
    match *ast.node(parent) {
        Node::ForOf { right, .. } => skip_parens(ast, right) == call,
        Node::Yield {
            delegate: true,
            arg: Some(arg),
        } => skip_parens(ast, arg) == call,
        Node::Spread { .. } => parent_skip_parens(ast, cx.file.parents, parent).is_some_and(|p| {
            matches!(
                ast.node(p),
                Node::ArrayLit { .. } | Node::Call { .. } | Node::New { .. }
            )
        }),
        Node::New { callee, args, .. } => {
            only_arg(args)
                && ["Map", "WeakMap", "Set", "WeakSet"]
                    .iter()
                    .chain(TYPED_ARRAYS)
                    .any(|n| is_named(ast, callee, n))
        }
        Node::Call { args, .. } => {
            let Some(c) = method_call(ast, parent) else {
                return false;
            };
            let list = ast.list(args);
            if list.first().map(|&a| skip_parens(ast, a)) != Some(call) || !c.plain() {
                return false;
            }
            let on = |name: &str| is_named(ast, c.obj, name);
            (c.name == "from" && (on("Array") || TYPED_ARRAYS.iter().any(|n| on(n))))
                || (c.name == "fromEntries" && on("Object"))
                || (matches!(c.name, "all" | "allSettled" | "any" | "race")
                    && on("Promise")
                    && list.len() == 1)
        }
        _ => false,
    }
}

impl Rule for PreferDirectIteration {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, id: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some(call) = method_call(ast, id) else {
            return;
        };
        if !matches!(call.name, "entries" | "keys" | "values")
            || !call.args.is_empty()
            || !call.plain()
            || !is_iterable_consumer(cx, id)
        {
            return;
        }
        let Some(types) = types_of(cx, call.obj, &mut Vec::new()) else {
            return;
        };
        if types & !allowed(call.name) != 0 {
            return;
        }
        let span = cx.span(id);
        let fix =
            (!has_comment_in(cx, span)).then(|| remove_method_call_into(cx, id, Fix::default()));
        let r = cx
            .report(
                cx.span(call.prop),
                format!("Prefer direct iteration instead of `.{}()`.", call.name),
            )
            .id("prefer-direct-iteration");
        if let Some(f) = fix {
            r.fix(f);
        }
    }
}
