//! `@typescript-eslint/require-array-sort-compare`: `arr.sort()` / `arr.toSorted()` without a
//! compare function on an array type (string arrays allowed by default).

use super::prelude::*;
use crate::type_util::{constrained, constrained_type_at, parts};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/require-array-sort-compare",
        "Require `Array#sort` and `Array#toSorted` calls to always provide a `compareFunction`",
        NO_PRESET,
        Category::Correctness,
    )
    .typed(),
    create: create::<RequireArraySortCompare>,
};

#[derive(Debug)]
struct RequireArraySortCompare {
    ignore_string_arrays: bool,
}

impl FromOptions for RequireArraySortCompare {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(RequireArraySortCompare {
            ignore_string_arrays: bool_field(obj_opt(opts, 0), "ignoreStringArrays", true),
        })
    }
}

/// The type's name would print as `string` (string-like, through constraints, unions of
/// strings and intersections with a string).
fn is_string_type(ti: &dyn TypeInfo, t: Ty, depth: u32) -> bool {
    if depth > 20 {
        return false;
    }
    let t = constrained(ti, t);
    let f = ti.flags(t);
    if f.contains(TypeFlags::UNION) {
        let ps = ti.union_members(t);
        return !ps.is_empty() && ps.iter().all(|&p| is_string_type(ti, p, depth + 1));
    }
    if f.contains(TypeFlags::INTERSECTION) {
        return ti
            .intersection_members(t)
            .iter()
            .any(|&p| is_string_type(ti, p, depth + 1));
    }
    f.intersects(TypeFlags::STRING_LIKE)
}

impl Rule for RequireArraySortCompare {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Call { callee, args, .. } = *cx.node(node) else {
            return;
        };
        if !ast.list(args).is_empty() {
            return;
        }
        let callee = skip_parens(ast, callee);
        if !matches!(ast.node(callee), Node::Member { .. } | Node::Index { .. }) {
            return;
        }
        let Some((obj, name)) = member_parts(ast, cx.src(), callee) else {
            return;
        };
        if name != "sort" && name != "toSorted" {
            return;
        }
        let ti = cx.types();
        let Some(t) = constrained_type_at(ti, obj) else {
            return;
        };
        if self.ignore_string_arrays
            && (ti.is_array(t) || ti.is_tuple(t))
            && ti.type_args(t).iter().all(|&a| is_string_type(ti, a, 0))
        {
            return;
        }
        let ps = parts(ti, t);
        if !ps.is_empty() && ps.iter().all(|&p| ti.is_array(p)) {
            cx.report_node(node, "Require 'compare' argument.")
                .id("requireCompare");
        }
    }
}
