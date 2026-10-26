//! `unicorn/prefer-simple-sort-comparator`: a `.sort()` / `.toSorted()` comparator (a plain
//! function or arrow with two identifier parameters) that is only a tree of comparisons
//! returning number literals (`a > b ? 1 : -1`, `if (a < b) return -1; ...`) is a
//! subtraction in disguise. Every test must be `<`, `>`, `<=` or `>=` between mirror
//! images of the parameters (`a` / `b`, `a.x` / `b.x`, `a[k]` / `b[k]`, no `?.`), all on
//! the same pair; the leaves must include a positive and a negative number, the first
//! test's true branch must not return 0, and each branch must return the sign the
//! subtraction would. The comparator is reported, unless the receiver is known not to be an
//! array; the suggestion `(a, b) => x - y` is dropped when the comparator holds a comment or
//! the receiver is a `BigInt64Array` / `BigUint64Array` (bigints can't be returned).

use super::prelude::*;
use super::unicorn_ast::{method_call, would_remove_comments};
use super::unicorn_types::{is_known_bigint_typed_array, should_skip_known_non_array_receiver};
use super::unicorn_util::has_optional_chain;
use super::unicorn_util::is_same_reference;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-simple-sort-comparator",
        "Prefer a simple comparison function for `Array#sort()`.",
        NO_PRESET,
        Category::Style,
    )
    .suggestions(),
    create: create_default::<PreferSimpleSortComparator>,
};

#[derive(Debug, Default)]
struct PreferSimpleSortComparator;

/// Nesting limit for the comparison tree.
const MAX_DEPTH: u32 = 200;

enum Tree {
    Leaf(NodeId),
    Branch {
        test: NodeId,
        cons: Box<Tree>,
        alt: Box<Tree>,
    },
}

/// A number literal, possibly signed: its sign.
fn signed_number(cx: &Ctx<'_, '_>, id: NodeId) -> Option<f64> {
    let ast = cx.ast();
    let id = skip_parens(ast, id);
    let (negate, lit) = match *ast.node(id) {
        Node::NumberLit { .. } => (false, id),
        Node::Unary {
            op: op @ (UnaryOp::Minus | UnaryOp::Plus),
            arg,
        } if matches!(ast.node(skip_parens(ast, arg)), Node::NumberLit { .. }) => {
            (op == UnaryOp::Minus, skip_parens(ast, arg))
        }
        _ => return None,
    };
    let v = super::unicorn_static::static_value(cx, lit)?.to_number()?;
    let v = if negate { -v } else { v };
    Some(if v > 0.0 {
        1.0
    } else if v < 0.0 {
        -1.0
    } else {
        0.0
    })
}

fn expression(cx: &Ctx<'_, '_>, id: NodeId, depth: u32) -> Option<Tree> {
    let ast = cx.ast();
    if depth > MAX_DEPTH {
        return None;
    }
    let id = skip_parens(ast, id);
    if signed_number(cx, id).is_some() {
        return Some(Tree::Leaf(id));
    }
    let Node::Conditional { test, cons, alt } = *ast.node(id) else {
        return None;
    };
    Some(Tree::Branch {
        test,
        cons: Box::new(expression(cx, cons, depth + 1)?),
        alt: Box::new(expression(cx, alt, depth + 1)?),
    })
}

fn block_or_one(ast: &Ast, stmt: NodeId) -> Vec<NodeId> {
    match *ast.node(stmt) {
        Node::Block { stmts } => ast.list(stmts).to_vec(),
        _ => vec![stmt],
    }
}

