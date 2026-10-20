//! `no-extra-boolean-cast`: disallow unnecessary boolean casts.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-extra-boolean-cast",
        "Disallow unnecessary boolean casts",
        ESLINT_REC,
        Category::Correctness,
    )
    .fixable(),
    create: create::<NoExtraBooleanCast>,
};

#[derive(Debug, Default)]
struct NoExtraBooleanCast {
    /// Logical operands (`||`, `&&`) of a boolean context are boolean contexts.
    logical: bool,
    /// Also `??` right sides, conditional branches and the last sequence expression.
    inner: bool,
}

impl FromOptions for NoExtraBooleanCast {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        let inner = bool_field(o, "enforceForInnerExpressions", false);
        Ok(NoExtraBooleanCast {
            logical: inner || bool_field(o, "enforceForLogicalOperands", false),
            inner,
        })
    }
}

/// Where a replaced expression sits, for deciding whether it needs parentheses.
#[derive(Clone, Copy)]
enum Slot {
    /// Any expression fits (statement tests, parenthesized nodes).
    Any,
    /// Minimum precedence.
    Min(u8),
    /// Operand of a logical operator.
    Logical(BinaryOp, bool),
}

impl NoExtraBooleanCast {
    fn is_boolean_call(cx: &Ctx<'_, '_>, call: NodeId) -> bool {
        let ast = cx.ast();
        let callee = match *ast.node(call) {
            Node::Call { callee, .. } | Node::New { callee, .. } => callee,
            _ => return false,
        };
        let callee = skip_parens(ast, callee);
        cx.is_global_ref(callee, "Boolean")
    }

    /// The topmost paren wrapper of `node` (or `node`) and its parent.
    fn context(cx: &Ctx<'_, '_>, node: NodeId) -> (NodeId, Option<NodeId>) {
        let ast = cx.ast();
        let mut cur = node;
        while let Some(p) = cx.parent(cur) {
            if matches!(ast.node(p), Node::Paren { .. }) {
                cur = p;
            } else {
                return (cur, Some(p));
            }
        }
        (cur, None)
    }

    /// True when `node` is evaluated only for its truthiness.
    fn in_boolean_context(&self, cx: &Ctx<'_, '_>, node: NodeId, depth: u32) -> bool {
        if depth > 200 {
            return false;
        }
        let ast = cx.ast();
        let (cur, Some(p)) = Self::context(cx, node) else {
            return false;
        };
        match *ast.node(p) {
            Node::If { test, .. } | Node::While { test, .. } | Node::DoWhile { test, .. } => {
                test == cur
            }
            Node::For { test, .. } => test == Some(cur),
            Node::Conditional { test, .. } if test == cur => true,
            Node::Conditional { .. } => self.inner && self.in_boolean_context(cx, p, depth + 1),
            Node::Unary {
                op: UnaryOp::Not, ..
            } => true,
            Node::Call { args, .. } | Node::New { args, .. } => {
                ast.list(args).first() == Some(&cur) && Self::is_boolean_call(cx, p)
            }
            Node::Binary {
                op: BinaryOp::LogicalOr | BinaryOp::LogicalAnd,
                ..
            } => self.logical && self.in_boolean_context(cx, p, depth + 1),
            Node::Binary {
                op: BinaryOp::Coalesce,
                right,
                ..
            } => self.inner && right == cur && self.in_boolean_context(cx, p, depth + 1),
            Node::Seq { exprs } => {
                self.inner
                    && ast.list(exprs).last() == Some(&cur)
                    && self.in_boolean_context(cx, p, depth + 1)
            }
            _ => false,
        }
    }

    fn slot(cx: &Ctx<'_, '_>, node: NodeId) -> Slot {
        let ast = cx.ast();
        let (cur, parent) = Self::context(cx, node);
        if cur != node {
            return Slot::Any;
        }
        let Some(p) = parent else { return Slot::Any };
        match *ast.node(p) {
            Node::If { .. } | Node::While { .. } | Node::DoWhile { .. } | Node::For { .. } => {
                Slot::Any
            }
            Node::Conditional { test, .. } => {
                if test == cur {
                    Slot::Min(4)
                } else {
                    Slot::Min(1)
                }
            }
            Node::Unary { .. } => Slot::Min(16),
            Node::Binary { op, left, .. } => Slot::Logical(op, left == cur),
            _ => Slot::Min(1),
        }
    }

