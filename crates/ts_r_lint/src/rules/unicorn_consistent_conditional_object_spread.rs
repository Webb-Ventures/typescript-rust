//! `unicorn/consistent-conditional-object-spread`: a conditional spread inside an object
//! literal (`{...(a ? b : {})}`, `{...(a && b)}`) written in one style.
//!
//! - `logical` (the default): a `?:` with exactly one branch that spreads nothing (`{}`,
//!   `undefined` or `null`) becomes `test && kept` (the test negated when the empty branch
//!   is the consequent, dropping a leading `!`). Skipped when the test already is the kept
//!   branch (`a ? a : {}`, `!a ? {} : a`) or a nullish check of it that `&&` would change
//!   (`a == null ? {} : a`). The `&&` operands of such a spread that are boolean casts
//!   (`!!a`, a global `Boolean(a)`) are reported too: a falsy value spreads nothing anyway.
//! - `ternary`: `test && kept` (kept not empty) becomes `test ? kept : {}`, unless the
//!   operands are the same or the test is a not-nullish check of the kept value.
//!
//! "The same" is unicorn's reference comparison, else the same node kind with the same
//! text (an update expression is never the same). Fixes are dropped when they would remove
//! a comment outside the kept operands.

use super::prelude::*;
use super::unicorn_conditions::{logical_child_text, negated_expression_text};
use super::unicorn_text::{is_parenthesized, paren_span, paren_text, would_remove_comments};
use super::unicorn_util::{is_global_boolean_call, is_same_reference};
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/consistent-conditional-object-spread",
        "Enforce consistent conditional object spread style.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<ConsistentConditionalObjectSpread>,
};

#[derive(Debug)]
struct ConsistentConditionalObjectSpread {
    ternary: bool,
}

impl FromOptions for ConsistentConditionalObjectSpread {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let ternary = match opts.first().and_then(Value::as_str) {
            None | Some("logical") => false,
            Some("ternary") => true,
            Some(other) => return Err(format!("unknown style `{other}`")),
        };
        Ok(Self { ternary })
    }
}

const MAX_DEPTH: u32 = 64;

/// `{}`, `undefined` or `null`: spreads nothing.
fn is_empty_branch(ast: &Ast, id: NodeId) -> bool {
    let id = skip_parens(ast, id);
    match *ast.node(id) {
        Node::ObjectLit { props } => ast.list(props).is_empty(),
        Node::NullLit {} => true,
        _ => ident_name(ast, id) == Some("undefined"),
    }
}

/// `id` (through parentheses) is the argument of a spread in an object literal.
fn is_object_spread_argument(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let mut cur = id;
    while let Some(p) = cx.parent(cur) {
        match *cx.node(p) {
            Node::Paren { .. } => cur = p,
            Node::Spread { arg } if arg == cur => {
                return cx
                    .parent(p)
                    .is_some_and(|o| matches!(cx.node(o), Node::ObjectLit { .. }));
            }
            _ => return false,
        }
    }
    false
}

fn is_logical(op: BinaryOp) -> bool {
    matches!(
        op,
        BinaryOp::LogicalAnd | BinaryOp::LogicalOr | BinaryOp::Coalesce
    )
}

fn not_argument(ast: &Ast, id: NodeId) -> Option<NodeId> {
    match *ast.node(skip_parens(ast, id)) {
        Node::Unary {
            op: UnaryOp::Not,
            arg,
        } => Some(arg),
        _ => None,
    }
}

