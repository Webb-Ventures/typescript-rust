//! `unicorn/no-useless-logical-operand`: boolean literals that don't change an outermost
//! `&&`/`||` chain (parentheses looked through, TS wrappers on a literal too).
//!
//! - A leading absorbing literal (`false && ..`, `true || ..`) is the whole value.
//! - An identity literal (`true` in `&&`, `false` in `||`) drops out, except as the last
//!   operand when the chain's value is used as more than a condition and the other operands
//!   aren't all known booleans. A chain of only identity literals keeps the last one.
//!
//! Skipped: a chain that is called, used as a tag or deleted (`this`/reference semantics),
//! and a lone string left as a statement of a directive prologue. The fix rewrites the
//! chain (one operand through `replaceNodeWithExpression`'s rules, several joined with
//! parentheses where an operand needs them) unless a comment would be lost.

use super::prelude::*;
use super::unicorn_conditions::{logical_child_text, unwrap_ts_and_parens};
use super::unicorn_no_duplicate_logical_operands::is_reference_context;
use super::unicorn_no_useless_coercion::replace_with_expression;
use super::unicorn_text::{
    is_parenthesized, needs_parens_at_expression_start, needs_semicolon, paren_span,
    would_remove_comments,
};
use super::unicorn_types::is_boolean;
use super::unicorn_util::is_boolean_context;
use super::unicorn_util::space_around_keyword;
use super::unicorn_util::{is_outermost_logical, logical_operands};
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-useless-logical-operand",
        "Disallow unnecessary operands in logical expressions involving boolean literals.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<NoUselessLogicalOperand>,
};

#[derive(Debug, Default)]
struct NoUselessLogicalOperand;

fn is_bool_literal(ast: &Ast, id: NodeId, value: bool) -> bool {
    matches!(*ast.node(unwrap_ts_and_parens(ast, id)), Node::BoolLit { value: v } if v == value)
}

/// `isBoolean`, also taking `x satisfies boolean` (or `as`/`<boolean>`) at its annotation.
fn is_known_boolean(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    match *ast.node(skip_parens(ast, id)) {
        Node::As { ty, .. } | Node::Satisfies { ty, .. } | Node::TypeAssertion { ty, .. }
            if matches!(
                ast.node(ty),
                Node::KeywordType {
                    kind: KeywordTypeKind::Boolean
                } | Node::BoolLit { .. }
            ) =>
        {
            true
        }
        _ => is_boolean(cx, id),
    }
}

/// `node` (through parentheses) is an expression statement of a directive prologue.
fn in_directive_prologue(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let Some(stmt) = parent_skip_parens(ast, cx.file.parents, node) else {
        return false;
    };
    if !matches!(ast.node(stmt), Node::ExprStmt { .. }) {
        return false;
    }
    let Some(owner) = cx.parent(stmt) else {
        return false;
    };
    let stmts = match *ast.node(owner) {
        Node::Program { stmts } => stmts,
        Node::Block { stmts }
            if cx.parent(owner).is_some_and(|f| {
                matches!(
                    ast.node(f),
                    Node::FunctionDecl { .. } | Node::FunctionExpr { .. } | Node::Arrow { .. }
                ) || matches!(ast.node(f), Node::MethodDef { .. })
            }) =>
        {
            stmts
        }
        _ => return false,
    };
    ast.list(stmts)
        .iter()
        .take_while(|&&s| s != stmt)
        .all(|&s| matches!(ast.node(s), Node::Directive { .. }))
}

/// The operand reported and the operands the chain becomes.
fn problem(cx: &Ctx<'_, '_>, operands: &[NodeId], op: BinaryOp) -> Option<(NodeId, Vec<NodeId>)> {
    let ast = cx.ast();
    let and = op == BinaryOp::LogicalAnd;
    if is_bool_literal(ast, operands[0], !and) {
        return Some((operands[0], vec![operands[0]]));
    }
    let last = operands.len() - 1;
    let removable: Vec<usize> = (0..operands.len())
        .filter(|&i| {
            is_bool_literal(ast, operands[i], and)
                && (i < last
                    || parent_skip_parens(ast, cx.file.parents, operands[i])
                        .is_some_and(|p| is_boolean_context(cx, p))
                    || operands[..last].iter().all(|&o| is_known_boolean(cx, o)))
        })
        .collect();
    let first = *removable.first()?;
    let mut kept: Vec<NodeId> = (0..operands.len())
        .filter(|i| !removable.contains(i))
        .map(|i| operands[i])
        .collect();
    if kept.is_empty() {
        kept.push(operands[removable[removable.len() - 1]]);
    }
    Some((operands[first], kept))
}

fn joined_text(cx: &Ctx<'_, '_>, node: NodeId, operands: &[NodeId], op: BinaryOp) -> String {
    let ast = cx.ast();
    let parts: Vec<String> = operands
        .iter()
        .enumerate()
        .map(|(i, &o)| {
            let text = logical_child_text(cx, o, op);
            if i == 0
                && !is_parenthesized(cx, o)
                && needs_parens_at_expression_start(cx, unwrap_ts_and_parens(ast, o), node)
            {
                format!("({text})")
            } else {
                text
            }
        })
        .collect();
    let replacement = parts.join(&format!(" {} ", op.as_str()));
    if needs_semicolon(cx, cx.span(node).start, &replacement) {
        format!(";{replacement}")
    } else {
        replacement
    }
}

impl Rule for NoUselessLogicalOperand {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Binary]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Binary { op, .. } = *cx.node(node) else {
            return;
        };
        if !matches!(op, BinaryOp::LogicalAnd | BinaryOp::LogicalOr)
            || !is_outermost_logical(cx, node)
            || is_reference_context(cx, node)
        {
            return;
        }
        let operands = logical_operands(ast, node, op);
        let Some((target, kept)) = problem(cx, &operands, op) else {
            return;
        };
        if let [only] = kept[..]
            && matches!(
                ast.node(unwrap_ts_and_parens(ast, only)),
                Node::StringLit { .. }
            )
            && in_directive_prologue(cx, node)
        {
            return;
        }
        let span = cx.span(node);
        let spans: Vec<Span> = kept.iter().map(|&o| paren_span(cx, o)).collect();
        let fix = if would_remove_comments(cx, span, &spans) {
            None
        } else if let [only] = kept[..] {
            replace_with_expression(cx, node, only)
        } else {
            Some(Fix::replace(span, joined_text(cx, node, &kept, op)))
        }
        .map(|mut fix| {
            for (s, t) in space_around_keyword(cx, node) {
                fix = fix.and(s, t);
            }
            fix
        });
        cx.report(
            cx.span(target),
            format!("Simplify this `{}` expression.", op.as_str()),
        )
        .id("no-useless-logical-operand")
        .fix_opt(fix);
    }
}
