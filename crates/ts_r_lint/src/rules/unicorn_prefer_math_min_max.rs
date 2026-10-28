//! `unicorn/prefer-math-min-max`: `a > b ? b : a` style ternaries (`>`, `>=`, `<`, `<=`
//! with the same operands in the branches, compared as source text without `as`, `<T>`
//! and `!`) are reported as `Math.min()` / `Math.max()`.
//!
//! Skipped when an operand is (or may be) a BigInt, is `new Date()`, has an `as` / `<T>`
//! type other than `number` / `Number`, or names a binding that is clearly not a number
//! (a non-`number` annotation, a string / boolean / null / regexp / BigInt literal
//! initializer or parameter default, `new Date()`). The rewrite is only a suggestion
//! (`NaN` and `-0` behave differently) and is offered when no comment outside the operands
//! would be lost and reading the operands twice has no effect (no calls, assignments,
//! updates, `await`, `yield`, `new`, tagged templates or templates with expressions).

use super::prelude::*;
use super::unicorn_text::{
    call_argument_text, paren_span, static_string_value, would_remove_comments,
};
use super::unicorn_types::{BIGINT_MIXED_UNION, is_bigint_with};
use super::unicorn_util::{space_around_keyword, with_edits};
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-math-min-max",
        "Prefer `Math.min()` and `Math.max()` over ternaries for simple comparisons.",
        NO_PRESET,
        Category::Correctness,
    ),
    create: create_default::<PreferMathMinMax>,
};

#[derive(Debug, Default)]
struct PreferMathMinMax;

/// Strips parentheses, `as`, `<T>` and `!`.
fn unwrap(ast: &Ast, mut id: NodeId) -> NodeId {
    loop {
        match *ast.node(id) {
            Node::Paren { expr }
            | Node::As { expr, .. }
            | Node::TypeAssertion { expr, .. }
            | Node::NonNull { expr } => id = expr,
            _ => return id,
        }
    }
}

/// The `as` / `<T>` type of an operand (through `!`).
fn assertion_type(ast: &Ast, mut id: NodeId) -> Option<NodeId> {
    loop {
        match *ast.node(id) {
            Node::Paren { expr } | Node::NonNull { expr } => id = expr,
            Node::As { ty, .. } | Node::TypeAssertion { ty, .. } => return Some(ty),
            _ => return None,
        }
    }
}

fn is_number_keyword(ast: &Ast, ty: NodeId) -> bool {
    matches!(
        ast.node(ty),
        Node::KeywordType {
            kind: KeywordTypeKind::Number
        }
    )
}

fn is_number_type(ast: &Ast, ty: NodeId) -> bool {
    is_number_keyword(ast, ty)
        || matches!(*ast.node(ty), Node::TypeRef { name, .. } if ident_name(ast, name) == Some("Number"))
}

fn is_new_date(ast: &Ast, id: NodeId) -> bool {
    matches!(*ast.node(skip_parens(ast, id)), Node::New { callee, .. }
        if ident_name(ast, callee) == Some("Date"))
}

/// A literal that is not a number (`typeof value !== "number"`) or a static string.
fn is_non_number_literal(ast: &Ast, id: NodeId) -> bool {
    let id = skip_parens(ast, id);
    matches!(
        ast.node(id),
        Node::StringLit { .. }
            | Node::BoolLit { .. }
            | Node::NullLit {}
            | Node::RegexLit { .. }
            | Node::BigIntLit { .. }
    ) || static_string_value(ast, id).is_some()
}

