//! `max-statements`: at most `max` (10) statements in a function's blocks.

use super::func_names::own_name_with_kind;
use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "max-statements",
        "Enforce a maximum number of statements allowed in function blocks",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<MaxStatements>,
};

#[derive(Debug, Default)]
struct MaxStatements {
    max: u64,
    ignore_top_level: bool,
}

impl FromOptions for MaxStatements {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        let max = match opts.first() {
            Some(Value::Number(n)) => n.as_u64(),
            _ => o
                .get("maximum")
                .and_then(Value::as_u64)
                .filter(|&m| m > 0)
                .or_else(|| o.get("max").and_then(Value::as_u64)),
        }
        .unwrap_or(10);
        Ok(MaxStatements {
            max,
            ignore_top_level: bool_field(obj_opt(opts, 1), "ignoreTopLevelFunctions", false),
        })
    }
}

fn is_function(n: &Node) -> bool {
    matches!(
        n,
        Node::FunctionDecl { body: Some(_), .. } | Node::FunctionExpr { .. } | Node::Arrow { .. }
    )
}

impl Rule for MaxStatements {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        // (function, statement count, top level), in source order.
        let mut found: Vec<(NodeId, u64, bool)> = Vec::new();
        // Walk with the index of the enclosing function frame (`usize::MAX` for none, and
        // for static blocks, whose statements count for no reported function).
        let mut frames: Vec<u64> = Vec::new();
        let mut stack: Vec<(NodeId, Option<usize>, bool)> = vec![(ast.root, None, true)];
        let mut owners: Vec<(NodeId, usize, bool)> = Vec::new();
        while let Some((n, frame, top)) = stack.pop() {
            let node = ast.node(n);
            let (frame, top_child) = if is_function(node) {
                frames.push(0);
                owners.push((n, frames.len() - 1, top));
                (Some(frames.len() - 1), false)
            } else if matches!(node, Node::StaticBlock { .. }) {
                frames.push(0);
                (Some(frames.len() - 1), false)
            } else {
                (frame, top)
            };
            if let Node::Block { stmts } = *node
                && let Some(f) = frame
            {
                frames[f] += ast.list(stmts).len() as u64;
            }
            ast.for_each_child(n, |c| stack.push((c, frame, top_child)));
        }
        for (n, f, top) in owners {
            found.push((n, frames[f], top));
        }
        found.sort_by_key(|&(n, _, _)| cx.span(n).start);
        let tops = found.iter().filter(|f| f.2).count();
        for (n, count, top) in found {
            if self.ignore_top_level && top && tops == 1 {
                continue;
            }
            if count <= self.max {
                continue;
            }
            let name = own_name_with_kind(cx, n);
            let mut c = name.chars();
            let name: String = match c.next() {
                Some(f) => f.to_uppercase().chain(c).collect(),
                None => name,
            };
            let head = function_head_span(ast, cx.src(), cx.file.parents, n);
            cx.report(
                head,
                format!(
                    "{name} has too many statements ({count}). Maximum allowed is {}.",
                    self.max
                ),
            )
            .id("exceed");
        }
    }
}
