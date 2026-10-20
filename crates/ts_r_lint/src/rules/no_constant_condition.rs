//! `no-constant-condition`: disallow constant expressions in conditions.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-constant-condition",
        "Disallow constant expressions in conditions",
        ESLINT_REC,
        Category::Correctness,
    ),
    create: create::<NoConstantCondition>,
};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Loops {
    All,
    #[default]
    AllExceptWhileTrue,
    None,
}

#[derive(Debug, Default)]
struct NoConstantCondition {
    loops: Loops,
}

impl FromOptions for NoConstantCondition {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let loops = match obj_opt(opts, 0).get("checkLoops") {
            Some(Value::Bool(true)) => Loops::All,
            Some(Value::Bool(false)) => Loops::None,
            Some(Value::String(s)) => match s.as_str() {
                "all" => Loops::All,
                "none" => Loops::None,
                _ => Loops::AllExceptWhileTrue,
            },
            _ => Loops::AllExceptWhileTrue,
        };
        Ok(NoConstantCondition { loops })
    }
}

fn is_literal(ast: &ts_r_ast::Ast, id: NodeId) -> bool {
    matches!(
        ast.node(id),
        Node::NumberLit { .. }
            | Node::StringLit { .. }
            | Node::BoolLit { .. }
            | Node::NullLit {}
            | Node::RegexLit { .. }
            | Node::BigIntLit { .. }
    )
}

/// Truthiness of a literal (`None` for non-literals).
fn literal_truthy(cx: &Ctx<'_, '_>, id: NodeId) -> Option<bool> {
    let ast = cx.ast();
    Some(match *ast.node(id) {
        Node::NumberLit { value } => {
            let v = value.get();
            v != 0.0 && !v.is_nan()
        }
        Node::StringLit { value } => !ast.atom(value).is_empty(),
        Node::BoolLit { value } => value,
        Node::NullLit {} => false,
        Node::RegexLit { .. } => true,
        Node::BigIntLit { .. } => {
            let t = cx.text(id).trim_end_matches('n');
            parse_bigint(t).is_none_or(|s| s != "0")
        }
        _ => return None,
    })
}

/// True when `id` always short-circuits the logical operator `op`.
fn is_logical_identity(cx: &Ctx<'_, '_>, id: NodeId, op: BinaryOp) -> bool {
    let ast = cx.ast();
    let id = skip_parens(ast, id);
    match *ast.node(id) {
        Node::Unary {
            op: UnaryOp::Void, ..
        } => op == BinaryOp::LogicalAnd,
        Node::Binary {
            left,
            op: inner,
            right,
        } if inner == op => is_logical_identity(cx, left, op) || is_logical_identity(cx, right, op),
        Node::Assign { op: aop, right, .. } => {
            let matches = matches!(
                (aop, op),
                (AssignOp::OrAssign, BinaryOp::LogicalOr)
                    | (AssignOp::AndAssign, BinaryOp::LogicalAnd)
            );
            matches && is_logical_identity(cx, right, op)
        }
        _ => match literal_truthy(cx, id) {
            Some(t) => (op == BinaryOp::LogicalOr && t) || (op == BinaryOp::LogicalAnd && !t),
            None => false,
        },
    }
}

