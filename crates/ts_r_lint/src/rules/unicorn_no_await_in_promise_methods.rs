//! `unicorn/no-await-in-promise-methods`: an `await` inside an element of the array passed
//! to `Promise.all/allSettled/any/race([...])` delays the call; it is reported on the
//! `await`. The walk from the `await` up to the array stops at a function boundary or an
//! outer `await` (the outer one is reported instead). When the `await` is the element
//! itself (through parentheses), a suggestion removes the keyword and the whitespace after
//! it.

use super::prelude::*;
use super::unicorn_ast::{promise_method_call, skip_spaces};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-await-in-promise-methods",
        "Disallow using `await` in `Promise` method parameters.",
        NO_PRESET,
        Category::Suspicious,
    )
    .suggestions(),
    create: create_default::<NoAwaitInPromiseMethods>,
};

#[derive(Debug, Default)]
struct NoAwaitInPromiseMethods;

const METHODS: &[&str] = &["all", "allSettled", "any", "race"];

impl Rule for NoAwaitInPromiseMethods {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Await]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let parents = cx.file.parents;
        let mut cur = node;
        let (element, method) = loop {
            let Some(p) = parents.parent(cur) else {
                return;
            };
            let k = ast.kind(p);
            if is_function(k) || k == NodeKind::Await {
                return;
            }
            if k == NodeKind::ArrayLit
                && let Some(call) = parent_skip_parens(ast, parents, p)
                && let Some(c) = promise_method_call(ast, call, METHODS)
                && skip_parens(ast, c.args[0]) == p
            {
                break (cur, c.name);
            }
            cur = p;
        };
        let msg = format!(
            "Avoid `await` inside `Promise.{method}()` arguments to prevent delaying the call."
        );
        let own = skip_parens(ast, element) == node;
        let start = cx.span(node).start;
        let end = skip_spaces(cx.src(), start as usize + "await".len());
        let r = cx
            .report_node(node, msg)
            .id("no-await-in-promise-methods/error");
        if own {
            r.suggest(
                "no-await-in-promise-methods/suggestion",
                "Remove `await`.",
                Fix::delete(Span::new(start, end as u32)),
            );
        }
    }
}
