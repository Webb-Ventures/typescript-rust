//! `unicorn/no-unused-iterator-helper`: a lazy iterator helper call (`iter.map/filter/
//! flatMap/take/drop(..)` on a known iterator) whose result is discarded: through TS
//! wrappers, `await` and parentheses, it is an expression statement, a `void` operand, or a
//! `for` initializer or update. Reported at the method name.
//!
//! For `.map(cb)` with one non-spread argument, no type arguments and no TS wrapper between
//! the call and the discarded expression, a suggestion renames it to `.forEach`.

use super::prelude::*;
use super::unicorn_ast::method_call;
use super::unicorn_iter::is_lazy_iterator_helper_call;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-unused-iterator-helper",
        "Disallow discarding lazy iterator helpers.",
        NO_PRESET,
        Category::Style,
    )
    .suggestions(),
    create: create_default::<NoUnusedIteratorHelper>,
};

#[derive(Debug, Default)]
struct NoUnusedIteratorHelper;

fn is_ts_wrapper(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(id),
        Node::As { .. }
            | Node::Satisfies { .. }
            | Node::NonNull { .. }
            | Node::TypeAssertion { .. }
            | Node::Instantiation { .. }
    )
}

/// The discarded expression around `call` and whether a TS wrapper is on the way.
fn discarded(cx: &Ctx<'_, '_>, call: NodeId) -> Option<bool> {
    let ast = cx.ast();
    let mut cur = call;
    let mut wrapped = false;
    let parent = loop {
        let p = cx.parent(cur)?;
        match ast.node(p) {
            Node::Paren { .. } | Node::Await { .. } => {}
            _ if is_ts_wrapper(ast, p) => wrapped = true,
            _ => break p,
        }
        cur = p;
    };
    let is_discarded = match *ast.node(parent) {
        Node::ExprStmt { .. } => true,
        Node::Unary {
            op: UnaryOp::Void, ..
        } => true,
        Node::For { init, update, .. } => init == Some(cur) || update == Some(cur),
        _ => false,
    };
    is_discarded.then_some(wrapped)
}

impl Rule for NoUnusedIteratorHelper {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, id: NodeId, cx: &mut Ctx<'_, '_>) {
        let Some(wrapped) = discarded(cx, id) else {
            return;
        };
        if !is_lazy_iterator_helper_call(cx, id, &mut Vec::new()) {
            return;
        }
        let ast = cx.ast();
        let Some(call) = method_call(ast, id) else {
            return;
        };
        let suggest = call.name == "map"
            && call.args.len() == 1
            && !matches!(ast.node(call.args[0]), Node::Spread { .. })
            && !matches!(
                *ast.node(id),
                Node::Call {
                    type_args: Some(_),
                    ..
                }
            )
            && !wrapped;
        let span = cx.span(call.prop);
        let r = cx
            .report(
                span,
                format!(
                    "Consume the iterator returned by `.{}(…)`; iterator helpers are lazy.",
                    call.name
                ),
            )
            .id("no-unused-iterator-helper");
        if suggest {
            r.suggest(
                "no-unused-iterator-helper/suggestion",
                "Use `.forEach()` to run the callback.",
                Fix::replace(span, "forEach"),
            );
        }
    }
}