/// True when `id` always evaluates to the same value (or, in a boolean position, the same
/// truthiness).
pub(crate) fn is_constant(cx: &Ctx<'_, '_>, id: NodeId, in_bool: bool) -> bool {
    let ast = cx.ast();
    let id = skip_parens(ast, id);
    match *ast.node(id) {
        _ if is_literal(ast, id) => true,
        Node::ObjectLit { .. }
        | Node::Arrow { .. }
        | Node::FunctionExpr { .. }
        | Node::ClassExpr { .. } => true,
        Node::ArrayLit { elements } => {
            in_bool
                || ast.list(elements).iter().all(|&e| match *ast.node(e) {
                    Node::Hole {} => true,
                    Node::Spread { arg } => is_constant(cx, arg, false),
                    _ => is_constant(cx, e, false),
                })
        }
        Node::TemplateLit { parts } => {
            let parts = ast.list(parts);
            if in_bool
                && parts.iter().any(|&p| match *ast.node(p) {
                    Node::TemplateElement { cooked, .. } => !ast.atom(cooked).is_empty(),
                    _ => false,
                })
            {
                return true;
            }
            parts.iter().all(|&p| {
                matches!(ast.node(p), Node::TemplateElement { .. }) || is_constant(cx, p, false)
            })
        }
        Node::Unary { op, arg } => match op {
            UnaryOp::Void => true,
            UnaryOp::TypeOf => in_bool || is_constant(cx, arg, true),
            UnaryOp::Not => is_constant(cx, arg, true),
            UnaryOp::Delete => false,
            _ => is_constant(cx, arg, false),
        },
        Node::Binary { left, op, right } => match op {
            BinaryOp::LogicalOr | BinaryOp::LogicalAnd | BinaryOp::Coalesce => {
                let l = is_constant(cx, left, in_bool);
                let r = is_constant(cx, right, in_bool);
                let l_short = op != BinaryOp::Coalesce && l && is_logical_identity(cx, left, op);
                let r_short =
                    op != BinaryOp::Coalesce && in_bool && r && is_logical_identity(cx, right, op);
                (l && r) || l_short || r_short
            }
            BinaryOp::In => false,
            _ => is_constant(cx, left, false) && is_constant(cx, right, false),
        },
        Node::Assign { op, right, .. } => match op {
            AssignOp::Assign => is_constant(cx, right, in_bool),
            AssignOp::OrAssign => in_bool && is_logical_identity(cx, right, BinaryOp::LogicalOr),
            AssignOp::AndAssign => in_bool && is_logical_identity(cx, right, BinaryOp::LogicalAnd),
            _ => false,
        },
        Node::Seq { exprs } => ast
            .list(exprs)
            .last()
            .is_some_and(|&l| is_constant(cx, l, in_bool)),
        Node::Ident { .. } => cx.is_global_ref(id, "undefined"),
        Node::Call { callee, args, .. } => {
            if !cx.is_global_ref(skip_parens(ast, callee), "Boolean") {
                return false;
            }
            match ast.list(args).first() {
                None => true,
                Some(&a) => !matches!(ast.node(a), Node::Spread { .. }) && is_constant(cx, a, true),
            }
        }
        Node::New { .. } => in_bool,
        _ => false,
    }
}

fn is_loop(k: NodeKind) -> bool {
    matches!(
        k,
        NodeKind::For | NodeKind::While | NodeKind::DoWhile | NodeKind::ForIn | NodeKind::ForOf
    )
}

impl NoConstantCondition {
    /// True when a `yield` inside the loop (not its `for` initializer, not nested functions)
    /// can exit it.
    fn loop_has_yield(cx: &Ctx<'_, '_>, lp: NodeId) -> bool {
        let ast = cx.ast();
        let mut stack: Vec<NodeId> = Vec::new();
        match *ast.node(lp) {
            Node::For {
                test, update, body, ..
            } => {
                stack.extend(test);
                stack.extend(update);
                stack.push(body);
            }
            _ => ast.for_each_child(lp, |c| stack.push(c)),
        }
        while let Some(n) = stack.pop() {
            let k = ast.kind(n);
            if is_function(k) {
                continue;
            }
            if k == NodeKind::Yield {
                return true;
            }
            ast.for_each_child(n, |c| stack.push(c));
        }
        false
    }

    fn in_generator(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
        let ast = cx.ast();
        for a in ancestors_inclusive(cx.file.parents, node) {
            match *ast.node(a) {
                Node::FunctionDecl { flags, .. } | Node::FunctionExpr { flags, .. } => {
                    return flags.contains(FnFlags::GENERATOR);
                }
                Node::Arrow { .. } => return false,
                _ => {}
            }
        }
        false
    }
}

impl Rule for NoConstantCondition {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::If,
            NodeKind::Conditional,
            NodeKind::While,
            NodeKind::DoWhile,
            NodeKind::For,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let test = match *cx.node(node) {
            Node::If { test, .. } | Node::Conditional { test, .. } => test,
            Node::While { test, .. } | Node::DoWhile { test, .. } => test,
            Node::For { test: Some(t), .. } => t,
            _ => return,
        };
        let k = ast.kind(node);
        if is_loop(k) {
            match self.loops {
                Loops::None => return,
                Loops::AllExceptWhileTrue => {
                    if k == NodeKind::While
                        && matches!(
                            ast.node(skip_parens(ast, test)),
                            Node::BoolLit { value: true }
                        )
                    {
                        return;
                    }
                }
                Loops::All => {}
            }
            if Self::in_generator(cx, node) && Self::loop_has_yield(cx, node) {
                return;
            }
        }
        if is_constant(cx, test, true) {
            cx.report_node(skip_parens(ast, test), "Unexpected constant condition.")
                .id("unexpected");
        }
    }
}
