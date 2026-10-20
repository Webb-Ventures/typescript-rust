//! `@typescript-eslint/no-confusing-void-expression`: a `void` value is used only as a
//! statement, not as a value of another expression.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-confusing-void-expression",
        "Require expressions of type void to appear in statement position",
        TS_STRICT_TC,
        Category::Suspicious,
    )
    .typed()
    .fixable()
    .suggestions(),
    create: create::<NoConfusingVoidExpression>,
};

#[derive(Debug)]
struct NoConfusingVoidExpression {
    ignore_arrow_shorthand: bool,
    ignore_void_operator: bool,
    ignore_void_returning_functions: bool,
}

impl FromOptions for NoConfusingVoidExpression {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(NoConfusingVoidExpression {
            ignore_arrow_shorthand: bool_field(o, "ignoreArrowShorthand", false),
            ignore_void_operator: bool_field(o, "ignoreVoidOperator", false),
            ignore_void_returning_functions: bool_field(o, "ignoreVoidReturningFunctions", false),
        })
    }
}

const EXPR: &str = "Placing a void expression inside another expression is forbidden. Move it to its own statement instead.";
const EXPR_WRAP: &str = "Void expressions used inside another expression must be moved to its own statement or marked explicitly with the `void` operator.";
const ARROW: &str = "Returning a void expression from an arrow function shorthand is forbidden. Please add braces to the arrow function.";
const ARROW_WRAP: &str = "Void expressions returned from an arrow function shorthand must be marked explicitly with the `void` operator.";
const RETURN: &str = "Returning a void expression from a function is forbidden. Please move it before the `return` statement.";
const RETURN_LAST: &str = "Returning a void expression from a function is forbidden. Please remove the `return` statement.";
const RETURN_WRAP: &str =
    "Void expressions returned from a function must be marked explicitly with the `void` operator.";

impl Rule for NoConfusingVoidExpression {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call, NodeKind::Await, NodeKind::TaggedTemplate]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ti = cx.types();
        let Some(t) = ti.type_at(node) else { return };
        if !is_void_type(ti, t) {
            return;
        }
        // The outermost expression whose value is this one's: through parentheses, the
        // right side of `&&` / `||` / `??` and the branches of `?:`.
        let mut cur = node;
        let mut depth = 0;
        let parent = loop {
            let Some(p) = cx.parent(cur) else { return };
            depth += 1;
            if depth > 256 {
                return;
            }
            match *cx.node(p) {
                Node::Paren { .. } => cur = p,
                Node::Binary {
                    op: BinaryOp::LogicalAnd | BinaryOp::LogicalOr | BinaryOp::Coalesce,
                    right,
                    ..
                } if right == cur => cur = p,
                Node::Conditional { cons, alt, .. } if cons == cur || alt == cur => cur = p,
                _ => break p,
            }
        };
        let ast = cx.ast();
        let src = cx.src();
        match *cx.node(parent) {
            Node::ExprStmt { .. } => {}
            Node::Unary {
                op: UnaryOp::Void, ..
            } if self.ignore_void_operator => {}
            Node::Arrow { body, .. } if body == cur => {
                if self.ignore_arrow_shorthand
                    || (self.ignore_void_returning_functions && returns_void(cx, parent))
                {
                    return;
                }
                let body_text = cx.span(cur).text(src).to_string();
                if self.ignore_void_operator {
                    let fix = Fix::replace(cx.span(cur), format!("void {body_text}"));
                    cx.report_node(node, ARROW_WRAP)
                        .id("invalidVoidExprArrowWrapVoid")
                        .fix(fix);
                } else {
                    let inner = cx.span(skip_parens(ast, cur)).text(src);
                    let fix = Fix::replace(cx.span(cur), format!("{{ {inner}; }}"));
                    cx.report_node(node, ARROW)
                        .id("invalidVoidExprArrow")
                        .fix(fix);
                }
            }
            Node::Return { arg: Some(arg) } if arg == cur => {
                let func = enclosing_function(cx, parent);
                if self.ignore_void_returning_functions && func.is_some_and(|f| returns_void(cx, f))
                {
                    return;
                }
                let arg_text = cx.span(cur).text(src).to_string();
                if self.ignore_void_operator {
                    let fix = Fix::replace(cx.span(cur), format!("void {arg_text}"));
                    cx.report_node(node, RETURN_WRAP)
                        .id("invalidVoidExprReturnWrapVoid")
                        .fix(fix);
                    return;
                }
                let inner = cx.span(skip_parens(ast, cur)).text(src);
                let semi = if inner.starts_with(['[', '(', '`', '+', '-', '/'])
                    && prev_statement(ast, cx.file.parents, parent)
                        .is_some_and(|p| stmt_continuable(ast, src, p))
                {
                    ";"
                } else {
                    ""
                };
                let last = func.is_some_and(|f| is_last_statement(cx, f, parent));
                if last {
                    let fix = Fix::replace(cx.span(parent), format!("{semi}{inner};"));
                    cx.report_node(node, RETURN_LAST)
                        .id("invalidVoidExprReturnLast")
                        .fix(fix);
                } else {
                    let fix = Fix::replace(cx.span(parent), format!("{semi}{inner}; return;"));
                    cx.report_node(node, RETURN)
                        .id("invalidVoidExprReturn")
                        .fix(fix);
                }
            }
            _ => {
                if self.ignore_void_operator {
                    let text = cx.span(node).text(src);
                    let fix = Fix::replace(cx.span(node), format!("void {text}"));
                    cx.report_node(node, EXPR_WRAP)
                        .id("invalidVoidExprWrapVoid")
                        .suggest(
                            "voidExprWrapVoid",
                            "Mark with an explicit `void` operator.",
                            fix,
                        );
                } else {
                    cx.report_node(node, EXPR).id("invalidVoidExpr");
                }
            }
        }
    }
}

