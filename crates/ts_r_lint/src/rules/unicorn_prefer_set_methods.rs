//! `unicorn/prefer-set-methods`: Set operations written by hand, on values guessed to be
//! built-in `Set`s (`unicorn_types::builtin_collection_type`):
//! - `new Set([...a, ...b])` is `a.union(b)` (fixed; every operand side-effect free);
//! - `[...a].filter(v => b.has(v))` is `a.intersection(b)` and with `!b.has(v)`
//!   `a.difference(b)` (suggestions, also inside `new Set(...)`);
//! - `[...a].every/some(v => (!)b.has(v))` is `a.isSubsetOf(b)`, `a.isDisjointFrom(b)` or
//!   their negation, and so are `a.intersection(b).size === 0` and
//!   `a.difference(b).size > 0` (fixed).
//!
//! Nothing is fixed when a comment inside the replaced node would go.

use super::prelude::*;
use super::unicorn_ast::method_call;
use super::unicorn_static::has_side_effect_with_getters;
use super::unicorn_text::{
    has_comment_in, is_same_reference, member_object_text, needs_semicolon, paren_text,
    unary_replacement_needs_parens,
};
use super::unicorn_types::builtin_collection_type;
use super::unicorn_util::{
    in_unparenthesized_new_callee, skip_wrappers, space_around_keyword, with_edits,
};
use ts_r_ast::FnFlags;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-set-methods",
        "Prefer `Set` methods for Set operations.",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<PreferSetMethods>,
};

#[derive(Debug, Default)]
struct PreferSetMethods;

fn is_builtin_set(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    builtin_collection_type(cx, id) == Some("Set")
}

/// `new Set(x)` with the global `Set` and one non-spread argument: `x`.
fn global_set_constructor_arg(cx: &Ctx<'_, '_>, id: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let Node::New { callee, args, .. } = *ast.node(id) else {
        return None;
    };
    let &[arg] = ast.list(args) else {
        return None;
    };
    (!matches!(ast.node(arg), Node::Spread { .. }) && cx.is_global_ref(callee, "Set"))
        .then_some(arg)
}

/// The parent, with parentheses skipped, and the child on the path to it.
fn parent_np(cx: &Ctx<'_, '_>, id: NodeId) -> Option<(NodeId, NodeId)> {
    let mut cur = id;
    loop {
        let p = cx.parent(cur)?;
        if matches!(cx.node(p), Node::Paren { .. }) {
            cur = p;
        } else {
            return Some((p, cur));
        }
    }
}

fn is_ts_wrapper(node: &Node) -> bool {
    matches!(
        node,
        Node::As { .. }
            | Node::Satisfies { .. }
            | Node::NonNull { .. }
            | Node::TypeAssertion { .. }
            | Node::Instantiation { .. }
    )
}

/// The outermost of the TS wrappers (and parentheses) around `id`.
fn outermost_wrapper(cx: &Ctx<'_, '_>, id: NodeId) -> NodeId {
    let mut cur = id;
    while let Some((p, _)) = parent_np(cx, cur) {
        if is_ts_wrapper(cx.node(p)) {
            cur = p;
        } else {
            break;
        }
    }
    cur
}

/// The text with a `;` in front when it starts its expression statement and would
/// otherwise continue the previous line.
fn with_semicolon(cx: &Ctx<'_, '_>, node: NodeId, text: String) -> String {
    let start = cx.span(node).start;
    let mut cur = node;
    let first_in_statement = loop {
        match cx.parent(cur) {
            Some(p) if matches!(cx.node(p), Node::ExprStmt { .. }) => {
                break cx.span(p).start == start;
            }
            Some(p) => cur = p,
            None => break false,
        }
    };
    if first_in_statement && needs_semicolon(cx, start, &text) {
        format!(";{text}")
    } else {
        text
    }
}

/// `[...set]` with a built-in Set: `set`.
fn single_spread_set(cx: &Ctx<'_, '_>, id: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let Node::ArrayLit { elements } = *ast.node(skip_parens(ast, id)) else {
        return None;
    };
    let &[element] = ast.list(elements) else {
        return None;
    };
    let Node::Spread { arg } = *ast.node(element) else {
        return None;
    };
    is_builtin_set(cx, arg).then_some(arg)
}

/// A sync arrow with one plain parameter used exactly once: `(parameter, body)`.
fn set_callback(cx: &Ctx<'_, '_>, id: NodeId) -> Option<(NodeId, NodeId)> {
    let ast = cx.ast();
    let Node::Arrow {
        flags,
        params,
        body,
        ..
    } = *ast.node(id)
    else {
        return None;
    };
    if flags.contains(FnFlags::ASYNC) {
        return None;
    }
    let &[param] = ast.list(params) else {
        return None;
    };
    let Node::Param {
        pattern,
        rest: false,
        init: None,
        ..
    } = *ast.node(param)
    else {
        return None;
    };
    if !matches!(ast.node(pattern), Node::Ident { .. }) {
        return None;
    }
    let sem = cx.semantic();
    let s = sem.symbol_of_ident(pattern)?;
    (sem.symbol(s).refs.len() == 1).then_some((pattern, body))
}

