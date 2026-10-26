//! `unicorn/no-negated-condition`: an `if` with an `else` (not `else if`) or a `?:` whose
//! test is negated: `!x`, `a != b`, `a !== b`, or an `&&`/`||` chain of only those. The fix
//! inverts the test (De Morgan on the chain: operators swap, `!` goes, `!=` becomes `==`)
//! and swaps the branches with the comments before them; a non-block `if` branch gets
//! braces. No fix when a comment follows a branch, or when the first operand's argument
//! starts with `{`, `function`, `class` (it would start a statement). Removing a leading `!`
//! spaces a glued keyword, wraps a `return`/`throw` argument whose next token is on another
//! line (and gives up for `yield`), or adds a `;` against ASI.

use super::prelude::*;
use super::unicorn_util::{
    fix_of, keyword_argument_parent, next_token_start, parenthesize_keyword_argument,
    prev_token_end,
};
use super::unicorn_util::{joins_previous_statement, space_around_keyword};
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-negated-condition",
        "Disallow negated conditions.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<NoNegatedCondition>,
};

#[derive(Debug, Default)]
struct NoNegatedCondition;

fn logical_op(ast: &Ast, id: NodeId) -> Option<BinaryOp> {
    match *ast.node(id) {
        Node::Binary {
            op: op @ (BinaryOp::LogicalAnd | BinaryOp::LogicalOr),
            ..
        } => Some(op),
        _ => None,
    }
}

fn operands(ast: &Ast, id: NodeId, op: BinaryOp, out: &mut Vec<NodeId>, depth: u32) {
    let id = skip_parens(ast, id);
    match *ast.node(id) {
        Node::Binary {
            left, op: o, right, ..
        } if o == op && depth < 500 => {
            operands(ast, left, op, out, depth + 1);
            operands(ast, right, op, out, depth + 1);
        }
        _ => out.push(id),
    }
}

fn is_negated(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(id),
        Node::Unary {
            op: UnaryOp::Not,
            ..
        } | Node::Binary {
            op: BinaryOp::NotEq | BinaryOp::StrictNotEq,
            ..
        }
    )
}

/// The argument starts with `{`, `class`, `function`, `async function` or `let [`, unless
/// parenthesized.
fn starts_like_declaration(cx: &Ctx<'_, '_>, arg: NodeId) -> bool {
    if matches!(cx.node(arg), Node::Paren { .. }) {
        return false;
    }
    let text = cx.text(arg);
    let word = |w: &str| {
        text.strip_prefix(w)
            .is_some_and(|r| !r.starts_with(is_ident_char))
    };
    if text.starts_with('{') || word("class") || word("function") {
        return true;
    }
    if word("async") {
        let rest = cx.span(arg).start as usize + 5;
        let next = skip_trivia_forward(cx.src(), rest);
        return cx.src()[next..].starts_with("function");
    }
    if word("let") {
        let next = skip_trivia_forward(cx.src(), cx.span(arg).start as usize + 3);
        return cx.src()[next..].starts_with('[');
    }
    false
}

/// Edits turning the negated `test` into its opposite.
fn invert(cx: &Ctx<'_, '_>, test: NodeId, edits: &mut Vec<(Span, String)>, depth: u32) {
    let ast = cx.ast();
    let test = skip_parens(ast, test);
    if depth > 500 {
        return;
    }
    match *ast.node(test) {
        Node::Binary {
            left,
            op: op @ (BinaryOp::LogicalAnd | BinaryOp::LogicalOr),
            right,
        } => {
            let at = next_token_start(cx, cx.span(left).end);
            let swapped = if op == BinaryOp::LogicalAnd {
                "||"
            } else {
                "&&"
            };
            edits.push((Span::new(at, at + 2), swapped.to_owned()));
            invert(cx, left, edits, depth + 1);
            invert(cx, right, edits, depth + 1);
        }
        Node::Unary { .. } => {
            let start = cx.span(test).start;
            edits.push((Span::new(start, start + 1), String::new()));
        }
        Node::Binary { left, op, .. } => {
            let at = next_token_start(cx, cx.span(left).end);
            let len = op.as_str().len() as u32;
            edits.push((Span::new(at, at + len), format!("={}", &op.as_str()[1..])));
        }
        _ => {}
    }
}

