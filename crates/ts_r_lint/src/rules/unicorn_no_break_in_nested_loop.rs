//! `unicorn/no-break-in-nested-loop`: an unlabeled `break` (and, with `checkContinue`, an
//! unlabeled `continue`) whose target loop or `switch` sits inside another loop of the same
//! function, or whose target loop encloses another loop or `switch` around it. A `continue`
//! whose nearest enclosing loop-or-switch is a `switch` gets its own message (it continues
//! the loop, not the next case).

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-break-in-nested-loop",
        "Disallow `break` and optionally `continue` in nested loops and switches inside loops.",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<NoBreakInNestedLoop>,
};

#[derive(Debug, Default)]
struct NoBreakInNestedLoop {
    check_continue: bool,
}

impl FromOptions for NoBreakInNestedLoop {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(NoBreakInNestedLoop {
            check_continue: bool_field(obj_opt(opts, 0), "checkContinue", false),
        })
    }
}

fn is_loop(k: NodeKind) -> bool {
    matches!(
        k,
        NodeKind::For | NodeKind::ForIn | NodeKind::ForOf | NodeKind::While | NodeKind::DoWhile
    )
}

impl Rule for NoBreakInNestedLoop {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Break, NodeKind::Continue]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let (is_break, label) = match *cx.node(node) {
            Node::Break { label } => (true, label),
            Node::Continue { label } => (false, label),
            _ => return,
        };
        if label.is_some() || (!is_break && !self.check_continue) {
            return;
        }
        let mut inner_control_flow = false;
        let mut target = false;
        let mut nearest_is_switch = None;
        let mut nested = false;
        let mut cur = node;
        for _ in 0..100_000 {
            let Some(p) = cx.parent(cur) else {
                break;
            };
            cur = p;
            let k = ast.kind(p);
            if is_function(k) {
                break;
            }
            let looping = is_loop(k);
            let control = looping || k == NodeKind::Switch;
            if control && nearest_is_switch.is_none() {
                nearest_is_switch = Some(k == NodeKind::Switch);
            }
            if target {
                if looping {
                    nested = true;
                    break;
                }
                continue;
            }
            if if is_break { control } else { looping } {
                if inner_control_flow && looping {
                    nested = true;
                    break;
                }
                target = true;
                continue;
            }
            if control {
                inner_control_flow = true;
            }
        }
        if !nested {
            return;
        }
        let span = cx.span(node);
        if !is_break && nearest_is_switch == Some(true) {
            cx.report(
                span,
                "An unlabeled `continue` inside a `switch` continues the surrounding loop, not the next `case`. Use a labeled `continue` if that is intentional.",
            )
            .id("switch-continue");
            return;
        }
        let keyword = if is_break { "break" } else { "continue" };
        cx.report(
            span,
            format!(
                "Move this nested loop or switch into a function instead of using `{keyword}` here."
            ),
        )
        .id("no-break-in-nested-loop");
    }
}