/// `other.has(parameter)` with `other` a side-effect free built-in Set: `other`.
fn set_has_object(cx: &Ctx<'_, '_>, id: NodeId, param: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let m = method_call(ast, skip_parens(ast, id))?;
    let &[arg] = m.args else {
        return None;
    };
    (m.name == "has"
        && m.plain()
        && !matches!(ast.node(arg), Node::Spread { .. })
        && is_same_reference(cx, arg, param)
        && !is_same_reference(cx, m.obj, param)
        && is_builtin_set(cx, m.obj)
        && !has_side_effect_with_getters(ast, m.obj))
    .then_some(m.obj)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Operation {
    Intersection,
    Difference,
}

/// `other.has(v)` or `!other.has(v)`.
fn set_operation(cx: &Ctx<'_, '_>, body: NodeId, param: NodeId) -> Option<(Operation, NodeId)> {
    let ast = cx.ast();
    if let Some(other) = set_has_object(cx, body, param) {
        return Some((Operation::Intersection, other));
    }
    let Node::Unary {
        op: UnaryOp::Not,
        arg,
    } = *ast.node(skip_parens(ast, body))
    else {
        return None;
    };
    set_has_object(cx, arg, param).map(|o| (Operation::Difference, o))
}

impl PreferSetMethods {
    fn union(&self, node: NodeId, cx: &mut Ctx<'_, '_>) -> bool {
        let ast = cx.ast();
        let Some(arg) = global_set_constructor_arg(cx, node) else {
            return false;
        };
        let Node::ArrayLit { elements } = *ast.node(skip_parens(ast, arg)) else {
            return false;
        };
        let elements = ast.list(elements);
        let mut sets = Vec::with_capacity(elements.len());
        for &e in elements {
            let Node::Spread { arg } = *ast.node(e) else {
                return false;
            };
            let safe = matches!(*ast.node(skip_parens(ast, arg)), Node::New { callee, args, .. }
                    if ast.list(args).is_empty() && cx.is_global_ref(callee, "Set"))
                || !has_side_effect_with_getters(ast, arg);
            if !is_builtin_set(cx, arg) || !safe {
                return false;
            }
            sets.push(arg);
        }
        if sets.len() < 2 {
            return false;
        }
        let span = cx.span(node);
        let fix = (!has_comment_in(cx, span)).then(|| {
            let mut text = member_object_text(cx, sets[0], node);
            for &s in &sets[1..] {
                text.push_str(&format!(".union({})", paren_text(cx, s)));
            }
            if in_unparenthesized_new_callee(cx, node) {
                text = format!("({text})");
            }
            Fix::replace(span, with_semicolon(cx, node, text))
        });
        cx.report_node(
            node,
            "Use `Set#union()` instead of spreading Sets into a new Set.",
        )
        .id("prefer-set-methods/union")
        .fix_opt(fix);
        true
    }

    /// `[...a].filter(v => (!)b.has(v))`, reported on `replaced`.
    fn filter(&self, node: NodeId, replaced: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let outer = outermost_wrapper(cx, node);
        if parent_np(cx, outer).is_some_and(|(p, c)| {
            matches!(*ast.node(p), Node::Member { obj, .. } | Node::Index { obj, .. } if obj == c)
        }) {
            return;
        }
        if node == replaced && parent_np(cx, node).is_some_and(|(p, _)| is_ts_wrapper(cx.node(p))) {
            return;
        }
        let Some(m) = method_call(ast, node) else {
            return;
        };
        let &[callback] = m.args else {
            return;
        };
        if m.name != "filter" || !m.plain() {
            return;
        }
        let Some(set) = single_spread_set(cx, m.obj) else {
            return;
        };
        let Some((param, body)) = set_callback(cx, callback) else {
            return;
        };
        let Some((operation, other)) = set_operation(cx, body, param) else {
            return;
        };
        let (method, message, message_id, suggestion_id, suggestion) = match operation {
            Operation::Intersection => (
                "intersection",
                "Use `Set#intersection()` instead of filtering by `Set#has()`.",
                "prefer-set-methods/intersection",
                "prefer-set-methods/intersection-suggestion",
                "Use `Set#intersection()`.",
            ),
            Operation::Difference => (
                "difference",
                "Use `Set#difference()` instead of filtering by `Set#has()`.",
                "prefer-set-methods/difference",
                "prefer-set-methods/difference-suggestion",
                "Use `Set#difference()`.",
            ),
        };
        let span = cx.span(replaced);
        let fix = (!has_comment_in(cx, span)).then(|| {
            let mut text = format!(
                "{}.{method}({})",
                member_object_text(cx, set, replaced),
                paren_text(cx, other)
            );
            if in_unparenthesized_new_callee(cx, replaced) {
                text = format!("({text})");
            }
            with_edits(
                Fix::replace(span, with_semicolon(cx, replaced, text)),
                space_around_keyword(cx, replaced),
            )
        });
        let report = cx.report_node(replaced, message).id(message_id);
        if let Some(fix) = fix {
            report.suggest(suggestion_id, suggestion, fix);
        }
    }

    /// Reports `node` as `(!)set.method(other)`.
    #[allow(clippy::too_many_arguments)]
    fn predicate(
        &self,
        node: NodeId,
        set: NodeId,
        other: NodeId,
        method: &'static str,
        negated: bool,
        cx: &mut Ctx<'_, '_>,
    ) {
        let span = cx.span(node);
        let fix = (!has_comment_in(cx, span)).then(|| {
            let mut text = format!(
                "{}.{method}({})",
                member_object_text(cx, set, if negated { set } else { node }),
                paren_text(cx, other)
            );
            if negated {
                text = format!("!{text}");
                if unary_replacement_needs_parens(cx, node) {
                    text = format!("({text})");
                }
            }
            with_edits(
                Fix::replace(span, with_semicolon(cx, node, text)),
                space_around_keyword(cx, node),
            )
        });
        let (message, id) = if method == "isSubsetOf" {
            (
                "Use `Set#isSubsetOf()` to check whether a Set is a subset of another Set.",
                "prefer-set-methods/is-subset-of",
            )
        } else {
            (
                "Use `Set#isDisjointFrom()` to check whether Sets have any elements in common.",
                "prefer-set-methods/is-disjoint-from",
            )
        };
        cx.report_node(node, message).id(id).fix_opt(fix);
    }

    fn array_predicate(&self, node: NodeId, cx: &mut Ctx<'_, '_>) -> bool {
        let ast = cx.ast();
        let Some(m) = method_call(ast, node) else {
            return false;
        };
        let &[callback] = m.args else {
            return false;
        };
        let some = match m.name {
            "every" => false,
            "some" => true,
            _ => return false,
        };
        if !m.plain() {
            return false;
        }
        let Some(set) = single_spread_set(cx, m.obj) else {
            return false;
        };
        let Some((param, body)) = set_callback(cx, callback) else {
            return false;
        };
        let Some((operation, other)) = set_operation(cx, body, param) else {
            return false;
        };
        let method = match (some, operation) {
            (false, Operation::Intersection) | (true, Operation::Difference) => "isSubsetOf",
            _ => "isDisjointFrom",
        };
        self.predicate(node, set, other, method, some, cx);
        true
    }

    fn size_comparison(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Binary { left, op, right } = *ast.node(node) else {
            return;
        };
        let is_zero = |id: NodeId| matches!(*ast.node(skip_parens(ast, id)), Node::NumberLit { value } if value.get() == 0.0);
        let equality = matches!(op, BinaryOp::StrictEq | BinaryOp::StrictNotEq);
        let positive =
            (op == BinaryOp::Gt && is_zero(right)) || (op == BinaryOp::Lt && is_zero(left));
        if !equality && !positive {
            return;
        }
        let (size, zero) = if is_zero(right) {
            (left, right)
        } else {
            (right, left)
        };
        if !is_zero(zero) {
            return;
        }
        let Node::Member {
            obj,
            optional: false,
            prop,
        } = *ast.node(skip_parens(ast, size))
        else {
            return;
        };
        if ident_name(ast, prop) != Some("size") {
            return;
        }
        let Some(m) = method_call(ast, skip_parens(ast, obj)) else {
            return;
        };
        let &[other] = m.args else {
            return;
        };
        let method = match m.name {
            "intersection" => "isDisjointFrom",
            "difference" => "isSubsetOf",
            _ => return,
        };
        if !m.plain()
            || matches!(ast.node(other), Node::Spread { .. })
            || !is_builtin_set(cx, m.obj)
            || !is_builtin_set(cx, other)
        {
            return;
        }
        self.predicate(node, m.obj, other, method, op != BinaryOp::StrictEq, cx);
    }
}

impl Rule for PreferSetMethods {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::New, NodeKind::Call, NodeKind::Binary]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        match ast.node(node) {
            Node::New { .. } => {
                if self.union(node, cx) {
                    return;
                }
                if let Some(arg) = global_set_constructor_arg(cx, node) {
                    self.filter(skip_wrappers(ast, arg), node, cx);
                }
            }
            Node::Call { .. } => {
                if self.array_predicate(node, cx) {
                    return;
                }
                let outer = outermost_wrapper(cx, node);
                let part_of_call = parent_np(cx, outer).is_some_and(|(p, c)| match *ast.node(p) {
                    Node::Call { callee, args, .. } | Node::New { callee, args, .. } => {
                        callee == c || ast.list(args).contains(&c)
                    }
                    _ => false,
                });
                if !part_of_call {
                    self.filter(node, node, cx);
                }
            }
            Node::Binary { .. } => self.size_comparison(node, cx),
            _ => {}
        }
    }
}
