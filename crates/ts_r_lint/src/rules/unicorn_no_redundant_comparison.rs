//! `unicorn/no-redundant-comparison`: in an outermost `&&` chain, a comparison already
//! implied by the others through `a === b` / `a !== b` links between references.
//!
//! The operands that are strict (in)equalities between two different references become
//! links; other comparisons (`<`, `<=`, `>`, `>=`, `===`, `!==`) with a reference on one
//! side become predicates on it (none with an optional chain). Equalities merge into
//! classes; within a class, a predicate implied by another (the same operator and value, or
//! numeric bounds known statically) makes the later one redundant, or the earlier when only
//! the later implies it. `a === k && a !== b` makes `b !== k` redundant. Nothing is
//! reported when an operand between the first and last analyzed one has a side effect or a
//! tagged template. The removal (with its `&&`) is a fix, or a suggestion when evaluating the
//! comparison could run user code (getters, conversions); no fix when it would drop a
//! comment.

use super::prelude::*;
use super::unicorn_ast::contains_node;
use super::unicorn_static::{Static, static_value_no_side_effects};
use super::unicorn_text::{needs_semicolon, paren_span};
use super::unicorn_util::has_side_effect_with as has_side_effect;
use super::unicorn_util::{
    is_outermost_logical, is_same_reference, logical_operands, skip_wrappers as unwrap_ts,
};
use super::unicorn_util_h4::has_side_effect;
use ts_r_ast::Ast;
use ts_r_ast::visit::{Visitor, Walk, walk};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-redundant-comparison",
        "Disallow comparisons made redundant by an equality check in the same logical AND.",
        NO_PRESET,
        Category::Suspicious,
    )
    .fixable()
    .suggestions(),
    create: create_default::<NoRedundantComparison>,
};

#[derive(Debug, Default)]
struct NoRedundantComparison;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Op {
    Lt,
    LtEq,
    Gt,
    GtEq,
    Eq,
    NotEq,
}

impl Op {
    fn of(op: BinaryOp) -> Option<Op> {
        Some(match op {
            BinaryOp::Lt => Op::Lt,
            BinaryOp::LtEq => Op::LtEq,
            BinaryOp::Gt => Op::Gt,
            BinaryOp::GtEq => Op::GtEq,
            BinaryOp::StrictEq => Op::Eq,
            BinaryOp::StrictNotEq => Op::NotEq,
            _ => return None,
        })
    }
    fn flip(self) -> Op {
        match self {
            Op::Lt => Op::Gt,
            Op::LtEq => Op::GtEq,
            Op::Gt => Op::Lt,
            Op::GtEq => Op::LtEq,
            o => o,
        }
    }
    fn ordering(self) -> bool {
        !matches!(self, Op::Eq | Op::NotEq)
    }
}

#[derive(Clone, Copy)]
struct Comparison {
    op: Op,
    left: NodeId,
    right: NodeId,
    node: NodeId,
}

#[derive(Clone, Copy)]
struct Predicate {
    op: Op,
    value: NodeId,
    node: NodeId,
}

pub(crate) fn is_reference(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(unwrap_ts(ast, id)),
        Node::Ident { .. }
            | Node::Member { .. }
            | Node::Index { .. }
            | Node::Super {}
            | Node::This {}
    )
}

pub(crate) fn contains_optional_chain(ast: &Ast, id: NodeId) -> bool {
    struct Find {
        found: bool,
    }
    impl Visitor for Find {
        fn enter(&mut self, ast: &Ast, id: NodeId) -> Walk {
            if matches!(
                ast.node(id),
                Node::Member { optional: true, .. }
                    | Node::Index { optional: true, .. }
                    | Node::Call { optional: true, .. }
            ) {
                self.found = true;
                return Walk::Stop;
            }
            Walk::Continue
        }
    }
    let mut f = Find { found: false };
    walk(ast, id, &mut f);
    f.found
}