/// `void` (possibly with `undefined` / `never`), never any or unresolved.
fn is_void_type(ti: &dyn TypeInfo, t: Ty) -> bool {
    if t == Ty::UNRESOLVED || has_unresolved_part(ti, t) || is_any(ti, t) {
        return false;
    }
    let ps = parts(ti, t);
    ps.iter().any(|&p| ti.flags(p).contains(TypeFlags::VOID))
        && ps.iter().all(|&p| {
            ti.flags(p)
                .intersects(TypeFlags::VOID | TypeFlags::UNDEFINED | TypeFlags::NEVER)
        })
}

fn enclosing_function(cx: &Ctx<'_, '_>, node: NodeId) -> Option<NodeId> {
    let mut cur = cx.parent(node);
    let mut depth = 0;
    while let Some(p) = cur {
        if matches!(
            cx.node(p),
            Node::FunctionDecl { .. } | Node::FunctionExpr { .. } | Node::Arrow { .. }
        ) {
            return Some(p);
        }
        depth += 1;
        if depth > 1024 {
            return None;
        }
        cur = cx.parent(p);
    }
    None
}

/// `ret` is the last statement of `func`'s body block.
fn is_last_statement(cx: &Ctx<'_, '_>, func: NodeId, ret: NodeId) -> bool {
    let body = match *cx.node(func) {
        Node::FunctionDecl { body: Some(b), .. }
        | Node::FunctionExpr { body: Some(b), .. }
        | Node::Arrow { body: b, .. } => b,
        _ => return false,
    };
    match *cx.node(body) {
        Node::Block { stmts } => cx.ast().list(stmts).last() == Some(&ret),
        _ => false,
    }
}

/// The function is declared or contextually typed to return `void`.
fn returns_void(cx: &Ctx<'_, '_>, func: NodeId) -> bool {
    let ti = cx.types();
    let (return_type, is_async) = match *cx.node(func) {
        Node::FunctionDecl {
            return_type, flags, ..
        }
        | Node::FunctionExpr {
            return_type, flags, ..
        }
        | Node::Arrow {
            return_type, flags, ..
        } => (return_type, flags.contains(FnFlags::ASYNC)),
        _ => return false,
    };
    let has_void = |t: Ty| {
        let t = if is_async { ti.awaited_type(t) } else { t };
        t != Ty::UNRESOLVED
            && !is_any(ti, t)
            && parts(ti, t)
                .iter()
                .any(|&p| ti.flags(p).contains(TypeFlags::VOID))
    };
    if let Some(rt) = return_type {
        return ti.type_of_type_node(rt).is_some_and(has_void);
    }
    // Overloads of a declared function.
    if let Node::FunctionDecl {
        name: Some(name), ..
    } = *cx.node(func)
        && let Some(s) = ti.symbol_at(name)
    {
        let ft = ti.type_of_symbol(s);
        return ti
            .signatures(ft, false)
            .into_iter()
            .any(|sig| has_void(ti.return_type(sig)));
    }
    let Some(ct) = ti.contextual_type(func) else {
        return false;
    };
    parts(ti, ct).into_iter().any(|p| {
        ti.signatures(p, false)
            .into_iter()
            .any(|sig| has_void(ti.return_type(sig)))
    })
}
