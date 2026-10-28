//! `unicorn/no-useless-recursion`: `return f(...)` inside the named function `f` itself (a
//! tail call of a plain, non-optional identifier that resolves to the function's own name)
//! is a loop in disguise. Generators are left alone, as are calls inside a `try` (between
//! the `return` and the function) and calls after a `using` / `await using` declaration
//! (an earlier statement of an enclosing block, or a `for` / `for...of` header), whose
//! cleanup runs after the call. The callee is reported.

use super::prelude::*;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-useless-recursion",
        "Disallow simple recursive function calls that can be replaced with a loop.",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<NoUselessRecursion>,
};

#[derive(Debug, Default)]
struct NoUselessRecursion;

fn is_using(ast: &Ast, id: Option<NodeId>) -> bool {
    id.is_some_and(|id| {
        matches!(
            ast.node(id),
            Node::VarDecl {
                kind: VarKind::Using | VarKind::AwaitUsing,
                ..
            }
        )
    })
}

impl Rule for NoUselessRecursion {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Return]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Return { arg: Some(arg) } = *cx.node(node) else {
            return;
        };
        let Node::Call {
            callee,
            optional: false,
            ..
        } = *ast.node(skip_parens(ast, arg))
        else {
            return;
        };
        let callee = skip_parens(ast, callee);
        let Some(name) = ident_name(ast, callee) else {
            return;
        };
        // The enclosing function, failing on the way at `try` and `using`.
        let mut cur = node;
        let func = loop {
            let Some(parent) = cx.parent(cur) else {
                return;
            };
            match *ast.node(parent) {
                Node::FunctionDecl { .. } | Node::FunctionExpr { .. } | Node::Arrow { .. } => {
                    break parent;
                }
                Node::Try { .. } => return,
                Node::For { init, .. } if is_using(ast, init) => return,
                Node::ForOf { left, .. } if is_using(ast, Some(left)) => return,
                Node::Block { stmts } => {
                    let stmts = ast.list(stmts);
                    let before = stmts.iter().take_while(|&&s| s != cur);
                    if before.copied().any(|s| is_using(ast, Some(s))) {
                        return;
                    }
                }
                _ => {}
            }
            cur = parent;
        };
        let (Node::FunctionDecl {
            flags,
            name: Some(fn_name),
            ..
        }
        | Node::FunctionExpr {
            flags,
            name: Some(fn_name),
            ..
        }) = *ast.node(func)
        else {
            return;
        };
        if flags.contains(FnFlags::GENERATOR) || ident_name(ast, fn_name) != Some(name) {
            return;
        }
        let sem = cx.semantic();
        let Some(r) = sem.reference_at(callee) else {
            return;
        };
        let Resolution::Symbol(s) = sem.reference(r).resolution else {
            return;
        };
        if !sem.symbol(s).decls.iter().any(|d| d.name == fn_name) {
            return;
        }
        cx.report_node(
            callee,
            "Use a loop instead of this recursive function call.",
        )
        .id("no-useless-recursion");
    }
}