    fn needs_parens(cx: &Ctx<'_, '_>, slot: Slot, expr: NodeId) -> bool {
        let ast = cx.ast();
        let prec = precedence(ast, expr);
        match slot {
            Slot::Any => false,
            Slot::Min(m) => prec < m,
            Slot::Logical(op, is_left) => {
                if let Node::Binary { op: inner, .. } = *ast.node(expr) {
                    let mixed = (op == BinaryOp::Coalesce) != (inner == BinaryOp::Coalesce)
                        && matches!(
                            inner,
                            BinaryOp::LogicalOr | BinaryOp::LogicalAnd | BinaryOp::Coalesce
                        )
                        && matches!(
                            op,
                            BinaryOp::LogicalOr | BinaryOp::LogicalAnd | BinaryOp::Coalesce
                        );
                    if mixed {
                        return true;
                    }
                }
                let own = if op == BinaryOp::LogicalAnd { 5 } else { 4 };
                if is_left { prec < own } else { prec <= own }
            }
        }
    }

    /// Replaces `node` with the text of `expr`, parenthesized if needed.
    fn replacement(cx: &Ctx<'_, '_>, node: NodeId, expr: NodeId) -> Option<Fix> {
        let span = cx.span(node);
        if !cx.ast().comments_in(span).is_empty() {
            return None;
        }
        let expr = skip_parens(cx.ast(), expr);
        let text = cx.text(expr);
        let text = if Self::needs_parens(cx, Self::slot(cx, node), expr) {
            format!("({text})")
        } else {
            space_before(cx.src(), span.start, text.to_owned())
        };
        Some(Fix::replace(span, text))
    }
}

impl Rule for NoExtraBooleanCast {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Unary, NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        match *cx.node(node) {
            Node::Unary {
                op: UnaryOp::Not,
                arg,
            } => {
                let inner = skip_parens(ast, arg);
                let Node::Unary {
                    op: UnaryOp::Not,
                    arg: inner_arg,
                } = *ast.node(inner)
                else {
                    return;
                };
                if !self.in_boolean_context(cx, node, 0) {
                    return;
                }
                let fix = Self::replacement(cx, node, inner_arg);
                cx.report_node(node, "Redundant double negation.")
                    .id("unexpectedNegation")
                    .fix_opt(fix);
            }
            Node::Call { args, .. } => {
                if !Self::is_boolean_call(cx, node) || !self.in_boolean_context(cx, node, 0) {
                    return;
                }
                let args = ast.list(args);
                let fix = match args {
                    [] => {
                        let (cur, parent) = Self::context(cx, node);
                        let _ = cur;
                        match parent.map(|p| (p, ast.node(p))) {
                            Some((
                                p,
                                Node::Unary {
                                    op: UnaryOp::Not, ..
                                },
                            )) => {
                                let span = cx.span(p);
                                if ast.comments_in(span).is_empty() {
                                    Some(Fix::replace(
                                        span,
                                        space_before(cx.src(), span.start, "true".into()),
                                    ))
                                } else {
                                    None
                                }
                            }
                            _ => {
                                let span = cx.span(node);
                                if ast.comments_in(span).is_empty() {
                                    Some(Fix::replace(
                                        span,
                                        space_before(cx.src(), span.start, "false".into()),
                                    ))
                                } else {
                                    None
                                }
                            }
                        }
                    }
                    [a] if !matches!(ast.node(*a), Node::Spread { .. }) => {
                        Self::replacement(cx, node, *a)
                    }
                    _ => None,
                };
                cx.report_node(node, "Redundant Boolean call.")
                    .id("unexpectedCall")
                    .fix_opt(fix);
            }
            _ => {}
        }
    }
}
