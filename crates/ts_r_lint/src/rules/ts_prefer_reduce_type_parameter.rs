//! `@typescript-eslint/prefer-reduce-type-parameter`: `arr.reduce<T>(.., init)` over
//! `arr.reduce(.., init as T)`.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/prefer-reduce-type-parameter",
        "Enforce using type parameter when calling `Array#reduce` instead of using a type assertion",
        TS_STRICT_TC,
        Category::Style,
    )
    .typed()
    .fixable(),
    create: create_default::<PreferReduceTypeParameter>,
};

#[derive(Debug, Default)]
struct PreferReduceTypeParameter;

impl Rule for PreferReduceTypeParameter {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Node::Call {
            callee,
            type_args,
            args,
            ..
        } = *cx.node(node)
        else {
            return;
        };
        let ast = cx.ast();
        let callee = skip_parens(ast, callee);
        let obj = match *cx.node(callee) {
            Node::Member { obj, prop, .. } if ident_name(ast, prop) == Some("reduce") => obj,
            Node::Index { obj, index, .. }
                if static_value_string(ast, cx.src(), skip_parens(ast, index)).as_deref()
                    == Some("reduce") =>
            {
                obj
            }
            _ => return,
        };
        let [_, init] = ast.list(args)[..] else {
            return;
        };
        let (expr, ty) = match *cx.node(init) {
            Node::As { expr, ty } | Node::TypeAssertion { expr, ty } => (expr, ty),
            _ => return,
        };
        let ti = cx.types();
        let Some(ot) = constrained_type_at(ti, obj) else {
            return;
        };
        if !all_arrays(ti, ot) {
            return;
        }
        let (Some(et), Some(at)) = (ti.type_at(expr), ti.type_of_type_node(ty)) else {
            return;
        };
        if et == Ty::UNRESOLVED || at == Ty::UNRESOLVED || !ti.is_assignable(et, at) {
            return;
        }
        let src = cx.src();
        let mut fix = Fix::replace(cx.span(init), cx.span(expr).text(src).to_string());
        if type_args.is_none() {
            fix = fix.and(
                Span::new(cx.span(callee).end, cx.span(callee).end),
                format!("<{}>", cx.span(ty).text(src)),
            );
        }
        cx.report_node(
            init,
            "Unnecessary assertion: Array#reduce accepts a type parameter for the default value.",
        )
        .id("preferTypeParameter")
        .fix(fix);
    }
}

/// Every union member is an array or tuple (an intersection only of those).
fn all_arrays(ti: &dyn TypeInfo, t: Ty) -> bool {
    if t == Ty::UNRESOLVED || is_any(ti, t) {
        return false;
    }
    let is_arr = |m: Ty| m != Ty::UNRESOLVED && (ti.is_array(m) || ti.is_tuple(m));
    parts(ti, t).into_iter().all(|p| {
        let p = constrained(ti, p);
        if ti.flags(p).contains(TypeFlags::INTERSECTION) {
            ti.intersection_members(p).into_iter().all(is_arr)
        } else {
            is_arr(p)
        }
    })
}
