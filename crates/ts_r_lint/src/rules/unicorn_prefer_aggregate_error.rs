//! `unicorn/prefer-aggregate-error`: `if (errors.length > 0) throw new Error(..)`, where
//! `errors` is a collection of errors, throws away the collected errors.
//!
//! The `if` holds only the `throw` of `new Error(..)` (the global, at most two arguments, no
//! spread). Among the `&&` operands of the test exactly one is a positive length check
//! (`x.length`, `!== 0`, `!= 0`, `> n` with `n >= 0`, `>= n` with `n >= 1`, or flipped) on a
//! name ending in `errors`/`Errors` or `ErrorList`/`ErrorArray`/`ErrorCollection`, known to
//! be an array, whose binding is annotated as an array of builtin errors: `E[]`, a tuple of
//! them, `Array<E>`/`ReadonlyArray<E>`, through `readonly`, type aliases (resolved where
//! they are declared), unions (all members) and intersections (any member). An error type
//! is a builtin error name that isn't declared in the file, or an alias of one. Nothing is
//! reported when a value named `AggregateError` shadows the global.
//!
//! The fix rewrites the `new` expression to `new AggregateError(<collection>, ..args)`,
//! unless that would drop a comment outside the arguments.

use super::prelude::*;
use super::unicorn_conditions::{global_value_available, unwrap_ts_and_parens};
use super::unicorn_no_error_property_assignment::BUILTIN_ERRORS;
use super::unicorn_text::{call_argument_text, paren_span, would_remove_comments};
use super::unicorn_types::{binding_annotation, is_array};
use crate::semantic::{ScopeId, SymbolFlags, SymbolId};
use ts_r_ast::{Ast, TypeOperatorKind};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-aggregate-error",
        "Prefer `AggregateError` when throwing collected errors.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<PreferAggregateError>,
};

#[derive(Debug, Default)]
struct PreferAggregateError;

/// Alias resolution stops this deep, which also bounds cyclic aliases.
const MAX_DEPTH: u32 = 64;

fn is_error_collection_name(name: &str) -> bool {
    let rest = name.strip_suffix("rrors").or_else(|| {
        ["List", "Array", "Collection"]
            .iter()
            .find_map(|s| name.strip_suffix(s))
            .and_then(|n| n.strip_suffix("rror"))
    });
    rest.is_some_and(|r| r.ends_with(['E', 'e']))
}

/// The written type with parentheses, `readonly` and a tuple member's label removed.
fn unwrap_type(ast: &Ast, mut ty: NodeId) -> NodeId {
    for _ in 0..1000 {
        match *ast.node(ty) {
            Node::ParenType { ty: inner }
            | Node::TypeOperator {
                op: TypeOperatorKind::Readonly,
                ty: inner,
            } => ty = inner,
            Node::NamedTupleMember { ty: inner, .. } => ty = inner,
            _ => return ty,
        }
    }
    ty
}

/// `Foo` or `A.B` as written.
fn type_name(ast: &Ast, id: NodeId) -> Option<String> {
    match *ast.node(id) {
        Node::Ident { .. } => ident_name(ast, id).map(str::to_owned),
        Node::QualifiedName { left, right } => Some(format!(
            "{}.{}",
            type_name(ast, left)?,
            ident_name(ast, right)?
        )),
        _ => None,
    }
}

struct Resolver<'c, 'a, 'b> {
    cx: &'c Ctx<'a, 'b>,
    visiting: Vec<SymbolId>,
}

