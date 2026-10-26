//! `unicorn/prefer-includes-over-repeated-comparisons`: `x === a || x === b || x === c`
//! (or `x !== a && x !== b && ...`) is a membership check, `[a, b, c].includes(x)`.
//!
//! The outermost `||` (with `===` or `==`) or `&&` (with `!==` or `!=`) chain, flattened
//! through parentheses, of at least `minimumComparisons` (3) comparisons is reported when
//! every comparison has the same subject on one side: a reference (name, member access,
//! `this`, `super`; TS wrappers looked through; not `undefined`) found on a side of every
//! comparison, and the only such one in the first. Operands with an optional chain or a
//! side effect, a compared value equal to the subject on both sides, and a `NaN` subject or
//! value (statically known) leave the chain alone.
//!
//! With type information, a subject whose type (its base constraint for a type parameter)
//! is a union is skipped (comparisons narrow, `.includes()` does not; `boolean` and enum
//! types are unions). Loose operators (`==`, `!=`) are only reported with types, when the
//! subject and every operand are all primitive strings or all primitive numbers: string
//! literals, template literal types and string enum members count as strings, number
//! literals and numeric enum members as numbers; `any`, `unknown`, `never` and boxed
//! `String` count as neither. No fix.

use super::prelude::*;
use super::unicorn_static::{Static, has_side_effect, static_value_no_side_effects};
use super::unicorn_util::{is_same_reference, skip_wrappers};
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-includes-over-repeated-comparisons",
        "Prefer `.includes()` over repeated equality comparisons.",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<PreferIncludesOverRepeatedComparisons>,
};

#[derive(Debug)]
struct PreferIncludesOverRepeatedComparisons {
    minimum: usize,
}

impl FromOptions for PreferIncludesOverRepeatedComparisons {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        let minimum = match o.get("minimumComparisons") {
            None => 3,
            Some(v) => {
                v.as_u64()
                    .filter(|&n| n >= 2)
                    .ok_or("minimumComparisons must be an integer >= 2")? as usize
            }
        };
        Ok(Self { minimum })
    }
}

const MESSAGE: &str = "Use `.includes()` instead of repeated equality checks.";

/// Bound on the walks through operands.
const MAX_DEPTH: u32 = 200;

fn unparen(ast: &Ast, mut id: NodeId) -> NodeId {
    while let Node::Paren { expr } = *ast.node(id) {
        id = expr;
    }
    id
}

/// The operands of the `op` chain at `id`, left to right, through parentheses.
fn operands(ast: &Ast, id: NodeId, op: BinaryOp, out: &mut Vec<NodeId>, limit: usize) {
    let mut stack = vec![id];
    while let Some(n) = stack.pop() {
        if out.len() > limit {
            return;
        }
        let u = unparen(ast, n);
        match *ast.node(u) {
            Node::Binary { left, op: o, right } if o == op => {
                stack.push(right);
                stack.push(left);
            }
            _ => out.push(u),
        }
    }
}

/// An optional link (`?.`) anywhere inside `id`.
fn contains_optional_chain(ast: &Ast, id: NodeId, depth: u32) -> bool {
    if depth > MAX_DEPTH {
        return true;
    }
    let optional = match *ast.node(id) {
        Node::Member { optional, .. }
        | Node::Index { optional, .. }
        | Node::Call { optional, .. } => optional,
        _ => false,
    };
    if optional {
        return true;
    }
    let mut found = false;
    ast.for_each_child(id, |c| {
        if !found && contains_optional_chain(ast, c, depth + 1) {
            found = true;
        }
    });
    found
}

fn is_reference(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(skip_wrappers(ast, id)),
        Node::Ident { .. }
            | Node::Member { .. }
            | Node::Index { .. }
            | Node::Super {}
            | Node::This {}
    )
}

fn is_undefined_ident(ast: &Ast, id: NodeId) -> bool {
    ident_name(ast, skip_wrappers(ast, id)) == Some("undefined")
}

