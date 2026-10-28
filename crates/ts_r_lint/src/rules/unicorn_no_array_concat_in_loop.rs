//! `unicorn/no-array-concat-in-loop`: `acc = acc.concat(x)` (plain `=`, a plain non-optional
//! `.concat` call with at least one argument, TS wrappers looked through) inside a loop of
//! the same function, where `acc` is a single non-global `let`/`var` initialized to `[]`
//! and declared outside that loop's body. Reported at `concat`.

use super::prelude::*;
use super::unicorn_ast::method_call;
use super::unicorn_static::skip_ts_only;
use crate::semantic::ScopeKind;
use ts_r_ast::VarKind;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-array-concat-in-loop",
        "Disallow array accumulation with `Array#concat()` in loops.",
        NO_PRESET,
        Category::Pedantic,
    ),
    create: create_default::<NoArrayConcatInLoop>,
};

#[derive(Debug, Default)]
struct NoArrayConcatInLoop;

fn symbol_of(cx: &Ctx<'_, '_>, ident: NodeId) -> Option<SymbolId> {
    let sem = cx.semantic();
    match sem
        .reference_at(ident)
        .map(|r| sem.reference(r).resolution)?
    {
        Resolution::Symbol(s) => Some(s),
        _ => None,
    }
}

impl Rule for NoArrayConcatInLoop {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Assign]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Assign {
            op: AssignOp::Assign,
            left,
            right,
        } = *cx.node(node)
        else {
            return;
        };
        let left = skip_parens(ast, left);
        if !matches!(ast.node(left), Node::Ident { .. }) {
            return;
        }
        let call_id = skip_ts_only(ast, right);
        let Some(call) = method_call(ast, call_id) else {
            return;
        };
        if call.name != "concat" || !call.plain() || call.args.is_empty() {
            return;
        }
        let receiver = skip_ts_only(ast, call.obj);
        if !matches!(ast.node(receiver), Node::Ident { .. }) {
            return;
        }
        let (Some(sym), Some(rsym)) = (symbol_of(cx, left), symbol_of(cx, receiver)) else {
            return;
        };
        if sym != rsym {
            return;
        }
        let sem = cx.semantic();
        let symbol = sem.symbol(sym);
        let [decl] = symbol.decls.as_slice() else {
            return;
        };
        if sem.scope(symbol.scope).kind == ScopeKind::Global {
            return;
        }
        let Node::VarDecl { kind, .. } = *ast.node(decl.parent) else {
            return;
        };
        if !matches!(kind, VarKind::Let | VarKind::Var) {
            return;
        }
        let Node::VarDeclarator {
            name,
            init: Some(init),
            ..
        } = *ast.node(decl.node)
        else {
            return;
        };
        if name != decl.name
            || !matches!(*ast.node(skip_ts_only(ast, init)), Node::ArrayLit { elements } if ast.list(elements).is_empty())
        {
            return;
        }
        let mut cur = node;
        let body = loop {
            let Some(p) = cx.parent(cur) else {
                return;
            };
            match *ast.node(p) {
                Node::FunctionDecl { .. } | Node::FunctionExpr { .. } | Node::Arrow { .. } => {
                    return;
                }
                Node::For { body, .. }
                | Node::ForIn { body, .. }
                | Node::ForOf { body, .. }
                | Node::While { body, .. }
                | Node::DoWhile { body, .. } => break body,
                _ => {}
            }
            cur = p;
        };
        let (d, b) = (cx.span(decl.node), cx.span(body));
        if d.start >= b.start && d.end <= b.end {
            return;
        }
        cx.report(
            cx.span(call.prop),
            "Do not use `Array#concat()` to accumulate an array in a loop.",
        )
        .id("no-array-concat-in-loop");
    }
}