impl Resolver<'_, '_, '_> {
    fn symbol(&self, scope: ScopeId, name: &str) -> Option<SymbolId> {
        let sem = self.cx.semantic();
        sem.lookup(scope, name)
            .filter(|&s| !sem.symbol(s).flags.contains(SymbolFlags::COMMENT_GLOBAL))
    }

    /// The aliased type and the scope it is written in, when `sym` is first declared as a
    /// type alias.
    fn alias(&self, sym: SymbolId) -> Option<(NodeId, ScopeId)> {
        let sem = self.cx.semantic();
        let decl = sem.symbol(sym).decls.first()?;
        if decl.kind != DeclKind::TypeAlias {
            return None;
        }
        let Node::TypeAlias { ty, .. } = *self.cx.node(decl.node) else {
            return None;
        };
        Some((ty, sem.enclosing_scope(decl.name)))
    }

    fn through_alias(
        &mut self,
        sym: Option<SymbolId>,
        depth: u32,
        check: fn(&mut Self, NodeId, ScopeId, u32) -> bool,
    ) -> bool {
        let Some(s) = sym.filter(|s| !self.visiting.contains(s)) else {
            return false;
        };
        let Some((ty, scope)) = self.alias(s) else {
            return false;
        };
        self.visiting.push(s);
        let result = check(self, ty, scope, depth + 1);
        self.visiting.pop();
        result
    }

    fn is_error_type(&mut self, ty: NodeId, scope: ScopeId, depth: u32) -> bool {
        if depth > MAX_DEPTH {
            return false;
        }
        let ast = self.cx.ast();
        let ty = unwrap_type(ast, ty);
        match *ast.node(ty) {
            Node::TypeRef { name, .. } => {
                let Some(name) = type_name(ast, name) else {
                    return false;
                };
                let sym = self.symbol(scope, &name);
                let locally_defined =
                    sym.is_some_and(|s| !self.cx.semantic().symbol(s).decls.is_empty());
                if BUILTIN_ERRORS.contains(&name.as_str()) && !locally_defined {
                    return true;
                }
                self.through_alias(sym, depth, Self::is_error_type)
            }
            Node::UnionType { types } => ast
                .list(types)
                .iter()
                .all(|&t| self.is_error_type(t, scope, depth + 1)),
            Node::IntersectionType { types } => ast
                .list(types)
                .iter()
                .any(|&t| self.is_error_type(t, scope, depth + 1)),
            _ => false,
        }
    }

    fn is_error_array_type(&mut self, ty: NodeId, scope: ScopeId, depth: u32) -> bool {
        if depth > MAX_DEPTH {
            return false;
        }
        let ast = self.cx.ast();
        let ty = unwrap_type(ast, ty);
        match *ast.node(ty) {
            Node::ArrayType { elem } => self.is_error_type(elem, scope, depth + 1),
            Node::TupleType { elems } => {
                let elems = ast.list(elems);
                !elems.is_empty()
                    && elems.iter().all(|&e| match *ast.node(e) {
                        Node::RestType { ty } | Node::NamedTupleMember { rest: true, ty, .. } => {
                            self.is_error_array_type(ty, scope, depth + 1)
                        }
                        _ => self.is_error_type(e, scope, depth + 1),
                    })
            }
            Node::TypeRef { name, type_args } => {
                let Some(name) = type_name(ast, name) else {
                    return false;
                };
                if name == "Array" || name == "ReadonlyArray" {
                    let args = match type_args.map(|a| ast.node(a)) {
                        Some(&Node::TypeArgs { args }) => ast.list(args),
                        _ => &[],
                    };
                    if let [arg] = args {
                        return self.is_error_type(*arg, scope, depth + 1);
                    }
                }
                let sym = self.symbol(scope, &name);
                self.through_alias(sym, depth, Self::is_error_array_type)
            }
            Node::UnionType { types } => ast
                .list(types)
                .iter()
                .all(|&t| self.is_error_array_type(t, scope, depth + 1)),
            Node::IntersectionType { types } => ast
                .list(types)
                .iter()
                .any(|&t| self.is_error_array_type(t, scope, depth + 1)),
            _ => false,
        }
    }
}

/// The binding of `id` is annotated as an array of errors.
fn has_error_collection_evidence(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let sem = cx.semantic();
    let Some(decl) = sem
        .symbol_of_ident(id)
        .and_then(|s| sem.symbol(s).decls.first())
    else {
        return false;
    };
    let Some(ty) = binding_annotation(cx, decl.name).0 else {
        return false;
    };
    let scope = sem.enclosing_scope(decl.name);
    Resolver {
        cx,
        visiting: Vec::new(),
    }
    .is_error_array_type(ty, scope, 0)
}