/// The same reference, or the same kind of node with the same text.
fn is_same_node(cx: &Ctx<'_, '_>, a: NodeId, b: NodeId, depth: u32) -> bool {
    if depth > MAX_DEPTH {
        return false;
    }
    if is_same_reference(cx, a, b) {
        return true;
    }
    let ast = cx.ast();
    let (a, b) = (skip_parens(ast, a), skip_parens(ast, b));
    if ast.kind(a) != ast.kind(b) {
        return false;
    }
    match (*ast.node(a), *ast.node(b)) {
        (Node::Await { arg: x }, Node::Await { arg: y }) => is_same_node(cx, x, y, depth + 1),
        (
            Node::Binary {
                left: l1,
                op: o1,
                right: r1,
            },
            Node::Binary {
                left: l2,
                op: o2,
                right: r2,
            },
        ) if is_logical(o1) || is_logical(o2) => {
            o1 == o2 && is_same_node(cx, l1, l2, depth + 1) && is_same_node(cx, r1, r2, depth + 1)
        }
        (Node::Unary { op: o1, arg: x }, Node::Unary { op: o2, arg: y }) => {
            o1 == o2 && is_same_node(cx, x, y, depth + 1)
        }
        (Node::Update { .. }, _) => false,
        _ => cx.text(a) == cx.text(b),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum NullishKind {
    Null,
    Undefined,
    Both,
}

struct NullishCheck {
    reference: NodeId,
    kind: NullishKind,
    true_when_nullish: bool,
}

fn nullish_kind(ast: &Ast, id: NodeId) -> Option<NullishKind> {
    let id = skip_parens(ast, id);
    match ast.node(id) {
        Node::NullLit {} => Some(NullishKind::Null),
        _ if ident_name(ast, id) == Some("undefined") => Some(NullishKind::Undefined),
        _ => None,
    }
}

/// `x == null`, `undefined !== x` and the like.
fn nullish_binary_check(ast: &Ast, id: NodeId) -> Option<NullishCheck> {
    let Node::Binary { left, op, right } = *ast.node(skip_parens(ast, id)) else {
        return None;
    };
    let (loose, true_when_nullish) = match op {
        BinaryOp::Eq => (true, true),
        BinaryOp::StrictEq => (false, true),
        BinaryOp::NotEq => (true, false),
        BinaryOp::StrictNotEq => (false, false),
        _ => return None,
    };
    let (lk, rk) = (nullish_kind(ast, left), nullish_kind(ast, right));
    let (reference, kind) = match (lk, rk) {
        (Some(k), None) => (right, k),
        (None, Some(k)) => (left, k),
        _ => return None,
    };
    Some(NullishCheck {
        reference,
        kind: if loose { NullishKind::Both } else { kind },
        true_when_nullish,
    })
}

/// A check that `reference` is (or is not) nullish: a loose comparison with `null` or
/// `undefined`, or strict ones against both joined by `||` (`&&` for "not nullish").
fn nullish_test(cx: &Ctx<'_, '_>, id: NodeId) -> Option<NullishCheck> {
    let ast = cx.ast();
    if let Some(c) = nullish_binary_check(ast, id)
        && c.kind == NullishKind::Both
    {
        return Some(c);
    }
    let Node::Binary { left, op, right } = *ast.node(skip_parens(ast, id)) else {
        return None;
    };
    let l = nullish_binary_check(ast, left)?;
    let r = nullish_binary_check(ast, right)?;
    if !is_same_node(cx, l.reference, r.reference, 0) {
        return None;
    }
    let null_and_undefined = matches!(
        (l.kind, r.kind),
        (NullishKind::Null, NullishKind::Undefined) | (NullishKind::Undefined, NullishKind::Null)
    );
    if !null_and_undefined {
        return None;
    }
    let true_when_nullish = match op {
        BinaryOp::LogicalOr if l.true_when_nullish && r.true_when_nullish => true,
        BinaryOp::LogicalAnd if !l.true_when_nullish && !r.true_when_nullish => false,
        _ => return None,
    };
    Some(NullishCheck {
        reference: l.reference,
        kind: NullishKind::Both,
        true_when_nullish,
    })
}

/// `getConditionalExpressionChildText`: `id` with its parentheses, wrapped in new ones when
/// it binds looser than a `?:` operand.
fn conditional_child_text(cx: &Ctx<'_, '_>, id: NodeId) -> String {
    let text = paren_text(cx, id);
    let needs = !is_parenthesized(cx, id)
        && matches!(
            cx.node(skip_parens(cx.ast(), id)),
            Node::Await { .. }
                | Node::Assign { .. }
                | Node::Yield { .. }
                | Node::Seq { .. }
                | Node::As { .. }
                | Node::TypeAssertion { .. }
        );
    if needs {
        format!("({text})")
    } else {
        text.to_owned()
    }
}

fn and_operand_text(cx: &Ctx<'_, '_>, id: NodeId) -> String {
    logical_child_text(cx, skip_parens(cx.ast(), id), BinaryOp::LogicalAnd)
}

fn conditional_problem(cx: &mut Ctx<'_, '_>, node: NodeId) {
    let ast = cx.ast();
    let Node::Conditional { test, cons, alt } = *cx.node(node) else {
        return;
    };
    let alt_empty = is_empty_branch(ast, alt);
    if alt_empty == is_empty_branch(ast, cons) {
        return;
    }
    let kept = if alt_empty { cons } else { alt };
    let negated_arg = not_argument(ast, test);
    let same_as_test = if alt_empty {
        is_same_node(cx, test, kept, 0)
    } else {
        negated_arg.is_some_and(|a| is_same_node(cx, a, kept, 0))
    };
    let nullish_skip = nullish_test(cx, test).is_some_and(|n| {
        (if n.true_when_nullish {
            !alt_empty
        } else {
            alt_empty
        }) && is_same_node(cx, n.reference, kept, 0)
    });
    if same_as_test || nullish_skip {
        return;
    }
    let test_text = match (alt_empty, negated_arg) {
        (true, _) => and_operand_text(cx, test),
        (false, Some(arg)) => and_operand_text(cx, arg),
        (false, None) => negated_expression_text(cx, test),
    };
    let preserved = match (alt_empty, negated_arg) {
        (false, Some(arg)) => arg,
        _ => test,
    };
    let span = cx.span(node);
    let fix =
        (!would_remove_comments(cx, span, &[paren_span(cx, preserved), paren_span(cx, kept)]))
            .then(|| {
                Fix::replace(
                    span,
                    format!("{test_text} && {}", and_operand_text(cx, kept)),
                )
            });
    cx.report(span, "Prefer logical conditional object spreads.")
        .id("consistent-conditional-object-spread")
        .fix_opt(fix);
}

fn ternary_problem(cx: &mut Ctx<'_, '_>, node: NodeId) {
    let ast = cx.ast();
    let Node::Binary {
        left,
        op: BinaryOp::LogicalAnd,
        right,
    } = *cx.node(node)
    else {
        return;
    };
    if is_empty_branch(ast, right) || is_same_node(cx, left, right, 0) {
        return;
    }
    if nullish_test(cx, left)
        .is_some_and(|n| !n.true_when_nullish && is_same_node(cx, n.reference, right, 0))
    {
        return;
    }
    let span = cx.span(node);
    let fix = (!would_remove_comments(cx, span, &[paren_span(cx, left), paren_span(cx, right)]))
        .then(|| {
            Fix::replace(
                span,
                format!(
                    "{} ? {} : {{}}",
                    conditional_child_text(cx, left),
                    conditional_child_text(cx, right)
                ),
            )
        });
    cx.report(span, "Prefer ternary conditional object spreads.")
        .id("consistent-conditional-object-spread")
        .fix_opt(fix);
}

/// `a` from `!!a` or a global `Boolean(a)`.
fn boolean_cast_argument(cx: &Ctx<'_, '_>, id: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let id = skip_parens(ast, id);
    if let Some(inner) = not_argument(ast, id)
        && let Some(arg) = not_argument(ast, inner)
    {
        return Some(arg);
    }
    if is_global_boolean_call(cx, id)
        && let Node::Call { args, .. } = *ast.node(id)
    {
        return ast.list(args).first().copied();
    }
    None
}

fn boolean_cast_problem(cx: &mut Ctx<'_, '_>, cast: NodeId) {
    let Some(arg) = boolean_cast_argument(cx, cast) else {
        return;
    };
    let cast = skip_parens(cx.ast(), cast);
    let span = cx.span(cast);
    let fix = (!would_remove_comments(cx, span, &[paren_span(cx, arg)]))
        .then(|| Fix::replace(span, and_operand_text(cx, arg)));
    cx.report(
        span,
        "Unnecessary boolean cast. A falsy value spreads nothing.",
    )
    .id("unnecessary-boolean-cast")
    .fix_opt(fix);
}

/// In `a && b && c`, every operand but the last decides only when it is falsy.
fn boolean_cast_problems(cx: &mut Ctx<'_, '_>, node: NodeId) {
    let ast = cx.ast();
    let Node::Binary {
        left,
        op: BinaryOp::LogicalAnd,
        ..
    } = *cx.node(node)
    else {
        return;
    };
    let mut cur = skip_parens(ast, left);
    // Each step moves to a strictly smaller left operand.
    while let Node::Binary {
        left,
        op: BinaryOp::LogicalAnd,
        right,
    } = *ast.node(cur)
    {
        boolean_cast_problem(cx, right);
        cur = skip_parens(ast, left);
    }
    boolean_cast_problem(cx, cur);
}

impl Rule for ConsistentConditionalObjectSpread {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Conditional, NodeKind::Binary]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let is_conditional = matches!(cx.node(node), Node::Conditional { .. });
        let is_and = matches!(
            cx.node(node),
            Node::Binary {
                op: BinaryOp::LogicalAnd,
                ..
            }
        );
        if !(is_conditional || is_and) || !is_object_spread_argument(cx, node) {
            return;
        }
        match (self.ternary, is_conditional) {
            (true, false) => ternary_problem(cx, node),
            (true, true) => {}
            (false, true) => conditional_problem(cx, node),
            (false, false) => boolean_cast_problems(cx, node),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::config::{ConfigFile, ConfigKind, ResolveOptions, objects_from_eslint_json};

    fn lint(src: &str) -> usize {
        let reg = crate::Registry::global();
        let json = serde_json::json!([{ "files": ["**/*.js"], "rules": {
            "unicorn/consistent-conditional-object-spread": "error" } }]);
        let objs = objects_from_eslint_json(&json, reg).unwrap();
        let file = ConfigFile::new(ConfigKind::Eslint, None, "/p".into(), objs);
        let fc = file.file_config("a.js", &ResolveOptions::new()).unwrap();
        crate::lint_source(
            "/p/a.js",
            src,
            &fc,
            &crate::NoTypeInfo,
            crate::types::FileKey(0),
        )
        .diagnostics
        .len()
    }

    #[test]
    fn long_chains_and_malformed_input() {
        let chain = vec!["!!a"; 5_000].join(" && ");
        assert_eq!(lint(&format!("({{...({chain} && b)}});")), 5_000);
        for src in ["({...(a ? ", "({...(a && ", "({...(!!", "({...(a ? {} :"] {
            let _ = lint(src);
        }
        assert_eq!(lint("({...(a ? b : {})});"), 1);
    }
}