fn is_nan(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    matches!(
        static_value_no_side_effects(cx, skip_wrappers(cx.ast(), id)),
        Some(Static::Num(n)) if n.is_nan()
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Prim {
    Str,
    Num,
}

/// Whether type `t` is assignable to `string` and to `number`.
fn assignable(ti: &dyn TypeInfo, t: Ty, depth: u32) -> (bool, bool) {
    if depth > 20 {
        return (false, false);
    }
    let t = ti.base_constraint(t);
    if t == Ty::UNRESOLVED || ti.is_error(t) {
        return (false, false);
    }
    let f = ti.flags(t);
    if f.intersects(TypeFlags::ANY | TypeFlags::NEVER) {
        return (true, true);
    }
    if f.contains(TypeFlags::UNION) {
        let mut acc = (true, true);
        for m in ti.union_members(t) {
            let (s, n) = assignable(ti, m, depth + 1);
            acc = (acc.0 && s, acc.1 && n);
        }
        return acc;
    }
    if f.contains(TypeFlags::INTERSECTION) {
        let mut acc = (false, false);
        for m in ti.intersection_members(t) {
            let (s, n) = assignable(ti, m, depth + 1);
            acc = (acc.0 || s, acc.1 || n);
        }
        return acc;
    }
    let is_str = f.intersects(TypeFlags::STRING_LIKE);
    let is_num = f.intersects(TypeFlags::NUMBER_LIKE);
    (is_str, is_num)
}

/// The primitive (string or number) the type at `node` is assignable to, when exactly one.
fn primitive(cx: &Ctx<'_, '_>, node: NodeId) -> Option<Prim> {
    let ti = cx.types();
    let t = ti.type_at(unparen(cx.ast(), node))?;
    match assignable(ti, t, 0) {
        (true, false) => Some(Prim::Str),
        (false, true) => Some(Prim::Num),
        _ => None,
    }
}

/// The subject's type is a union (or a type parameter constrained to one).
fn is_union_typed(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ti = cx.types();
    let Some(t) = ti.type_at(node) else {
        return false;
    };
    let t = ti.base_constraint(t);
    let f = ti.flags(t);
    f.contains(TypeFlags::UNION)
        || (f.contains(TypeFlags::BOOLEAN) && !f.contains(TypeFlags::BOOLEAN_LITERAL))
}

impl Rule for PreferIncludesOverRepeatedComparisons {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Binary]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Binary { op, .. } = *ast.node(node) else {
            return;
        };
        let (strict, loose) = match op {
            BinaryOp::LogicalOr => (BinaryOp::StrictEq, BinaryOp::Eq),
            BinaryOp::LogicalAnd => (BinaryOp::StrictNotEq, BinaryOp::NotEq),
            _ => return,
        };
        // Only the outermost expression of a chain.
        if let Some(p) = parent_skip_parens(ast, cx.file.parents, node)
            && matches!(*ast.node(p), Node::Binary { op: o, .. } if o == op)
        {
            return;
        }
        let mut ops = Vec::new();
        operands(ast, node, op, &mut ops, 10_000);
        if ops.len() < self.minimum {
            return;
        }
        let mut comparisons = Vec::with_capacity(ops.len());
        let mut has_loose = false;
        for &o in &ops {
            let Node::Binary { left, op: c, right } = *ast.node(o) else {
                return;
            };
            if c == loose {
                has_loose = true;
            } else if c != strict {
                return;
            }
            comparisons.push((left, right));
        }
        if comparisons.iter().any(|&(l, r)| {
            [l, r]
                .iter()
                .any(|&x| contains_optional_chain(ast, x, 0) || has_side_effect(ast, x))
        }) {
            return;
        }
        // The shared subject.
        let (l0, r0) = comparisons[0];
        let mut candidates: Vec<NodeId> = [l0, r0]
            .into_iter()
            .filter(|&x| is_reference(ast, x) && !is_undefined_ident(ast, x))
            .collect();
        for &(l, r) in &comparisons[1..] {
            candidates.retain(|&c| is_same_reference(cx, c, l) || is_same_reference(cx, c, r));
            if candidates.is_empty() {
                return;
            }
        }
        let [subject] = candidates[..] else {
            return;
        };
        if is_nan(cx, subject) {
            return;
        }
        for &(l, r) in &comparisons {
            let (sl, sr) = (
                is_same_reference(cx, l, subject),
                is_same_reference(cx, r, subject),
            );
            if sl == sr {
                return;
            }
            if is_nan(cx, if sl { r } else { l }) {
                return;
            }
        }
        let typed = cx.types().is_available();
        if typed && is_union_typed(cx, skip_wrappers(ast, subject)) {
            return;
        }
        if has_loose {
            if !typed {
                return;
            }
            let Some(p) = primitive(cx, subject) else {
                return;
            };
            if comparisons
                .iter()
                .any(|&(l, r)| primitive(cx, l) != Some(p) || primitive(cx, r) != Some(p))
            {
                return;
            }
        }
        cx.report(cx.span(node), MESSAGE)
            .id("prefer-includes-over-repeated-comparisons");
    }
}