/// The text a branch moves with (its comments before it, braces for an `if` statement
/// body), or `None` when a comment follows it.
fn branch_text(cx: &Ctx<'_, '_>, branch: NodeId, is_if: bool) -> Option<(Span, String)> {
    let ast = cx.ast();
    let span = cx.span(branch);
    let next = next_token_start(cx, span.end);
    if !ast.comments_between(span.end, next).is_empty() {
        return None;
    }
    let prev = prev_token_end(cx, span.start);
    let start = ast
        .comments_between(prev, span.start)
        .first()
        .map_or(span.start, |c| c.span.start);
    let range = Span::new(start, span.end);
    let mut text = range.text(cx.src()).to_owned();
    if is_if && !matches!(ast.node(branch), Node::Block { .. }) {
        text = format!("{{{text}}}");
    }
    Some((range, text))
}

impl NoNegatedCondition {
    fn fix(&self, cx: &Ctx<'_, '_>, node: NodeId, test: NodeId, ops: &[NodeId]) -> Option<Fix> {
        let ast = cx.ast();
        let src = cx.src();
        let (is_if, cons, alt) = match *ast.node(node) {
            Node::If {
                cons, alt: Some(a), ..
            } => (true, cons, a),
            Node::Conditional { cons, alt, .. } => (false, cons, alt),
            _ => return None,
        };
        let first_unary = match *ast.node(ops[0]) {
            Node::Unary { arg, .. } => Some(arg),
            _ => None,
        };
        if first_unary.is_some_and(|arg| starts_like_declaration(cx, arg)) {
            return None;
        }
        let mut edits = Vec::new();
        if is_if && let Node::Unary { arg, .. } = *ast.node(test) {
            let mut p = arg;
            while let Node::Paren { expr } = *ast.node(p) {
                let s = cx.span(p);
                edits.push((Span::new(s.start, s.start + 1), String::new()));
                edits.push((Span::new(s.end - 1, s.end), String::new()));
                p = expr;
            }
        }
        invert(cx, test, &mut edits, 0);
        let (cr, ct) = branch_text(cx, cons, is_if)?;
        let (ar, at) = branch_text(cx, alt, is_if)?;
        if ct != at {
            edits.push((cr, at));
            edits.push((ar, ct));
        }
        if !is_if && first_unary.is_some() {
            edits.extend(space_around_keyword(cx, node));
            let bang = cx.span(test).start;
            if src.as_bytes().get(bang as usize) == Some(&b'!') {
                let second = next_token_start(cx, bang + 1);
                let lines = cx.file.lines;
                let same_line = lines.line_of(bang) == lines.line_of(second);
                match keyword_argument_parent(cx, test) {
                    Some(kp) if !same_line => {
                        if matches!(ast.node(kp), Node::Yield { .. }) {
                            return None;
                        }
                        edits.extend(parenthesize_keyword_argument(cx, kp));
                    }
                    _ => {
                        let c = src[second as usize..].chars().next();
                        if matches!(
                            c,
                            Some('[' | '(' | '/' | '`' | '+' | '-' | '*' | ',' | '.' | '<')
                        ) && joins_previous_statement(cx, node)
                        {
                            edits.push((Span::at(cx.span(node).start), ";".to_owned()));
                        }
                    }
                }
            }
        }
        Some(fix_of(edits))
    }
}

impl Rule for NoNegatedCondition {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::If, NodeKind::Conditional]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let test = match *cx.node(node) {
            Node::If {
                test, alt: Some(a), ..
            } if !matches!(ast.node(a), Node::If { .. }) => test,
            Node::Conditional { test, .. } => test,
            _ => return,
        };
        let test = skip_parens(ast, test);
        let mut ops = Vec::new();
        match logical_op(ast, test) {
            Some(op) => operands(ast, test, op, &mut ops, 0),
            None => ops.push(test),
        }
        if !ops.iter().all(|&o| is_negated(ast, o)) {
            return;
        }
        let fix = self.fix(cx, node, test, &ops);
        cx.report_node(test, "Unexpected negated condition.")
            .id("no-negated-condition")
            .fix_opt(fix);
    }
}
