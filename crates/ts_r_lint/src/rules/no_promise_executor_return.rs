//! `no-promise-executor-return`: disallow returning a value from a Promise executor (the
//! first argument of `new Promise(...)`, a function or arrow, with the global `Promise`).
//!
//! A `return` with a value and an arrow's expression body both report. With `allowVoid`,
//! a `void` expression is allowed and the reports suggest prepending `void`; an expression
//! body also suggests wrapping it in braces (not for an anonymous function or class, where
//! that would make a declaration).

use super::prelude::*;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-promise-executor-return",
        "Disallow returning values from Promise executor functions",
        NO_PRESET,
        Category::Suspicious,
    )
    .suggestions(),
    create: create::<NoPromiseExecutorReturn>,
};

const MESSAGE: &str = "Return values from promise executor functions cannot be read.";

#[derive(Debug, Default)]
struct NoPromiseExecutorReturn {
    allow_void: bool,
}

impl FromOptions for NoPromiseExecutorReturn {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let empty = serde_json::Map::new();
        let o = opts.first().and_then(Value::as_object).unwrap_or(&empty);
        Ok(NoPromiseExecutorReturn {
            allow_void: bool_field(o, "allowVoid", false),
        })
    }
}

fn is_void(ast: &Ast, id: NodeId) -> bool {
    matches!(
        *ast.node(skip_parens(ast, id)),
        Node::Unary {
            op: UnaryOp::Void,
            ..
        }
    )
}

impl NoPromiseExecutorReturn {
    /// Whether `f` is the executor of a `new Promise(...)` with the global `Promise`.
    fn is_executor(cx: &Ctx<'_, '_>, f: NodeId) -> bool {
        let ast = cx.ast();
        if !matches!(ast.node(f), Node::Arrow { .. } | Node::FunctionExpr { .. }) {
            return false;
        }
        let Some(new) = parent_skip_parens(ast, cx.file.parents, f) else {
            return false;
        };
        let Node::New { callee, args, .. } = *ast.node(new) else {
            return false;
        };
        ast.list(args)
            .first()
            .is_some_and(|&a| skip_parens(ast, a) == f)
            && matches!(ast.node(callee), Node::Ident { .. })
            && cx.is_global_ref(callee, "Promise")
    }

    /// `void ` before `first` (the first token of `expr` as written) and `)` after `expr`
    /// when the operand needs parentheses.
    fn prepend_void(cx: &Ctx<'_, '_>, first: u32, space: bool, expr: NodeId) -> Fix {
        let ast = cx.ast();
        let inner = skip_parens(ast, expr);
        let parens = precedence(ast, inner) < 16 && inner == expr;
        let head = format!(
            "{}void {}",
            if space { " " } else { "" },
            if parens { "(" } else { "" }
        );
        Fix::insert(first, head).and(Span::at(cx.span(inner).end), if parens { ")" } else { "" })
    }
}

impl Rule for NoPromiseExecutorReturn {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Arrow, NodeKind::Return]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        match *ast.node(node) {
            Node::Arrow { flags, body, .. } => {
                if !flags.contains(FnFlags::EXPRESSION_BODY) || !Self::is_executor(cx, node) {
                    return;
                }
                if self.allow_void && is_void(ast, body) {
                    return;
                }
                let inner = skip_parens(ast, body);
                let body_start = ast.span(body).start;
                let mut suggestions = Vec::new();
                if self.allow_void {
                    suggestions.push((
                        "prependVoid",
                        "Prepend `void` to the expression.",
                        Self::prepend_void(cx, body_start, false, body),
                    ));
                }
                let anonymous = match *ast.node(inner) {
                    Node::FunctionExpr { name, .. } => name.is_none(),
                    Node::ClassExpr { name, .. } => name.is_none(),
                    _ => false,
                };
                if !anonymous {
                    let end = cx.span(node).end;
                    suggestions.push((
                        "wrapBraces",
                        "Wrap the expression in `{}`.",
                        Fix::insert(body_start, "{").and(Span::at(end), "}"),
                    ));
                }
                let mut r = cx.report(cx.span(inner), MESSAGE).id("returnsValue");
                for (id, desc, fix) in suggestions {
                    r = r.suggest(id, desc, fix);
                }
            }
            Node::Return { arg: Some(arg) } => {
                let parents = cx.file.parents;
                let Some(f) = parents
                    .ancestors(node)
                    .find(|&a| is_function(ast.node(a).kind()))
                else {
                    return;
                };
                if !Self::is_executor(cx, f) {
                    return;
                }
                if !self.allow_void {
                    cx.report(cx.span(node), MESSAGE).id("returnsValue");
                    return;
                }
                if is_void(ast, arg) {
                    return;
                }
                let kw_end = cx.span(node).start + "return".len() as u32;
                let first = ast.span(arg).start;
                let fix = Self::prepend_void(cx, first, first == kw_end, arg);
                cx.report(cx.span(node), MESSAGE)
                    .id("returnsValue")
                    .suggest("prependVoid", "Prepend `void` to the expression.", fix);
            }
            _ => {}
        }
    }
}
