//! `@typescript-eslint/no-unsafe-type-assertion`: `x as T` / `<T>x` where the expression's
//! type is not assignable to `T` (a narrowing), or where an `any` goes in or out.

use super::prelude::*;
use crate::type_util::{is_any, is_unknown, is_unsafe_assignment};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-unsafe-type-assertion",
        "Disallow type assertions that narrow a type",
        NO_PRESET,
        Category::Correctness,
    )
    .typed(),
    create: create_default::<NoUnsafeTypeAssertion>,
};

#[derive(Debug, Default)]
struct NoUnsafeTypeAssertion;

/// Assigning `sender` to `receiver` lets an `any` through: `sender` is `any` itself (and
/// `receiver` neither `any` nor `unknown`), or a type argument of the same generic is. A
/// `new X()` without type arguments is inferred, so only its top level counts.
fn unsafe_any(
    ti: &dyn TypeInfo,
    ast: &ts_r_ast::Ast,
    sender: Ty,
    receiver: Ty,
    node: NodeId,
) -> bool {
    if is_any(ti, sender) {
        return !is_any(ti, receiver) && !is_unknown(ti, receiver);
    }
    if let Node::New {
        callee,
        type_args: None,
        ..
    } = *ast.node(skip_parens(ast, node))
        && matches!(ast.node(callee), Node::Ident { .. })
    {
        return false;
    }
    is_unsafe_assignment(ti, sender, receiver)
}

/// A type parameter whose constraint chain ends in a declared constraint (`T extends
/// unknown` counts; `V extends T` with `T` unconstrained doesn't).
fn has_constraint(ti: &dyn TypeInfo, mut t: Ty) -> bool {
    for _ in 0..16 {
        let Some(s) = ti.type_symbol(t) else {
            return false;
        };
        let constraint = ti.symbol_decls(s).iter().find_map(|d| {
            let (ast, _) = ti.decl_file(d)?;
            match *ast.node(d.node) {
                Node::TypeParam { constraint, .. } => Some((constraint?, d.in_this_file)),
                _ => None,
            }
        });
        let Some((c, here)) = constraint else {
            return false;
        };
        // Follow a constraint that is itself a type parameter (known for this file).
        match here.then(|| ti.type_of_type_node(c)).flatten() {
            Some(ct) if ct != t && ti.flags(ct).contains(TypeFlags::TYPE_PARAMETER) => t = ct,
            _ => return true,
        }
    }
    true
}

fn any_name(ti: &dyn TypeInfo, t: Ty) -> &'static str {
    if ti.is_error(t) {
        "error typed"
    } else {
        "`any`"
    }
}

impl Rule for NoUnsafeTypeAssertion {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::As, NodeKind::TypeAssertion]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let (expr, ty) = match *cx.node(node) {
            Node::As { expr, ty } | Node::TypeAssertion { ty, expr } => (expr, ty),
            _ => return,
        };
        let ti = cx.types();
        let (Some(from), Some(to)) = (ti.type_at(expr), ti.type_of_type_node(ty)) else {
            return;
        };
        if from == to || from == Ty::UNRESOLVED || to == Ty::UNRESOLVED {
            return;
        }
        if is_any(ti, to) && is_unknown(ti, from) {
            cx.report_node(
                node,
                "Unsafe assertion to `any` detected: consider using a more specific type to ensure safety.",
            )
            .id("unsafeToAnyTypeAssertion");
            return;
        }
        if unsafe_any(ti, ast, from, to, expr) {
            let msg = format!(
                "Unsafe assertion from {} detected: consider using type guards or a safer assertion.",
                any_name(ti, from)
            );
            cx.report_node(node, msg).id("unsafeOfAnyTypeAssertion");
            return;
        }
        if unsafe_any(ti, ast, to, from, ty) {
            let msg = format!(
                "Unsafe assertion to {} detected: consider using a more specific type to ensure safety.",
                any_name(ti, to)
            );
            cx.report_node(node, msg).id("unsafeToAnyTypeAssertion");
            return;
        }
        // A fresh object literal is widened so its extra properties aren't excess ones.
        let from = if ti.object_flags(from).contains(ObjectFlags::OBJECT_LITERAL) {
            ti.widened_type(from)
        } else {
            from
        };
        if ti.is_assignable(from, to) {
            return;
        }
        let name = ti.type_to_string(to);
        if ti.flags(to).contains(TypeFlags::TYPE_PARAMETER) {
            let c = ti.base_constraint(to);
            if !has_constraint(ti, to) || c == to || c == Ty::UNRESOLVED {
                let msg = format!(
                    "Unsafe type assertion: '{name}' could be instantiated with an arbitrary type which could be unrelated to the original type."
                );
                cx.report_node(node, msg)
                    .id("unsafeToUnconstrainedTypeAssertion");
                return;
            }
            if ti.is_assignable(from, c) {
                let msg = format!(
                    "Unsafe type assertion: the original type is assignable to the constraint of type '{name}', but '{name}' could be instantiated with a different subtype of its constraint."
                );
                cx.report_node(node, msg)
                    .id("unsafeTypeAssertionAssignableToConstraint");
                return;
            }
        }
        let msg =
            format!("Unsafe type assertion: type '{name}' is more narrow than the original type.");
        cx.report_node(node, msg).id("unsafeTypeAssertion");
    }
}
