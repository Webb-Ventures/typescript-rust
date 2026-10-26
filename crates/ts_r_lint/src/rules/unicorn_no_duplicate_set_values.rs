//! `unicorn/no-duplicate-set-values`: in `new Set([...])` (one non-spread argument, an
//! array literal through TS wrappers), an element equal to an earlier one. Elements
//! compare by their side-effect-free static primitive value (SameValueZero), else, for
//! non-literals, by `is_same_reference`. Spreads are skipped; a hole counts as
//! `undefined` and is reported at the array.

use super::prelude::*;
use super::unicorn_static::{Static, skip_ts_only, static_value_no_side_effects};
use super::unicorn_text::is_same_reference;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-duplicate-set-values",
        "Disallow duplicate values in `Set` constructor array literals.",
        NO_PRESET,
        Category::Suspicious,
    ),
    create: create_default::<NoDuplicateSetValues>,
};

#[derive(Debug, Default)]
struct NoDuplicateSetValues;

fn comparable(v: Static) -> Option<Static> {
    matches!(
        v,
        Static::Undefined
            | Static::Null
            | Static::Bool(_)
            | Static::Num(_)
            | Static::Str(_)
            | Static::BigInt(_)
    )
    .then_some(v)
}

fn same_value_zero(a: &Static, b: &Static) -> bool {
    match (a, b) {
        (Static::Num(x), Static::Num(y)) => x == y || (x.is_nan() && y.is_nan()),
        (Static::Undefined, Static::Undefined) | (Static::Null, Static::Null) => true,
        (Static::Bool(x), Static::Bool(y)) => x == y,
        (Static::Str(x), Static::Str(y)) => x == y,
        (Static::BigInt(x), Static::BigInt(y)) => x == y,
        _ => false,
    }
}

fn is_literal(ast: &ts_r_ast::Ast, id: NodeId) -> bool {
    matches!(
        ast.node(id),
        Node::StringLit { .. }
            | Node::NumberLit { .. }
            | Node::BigIntLit { .. }
            | Node::BoolLit { .. }
            | Node::NullLit {}
            | Node::RegexLit { .. }
    )
}

impl Rule for NoDuplicateSetValues {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::New]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::New { callee, args, .. } = *cx.node(node) else {
            return;
        };
        if ident_name(ast, callee) != Some("Set") {
            return;
        }
        let [iterable] = *ast.list(args) else {
            return;
        };
        let Node::ArrayLit { elements } = *ast.node(skip_ts_only(ast, iterable)) else {
            return;
        };
        let mut seen: Vec<(Option<NodeId>, Option<Static>)> = Vec::new();
        let mut dups: Vec<Option<NodeId>> = Vec::new();
        for &e in ast.list(elements) {
            let el = match ast.node(e) {
                Node::Spread { .. } => continue,
                Node::Hole {} => None,
                _ => Some(e),
            };
            let value = match el {
                None => Some(Static::Undefined),
                Some(e) => static_value_no_side_effects(cx, e).and_then(comparable),
            };
            let dup = seen.iter().any(|(pe, pv)| match (pv, &value) {
                (Some(a), Some(b)) => same_value_zero(a, b),
                _ => match (pe, el) {
                    (Some(a), Some(b)) => {
                        !is_literal(ast, *a) && !is_literal(ast, b) && is_same_reference(cx, *a, b)
                    }
                    _ => false,
                },
            });
            if dup {
                dups.push(el);
            }
            seen.push((el, value));
        }
        for d in dups {
            let (span, text) = match d {
                Some(e) => (cx.span(e), cx.text(e).to_owned()),
                None => (cx.span(iterable), "undefined".to_owned()),
            };
            cx.report(
                span,
                format!("Remove duplicate value `{text}` from the Set."),
            )
            .id("no-duplicate-set-values");
        }
    }
}