fn implies_numeric(p: Op, pv: f64, q: Op, qv: f64) -> bool {
    let lower = |o: Op| matches!(o, Op::Gt | Op::GtEq);
    if lower(p) != lower(q) {
        return false;
    }
    if lower(p) {
        if pv > qv {
            return true;
        }
        if pv < qv {
            return false;
        }
    } else {
        if pv < qv {
            return true;
        }
        if pv > qv {
            return false;
        }
    }
    let inclusive = |o: Op| matches!(o, Op::GtEq | Op::LtEq);
    !inclusive(p) || inclusive(q)
}

fn number_value(cx: &Ctx<'_, '_>, id: NodeId) -> Option<f64> {
    match static_value_no_side_effects(cx, unwrap_ts(cx.ast(), id))? {
        Static::Num(n) if !n.is_nan() => Some(n),
        _ => None,
    }
}

fn entails(cx: &Ctx<'_, '_>, a: Predicate, b: Predicate) -> bool {
    if a.op == b.op && is_same_reference(cx, a.value, b.value) {
        return true;
    }
    if !a.op.ordering() || !b.op.ordering() {
        return false;
    }
    match (number_value(cx, a.value), number_value(cx, b.value)) {
        (Some(av), Some(bv)) => implies_numeric(a.op, av, b.op, bv),
        _ => false,
    }
}

fn for_subject(cx: &Ctx<'_, '_>, c: Comparison, subject: NodeId) -> Option<Predicate> {
    if is_same_reference(cx, c.left, subject) {
        return Some(Predicate {
            op: c.op,
            value: c.right,
            node: c.node,
        });
    }
    is_same_reference(cx, c.right, subject).then_some(Predicate {
        op: c.op.flip(),
        value: c.left,
        node: c.node,
    })
}

fn for_class(cx: &Ctx<'_, '_>, c: Comparison, class: &[NodeId]) -> Option<Predicate> {
    let l = class.iter().any(|&m| is_same_reference(cx, m, c.left));
    let r = class.iter().any(|&m| is_same_reference(cx, m, c.right));
    if l == r {
        return None;
    }
    Some(if l {
        Predicate {
            op: c.op,
            value: c.right,
            node: c.node,
        }
    } else {
        Predicate {
            op: c.op.flip(),
            value: c.left,
            node: c.node,
        }
    })
}

fn equality_classes(cx: &Ctx<'_, '_>, links: &[(NodeId, NodeId)]) -> Vec<Vec<NodeId>> {
    let mut classes: Vec<Vec<NodeId>> = Vec::new();
    for &(l, r) in links {
        let find = |classes: &Vec<Vec<NodeId>>, x: NodeId| {
            classes
                .iter()
                .position(|c| c.iter().any(|&m| is_same_reference(cx, m, x)))
        };
        match (find(&classes, l), find(&classes, r)) {
            (Some(a), Some(b)) => {
                if a != b {
                    let moved = classes.remove(b);
                    let a = if b < a { a - 1 } else { a };
                    classes[a].extend(moved);
                }
            }
            (Some(a), None) => classes[a].push(r),
            (None, Some(b)) => classes[b].push(l),
            (None, None) => classes.push(vec![l, r]),
        }
    }
    classes
}

fn find_redundant(
    cx: &Ctx<'_, '_>,
    equalities: &[(NodeId, NodeId)],
    disequalities: &[(NodeId, NodeId)],
    comparisons: &[Comparison],
) -> Option<NodeId> {
    for class in equality_classes(cx, equalities) {
        let preds: Vec<Predicate> = comparisons
            .iter()
            .filter_map(|&c| for_class(cx, c, &class))
            .collect();
        for (i, &first) in preds.iter().enumerate() {
            for &second in &preds[i + 1..] {
                if entails(cx, first, second) {
                    return Some(second.node);
                }
                if entails(cx, second, first) {
                    return Some(first.node);
                }
            }
        }
    }
    for &(l, r) in disequalities {
        for (subject, other) in [(l, r), (r, l)] {
            let Some(value) = comparisons
                .iter()
                .filter_map(|&c| for_subject(cx, c, subject))
                .find(|p| p.op == Op::Eq)
                .map(|p| p.value)
            else {
                continue;
            };
            if let Some(c) = comparisons.iter().find(|&&c| {
                for_subject(cx, c, other)
                    .is_some_and(|p| p.op == Op::NotEq && is_same_reference(cx, p.value, value))
            }) {
                return Some(c.node);
            }
        }
    }
    None
}