fn statements(cx: &Ctx<'_, '_>, stmts: &[NodeId], depth: u32) -> Option<Tree> {
    let ast = cx.ast();
    if depth > MAX_DEPTH {
        return None;
    }
    let (&first, rest) = stmts.split_first()?;
    match *ast.node(first) {
        Node::Return { arg } => expression(cx, arg?, depth + 1),
        Node::If { test, cons, alt } => {
            let c = statements(cx, &block_or_one(ast, cons), depth + 1)?;
            let a = match alt {
                Some(alt) => statements(cx, &block_or_one(ast, alt), depth + 1)?,
                None => statements(cx, rest, depth + 1)?,
            };
            Some(Tree::Branch {
                test,
                cons: Box::new(c),
                alt: Box::new(a),
            })
        }
        _ => None,
    }
}

fn collect(tree: &Tree, tests: &mut Vec<NodeId>, leaves: &mut Vec<NodeId>) {
    match tree {
        Tree::Leaf(id) => leaves.push(*id),
        Tree::Branch { test, cons, alt } => {
            tests.push(*test);
            collect(cons, tests, leaves);
            collect(alt, tests, leaves);
        }
    }
}

fn true_sign(cx: &Ctx<'_, '_>, mut tree: &Tree) -> f64 {
    loop {
        match tree {
            Tree::Leaf(id) => return signed_number(cx, *id).unwrap_or(0.0),
            Tree::Branch { cons, .. } => tree = cons,
        }
    }
}

/// `(left, op, right)` of a relational test.
fn relation(ast: &Ast, test: NodeId) -> Option<(NodeId, BinaryOp, NodeId)> {
    match *ast.node(skip_parens(ast, test)) {
        Node::Binary { left, op, right }
            if matches!(
                op,
                BinaryOp::Gt | BinaryOp::Lt | BinaryOp::GtEq | BinaryOp::LtEq
            ) =>
        {
            Some((left, op, right))
        }
        _ => None,
    }
}

fn is_greater(op: BinaryOp) -> bool {
    matches!(op, BinaryOp::Gt | BinaryOp::GtEq)
}

/// `left` and `right` are the same path on the two parameters, swapped.
fn is_parameter_swap(
    cx: &Ctx<'_, '_>,
    left: NodeId,
    right: NodeId,
    p: (&str, &str),
    depth: u32,
) -> bool {
    let ast = cx.ast();
    if depth > MAX_DEPTH {
        return false;
    }
    let (left, right) = (skip_parens(ast, left), skip_parens(ast, right));
    if let (Some(l), Some(r)) = (ident_name(ast, left), ident_name(ast, right)) {
        return (l == p.0 && r == p.1) || (l == p.1 && r == p.0);
    }
    if has_optional_chain(ast, left) || has_optional_chain(ast, right) {
        return false;
    }
    match (*ast.node(left), *ast.node(right)) {
        (
            Node::Member {
                obj: lo, prop: lp, ..
            },
            Node::Member {
                obj: ro, prop: rp, ..
            },
        ) => {
            let name = |id| match *ast.node(id) {
                Node::Ident { name } | Node::PrivateName { name } => Some(name),
                _ => None,
            };
            name(lp).is_some()
                && name(lp) == name(rp)
                && is_parameter_swap(cx, lo, ro, p, depth + 1)
        }
        (
            Node::Index {
                obj: lo, index: li, ..
            },
            Node::Index {
                obj: ro, index: ri, ..
            },
        ) => is_same_reference(cx, li, ri) && is_parameter_swap(cx, lo, ro, p, depth + 1),
        _ => false,
    }
}

fn signs_match(cx: &Ctx<'_, '_>, tree: &Tree, minuend: NodeId, subtrahend: NodeId) -> bool {
    let ast = cx.ast();
    let Tree::Branch { test, cons, alt } = tree else {
        return true;
    };
    let Some((left, op, right)) = relation(ast, *test) else {
        return false;
    };
    let left_is_minuend =
        is_same_reference(cx, left, minuend) && is_same_reference(cx, right, subtrahend);
    let expected = if left_is_minuend == is_greater(op) {
        1.0
    } else {
        -1.0
    };
    true_sign(cx, tree) == expected
        && signs_match(cx, cons, minuend, subtrahend)
        && signs_match(cx, alt, minuend, subtrahend)
}

