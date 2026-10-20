//! `no-sparse-arrays`: disallow sparse arrays.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-sparse-arrays",
        "Disallow sparse arrays",
        ESLINT_REC,
        Category::Correctness,
    ),
    create: create_default::<NoSparseArrays>,
};

#[derive(Debug, Default)]
struct NoSparseArrays;

impl Rule for NoSparseArrays {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::ArrayLit]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::ArrayLit { elements } = *cx.node(node) else {
            return;
        };
        if is_assignment_target(ast, cx.file.parents, node) {
            return;
        }
        let els = ast.list(elements);
        // Each hole ends at the comma that follows the previous element (or `[`).
        let mut prev_end = cx.span(node).start + 1;
        for &e in els {
            if matches!(ast.node(e), Node::Hole {}) {
                if let Some(c) = find_char(cx.src(), prev_end, b',') {
                    cx.report(Span::new(c, c + 1), "Unexpected comma in middle of array.")
                        .id("unexpectedSparseArray");
                    prev_end = c + 1;
                }
            } else {
                prev_end = ast.span(e).end;
                // Skip the comma after this element.
                if let Some(c) = find_char(cx.src(), prev_end, b',') {
                    prev_end = c + 1;
                }
            }
        }
    }
}