impl Rule for NoRedundantComparison {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Binary]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        if !matches!(
            cx.node(node),
            Node::Binary {
                op: BinaryOp::LogicalAnd,
                ..
            }
        ) || !is_outermost_logical(cx, node)
        {
            return;
        }
        let operands = logical_operands(ast, node, BinaryOp::LogicalAnd);
        let mut equalities = Vec::new();
        let mut disequalities = Vec::new();
        let mut comparisons = Vec::new();
        let (mut first, mut last) = (None, 0);
        for (i, &operand) in operands.iter().enumerate() {
            let expr = unwrap_ts(ast, operand);
            let Node::Binary { left, op, right } = *ast.node(expr) else {
                continue;
            };
            let Some(op) = Op::of(op) else {
                continue;
            };
            if contains_optional_chain(ast, expr) {
                continue;
            }
            let (lr, rr) = (is_reference(ast, left), is_reference(ast, right));
            if lr && rr && matches!(op, Op::Eq | Op::NotEq) {
                if is_same_reference(cx, left, right) {
                    continue;
                }
                if op == Op::Eq {
                    equalities.push((left, right));
                } else {
                    disequalities.push((left, right));
                }
            } else if lr || rr {
                comparisons.push(Comparison {
                    op,
                    left,
                    right,
                    node: operand,
                });
            } else {
                continue;
            }
            first.get_or_insert(i);
            last = i;
        }
        let Some(first) = first else {
            return;
        };
        if comparisons.is_empty() || (equalities.is_empty() && disequalities.is_empty()) {
            return;
        }
        if operands[first..=last].iter().any(|&o| {
            has_side_effect(ast, o, false, false)
                || contains_node(ast, o, |n| matches!(n, Node::TaggedTemplate { .. }))
        }) {
            return;
        }
        let Some(redundant) = find_redundant(cx, &equalities, &disequalities, &comparisons) else {
            return;
        };
        let Some(parent) = parent_skip_parens(ast, cx.file.parents, redundant) else {
            return;
        };
        let Node::Binary { left, right, .. } = *cx.node(parent) else {
            return;
        };
        let (ls, rs) = (paren_span(cx, left), paren_span(cx, right));
        let is_left = skip_parens(ast, left) == redundant;
        let range = if is_left {
            Span::new(ls.start, rs.start)
        } else {
            Span::new(ls.end, rs.end)
        };
        let msg =
            "This comparison is redundant because it is already implied by the other conditions.";
        let id = "no-redundant-comparison";
        let span = cx.span(redundant);
        if !ast.comments_in(range).is_empty() {
            cx.report(span, msg).id(id);
            return;
        }
        let src = cx.src();
        let next = src[range.end as usize..]
            .chars()
            .next()
            .map(String::from)
            .unwrap_or_default();
        let replacement = if is_left && needs_semicolon(cx, range.start, &next) {
            ";"
        } else {
            ""
        };
        let fix = Fix::replace(range, replacement);
        if has_side_effect(ast, redundant, true, true) {
            cx.report(span, msg).id(id).suggest(
                "no-redundant-comparison/suggestion",
                "Remove this redundant comparison.",
                fix,
            );
        } else {
            cx.report(span, msg).id(id).fix(fix);
        }
    }
}