/// The collection `x` of `x.length`, when it qualifies.
fn length_object(cx: &Ctx<'_, '_>, id: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let Node::Member {
        obj,
        optional: false,
        prop,
    } = *ast.node(unwrap_ts_and_parens(ast, id))
    else {
        return None;
    };
    if ident_name(ast, prop) != Some("length") {
        return None;
    }
    let obj = unwrap_ts_and_parens(ast, obj);
    let name = match ast.node(obj) {
        Node::Ident { .. } => ident_name(ast, obj)?,
        _ => return None,
    };
    (is_error_collection_name(name) && is_array(cx, obj) && has_error_collection_evidence(cx, obj))
        .then_some(obj)
}

fn number_value(ast: &Ast, id: NodeId) -> Option<f64> {
    match *ast.node(unwrap_ts_and_parens(ast, id)) {
        Node::NumberLit { value } => Some(value.get()),
        _ => None,
    }
}

fn is_positive_comparison(ast: &Ast, op: BinaryOp, value: NodeId, length_on_left: bool) -> bool {
    let Some(n) = number_value(ast, value) else {
        return false;
    };
    match op {
        BinaryOp::StrictNotEq | BinaryOp::NotEq => n == 0.0,
        BinaryOp::Gt if length_on_left => n >= 0.0,
        BinaryOp::GtEq if length_on_left => n >= 1.0,
        BinaryOp::Lt if !length_on_left => n >= 0.0,
        BinaryOp::LtEq if !length_on_left => n >= 1.0,
        _ => false,
    }
}

fn positive_length_check(cx: &Ctx<'_, '_>, id: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let id = unwrap_ts_and_parens(ast, id);
    if let Some(obj) = length_object(cx, id) {
        return Some(obj);
    }
    let Node::Binary { left, op, right } = *ast.node(id) else {
        return None;
    };
    if let Some(obj) = length_object(cx, left)
        && is_positive_comparison(ast, op, right, true)
    {
        return Some(obj);
    }
    length_object(cx, right).filter(|_| is_positive_comparison(ast, op, left, false))
}

/// The `&&` operands of `id`, looking through parentheses and TS wrappers at the top.
fn and_operands(ast: &Ast, id: NodeId, out: &mut Vec<NodeId>) {
    let mut stack = vec![unwrap_ts_and_parens(ast, id)];
    while let Some(n) = stack.pop() {
        match *ast.node(n) {
            Node::Binary {
                left,
                op: BinaryOp::LogicalAnd,
                right,
            } if out.len() + stack.len() < 100_000 => {
                stack.push(skip_parens(ast, right));
                stack.push(skip_parens(ast, left));
            }
            _ => out.push(n),
        }
    }
}

impl Rule for PreferAggregateError {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::If]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::If { test, cons, .. } = *cx.node(node) else {
            return;
        };
        let stmt = match *ast.node(cons) {
            Node::Block { stmts } => match ast.list(stmts) {
                [only] => *only,
                _ => return,
            },
            _ => cons,
        };
        let Node::Throw { arg } = *ast.node(stmt) else {
            return;
        };
        let error = skip_parens(ast, arg);
        let Node::New { callee, args, .. } = *ast.node(error) else {
            return;
        };
        let callee = skip_parens(ast, callee);
        let args = ast.list(args);
        if ident_name(ast, callee) != Some("Error")
            || !cx.is_global_ref(callee, "Error")
            || args.len() > 2
            || args
                .iter()
                .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
        {
            return;
        }
        let mut operands = Vec::new();
        and_operands(ast, test, &mut operands);
        let mut found = operands
            .iter()
            .filter_map(|&o| positive_length_check(cx, o));
        let (Some(collection), None) = (found.next(), found.next()) else {
            return;
        };
        if !global_value_available(cx, error, "AggregateError") {
            return;
        }
        let spans: Vec<Span> = args.iter().map(|&a| paren_span(cx, a)).collect();
        let fix = (!would_remove_comments(cx, cx.span(error), &spans)).then(|| {
            let mut parts = vec![cx.text(collection).to_owned()];
            parts.extend(args.iter().map(|&a| call_argument_text(cx, a)));
            Fix::replace(
                cx.span(error),
                format!("new AggregateError({})", parts.join(", ")),
            )
        });
        cx.report(
            cx.span(callee),
            "Use `AggregateError` when throwing collected errors.",
        )
        .id("prefer-aggregate-error")
        .fix_opt(fix);
    }
}
