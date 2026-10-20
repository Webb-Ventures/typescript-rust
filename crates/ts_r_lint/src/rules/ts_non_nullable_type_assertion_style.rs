//! `@typescript-eslint/non-nullable-type-assertion-style`: prefer `x!` over `x as T` when
//! the assertion only removes `null`/`undefined`.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/non-nullable-type-assertion-style",
        "Enforce non-null assertions over explicit type assertions",
        TS_STYLISTIC_TC,
        Category::Style,
    )
    .typed()
    .fixable(),
    create: create_default::<NonNullableTypeAssertionStyle>,
};

#[derive(Debug, Default)]
struct NonNullableTypeAssertionStyle;

const NULLISH: TypeFlags = TypeFlags::NULL
    .union(TypeFlags::UNDEFINED)
    .union(TypeFlags::VOID);

impl Rule for NonNullableTypeAssertionStyle {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::As, NodeKind::TypeAssertion]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let (Node::As { expr, ty } | Node::TypeAssertion { ty, expr }) = *cx.node(node) else {
            return;
        };
        if matches!(
            cx.node(ty),
            Node::KeywordType {
                kind: KeywordTypeKind::Const
            }
        ) {
            return;
        }
        let ti = cx.types();
        let (Some(uncast), Some(cast)) = (ti.type_at(expr), ti.type_of_type_node(ty)) else {
            return;
        };
        if has_unresolved_part(ti, uncast) || has_unresolved_part(ti, cast) || is_any(ti, uncast) {
            return;
        }
        let up = parts(ti, uncast);
        if up.iter().any(|&p| is_any(ti, p) || is_unknown(ti, p)) {
            return;
        }
        let kept: Vec<Ty> = up
            .iter()
            .copied()
            .filter(|&p| !ti.flags(p).intersects(NULLISH))
            .collect();
        if kept.len() == up.len() {
            return;
        }
        let cp = parts(ti, cast);
        let possibly_nullish = |t: Ty| {
            let c = constrained(ti, t);
            c == Ty::UNRESOLVED
                || is_any(ti, c)
                || is_unknown(ti, c)
                || parts(ti, c)
                    .into_iter()
                    .any(|x| x == Ty::UNRESOLVED || ti.flags(x).intersects(NULLISH))
        };
        if cp.iter().any(|&p| possibly_nullish(p)) {
            return;
        }
        if kept.len() != cp.len() || !kept.iter().all(|k| cp.contains(k)) {
            return;
        }
        let ast = cx.ast();
        let inner = skip_parens(ast, expr);
        let text = cx.span(expr).text(cx.src());
        let replacement = if inner == expr && precedence(ast, expr) < 18 {
            format!("({text})!")
        } else {
            format!("{text}!")
        };
        let fix = Fix::replace(cx.span(node), replacement);
        cx.report_node(
            node,
            "Use a ! assertion to more succinctly remove null and undefined from the type.",
        )
        .id("preferNonNullAssertion")
        .fix(fix);
    }
}