impl Rule for PreferSimpleSortComparator {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some(call) = method_call(ast, node) else {
            return;
        };
        if !matches!(call.name, "sort" | "toSorted") || call.args.len() != 1 {
            return;
        }
        let comparator = skip_parens(ast, call.args[0]);
        let (params, body, expr_body) = match *ast.node(comparator) {
            Node::FunctionExpr {
                flags,
                params,
                body: Some(body),
                ..
            } if !flags.intersects(FnFlags::ASYNC | FnFlags::GENERATOR) => (params, body, false),
            Node::Arrow {
                flags,
                params,
                body,
                ..
            } if !flags.contains(FnFlags::ASYNC) => {
                (params, body, flags.contains(FnFlags::EXPRESSION_BODY))
            }
            _ => return,
        };
        let names: Vec<&str> = ast
            .list(params)
            .iter()
            .filter_map(|&p| match *ast.node(p) {
                Node::Param {
                    rest: false,
                    pattern,
                    init: None,
                    ..
                } => ident_name(ast, pattern),
                _ => None,
            })
            .collect();
        let &[first, second] = names.as_slice() else {
            return;
        };
        if ast.list(params).len() != 2 {
            return;
        }
        let tree = if expr_body {
            expression(cx, body, 0)
        } else {
            match *ast.node(body) {
                Node::Block { stmts } => statements(cx, ast.list(stmts), 0),
                _ => None,
            }
        };
        let Some(tree @ Tree::Branch { .. }) = tree else {
            return;
        };
        let (mut tests, mut leaves) = (Vec::new(), Vec::new());
        collect(&tree, &mut tests, &mut leaves);
        let mut pairs = Vec::with_capacity(tests.len());
        for &t in &tests {
            let Some((l, _, r)) = relation(ast, t) else {
                return;
            };
            if !is_parameter_swap(cx, l, r, (first, second), 0) {
                return;
            }
            pairs.push((l, r));
        }
        let (l0, r0) = pairs[0];
        if !pairs.iter().all(|&(l, r)| {
            (is_same_reference(cx, l, l0) && is_same_reference(cx, r, r0))
                || (is_same_reference(cx, l, r0) && is_same_reference(cx, r, l0))
        }) {
            return;
        }
        let signs: Vec<f64> = leaves
            .iter()
            .map(|&l| signed_number(cx, l).unwrap_or(0.0))
            .collect();
        let sign = true_sign(cx, &tree);
        if signs.iter().all(|&s| s <= 0.0) || signs.iter().all(|&s| s >= 0.0) || sign == 0.0 {
            return;
        }
        let Tree::Branch { test, .. } = &tree else {
            return;
        };
        let Some((left, op, right)) = relation(ast, *test) else {
            return;
        };
        let left_is_minuend = if is_greater(op) {
            sign > 0.0
        } else {
            sign < 0.0
        };
        let (minuend, subtrahend) = if left_is_minuend {
            (left, right)
        } else {
            (right, left)
        };
        if !signs_match(cx, &tree, minuend, subtrahend)
            || should_skip_known_non_array_receiver(cx, call.obj)
        {
            return;
        }
        let replacement = format!(
            "({first}, {second}) => {} - {}",
            cx.text(skip_parens(ast, minuend)),
            cx.text(skip_parens(ast, subtrahend))
        );
        let span = cx.span(comparator);
        let suggest =
            !would_remove_comments(ast, span, &[]) && !is_known_bigint_typed_array(cx, call.obj);
        let report = cx
            .report_node(
                comparator,
                "Prefer a simple comparison function for `Array#sort()`.",
            )
            .id("prefer-simple-sort-comparator/error");
        if suggest {
            report.suggest(
                "prefer-simple-sort-comparator/suggestion",
                format!("Replace with `{replacement}`."),
                Fix::replace(span, replacement),
            );
        }
    }
}