impl Rule for PreferMathMinMax {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Conditional]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Conditional { test, cons, alt } = *cx.node(node) else {
            return;
        };
        let Node::Binary { left, op, right } = *ast.node(skip_parens(ast, test)) else {
            return;
        };
        let greater = matches!(op, BinaryOp::Gt | BinaryOp::GtEq);
        let less = matches!(op, BinaryOp::Lt | BinaryOp::LtEq);
        if !greater && !less {
            return;
        }
        if [left, right]
            .iter()
            .any(|&n| is_bigint_with(cx, n, &BIGINT_MIXED_UNION) || is_new_date(ast, n))
        {
            return;
        }
        let text = |n: NodeId| cx.text(unwrap(ast, n));
        let (l, r, c, a) = (text(left), text(right), text(cons), text(alt));
        let method = if (greater && l == a && r == c) || (less && l == c && r == a) {
            "min"
        } else if (greater && l == c && r == a) || (less && l == a && r == c) {
            "max"
        } else {
            return;
        };
        for operand in [left, right] {
            if !operand_may_be_number(cx, operand) {
                return;
            }
        }
        let suggestion = (!would_remove_comments(
            cx,
            cx.span(node),
            &[paren_span(cx, left), paren_span(cx, right)],
        ) && effect_free(ast, left, 0)
            && effect_free(ast, right, 0))
        .then(|| {
            let args = format!(
                "{}, {}",
                call_argument_text(cx, left),
                call_argument_text(cx, right)
            );
            with_edits(
                Fix::replace(cx.span(node), format!("Math.{method}({args})")),
                space_around_keyword(cx, node),
            )
        });
        let msg = format!("Prefer `Math.{method}()` to simplify ternary expressions.");
        let report = cx.report_node(node, msg).id("prefer-math-min-max");
        if let Some(fix) = suggestion {
            report.suggest(
                "replace-with-math-min-max",
                format!("Replace with `Math.{method}()`."),
                fix,
            );
        }
    }
}

/// False when the operand's assertion or binding says it isn't a number.
fn operand_may_be_number(cx: &Ctx<'_, '_>, operand: NodeId) -> bool {
    let ast = cx.ast();
    let expr = unwrap(ast, operand);
    if let Some(ty) = assertion_type(ast, operand)
        && !is_number_type(ast, ty)
    {
        return false;
    }
    if !matches!(ast.node(expr), Node::Ident { .. }) {
        return true;
    }
    let sem = cx.semantic();
    let Some(s) = sem.symbol_of_ident(expr) else {
        return true;
    };
    for decl in &sem.symbol(s).decls {
        match decl.kind {
            DeclKind::Param => {
                let Some(p) = cx.parent(decl.name) else {
                    continue;
                };
                let default = match *ast.node(p) {
                    Node::Param {
                        pattern,
                        type_ann,
                        init,
                        ..
                    } if pattern == decl.name => {
                        if type_ann.is_some_and(|t| !is_number_keyword(ast, t)) {
                            return false;
                        }
                        init
                    }
                    Node::AssignPattern { left, right } if left == decl.name => Some(right),
                    Node::BindingProperty {
                        value: None, init, ..
                    } => init,
                    _ => None,
                };
                if let Some(d) = default
                    && (is_non_number_literal(ast, d) || is_bigint_with(cx, d, &BIGINT_MIXED_UNION))
                {
                    return false;
                }
            }
            DeclKind::Var | DeclKind::Let | DeclKind::Const | DeclKind::Using => {
                let Node::VarDeclarator {
                    name,
                    type_ann,
                    init,
                    ..
                } = *ast.node(decl.node)
                else {
                    continue;
                };
                if name == decl.name && type_ann.is_some_and(|t| !is_number_keyword(ast, t)) {
                    return false;
                }
                if let Some(init) = init
                    && (is_non_number_literal(ast, init)
                        || is_bigint_with(cx, init, &BIGINT_MIXED_UNION)
                        || is_new_date(ast, init))
                {
                    return false;
                }
            }
            _ => {}
        }
    }
    true
}

/// Reading the operand twice is the same as once: nothing in it calls, writes or waits.
fn effect_free(ast: &Ast, id: NodeId, depth: u32) -> bool {
    if depth > 200 {
        return false;
    }
    let id = unwrap(ast, id);
    match *ast.node(id) {
        Node::Assign { .. }
        | Node::Await { .. }
        | Node::Call { .. }
        | Node::ImportCall { .. }
        | Node::New { .. }
        | Node::TaggedTemplate { .. }
        | Node::Update { .. }
        | Node::Yield { .. } => false,
        Node::TemplateLit { parts } => ast.list(parts).len() <= 1,
        Node::Arrow { .. }
        | Node::FunctionExpr { .. }
        | Node::FunctionDecl { .. }
        | Node::ClassExpr { .. }
        | Node::ClassDecl { .. } => true,
        _ => {
            let mut ok = true;
            ast.for_each_child(id, |c| {
                if ok && !effect_free(ast, c, depth + 1) {
                    ok = false;
                }
            });
            ok
        }
    }
}
