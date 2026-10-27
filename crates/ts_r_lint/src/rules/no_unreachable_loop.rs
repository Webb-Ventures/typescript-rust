//! `no-unreachable-loop`: loops whose body can never get to a second iteration.

use super::prelude::*;
use crate::cfg::{Cfg, Options};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-unreachable-loop",
        "Disallow loops with a body that allows only one iteration",
        NO_PRESET,
        Category::Correctness,
    ),
    create: create::<NoUnreachableLoop>,
};

#[derive(Debug, Default)]
struct NoUnreachableLoop {
    ignore: Vec<NodeKind>,
}

impl FromOptions for NoUnreachableLoop {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let ignore = obj_opt(opts, 0)
            .get("ignore")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|v| match v.as_str()? {
                "WhileStatement" => Some(NodeKind::While),
                "DoWhileStatement" => Some(NodeKind::DoWhile),
                "ForStatement" => Some(NodeKind::For),
                "ForInStatement" => Some(NodeKind::ForIn),
                "ForOfStatement" => Some(NodeKind::ForOf),
                _ => None,
            })
            .collect();
        Ok(NoUnreachableLoop { ignore })
    }
}

impl Rule for NoUnreachableLoop {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::Program,
            NodeKind::FunctionDecl,
            NodeKind::FunctionExpr,
            NodeKind::Arrow,
            NodeKind::StaticBlock,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        // This function's own loops (not those of nested functions).
        let mut loops = Vec::new();
        let mut stack = Vec::new();
        ast.for_each_child(node, |c| stack.push(c));
        while let Some(n) = stack.pop() {
            let k = ast.kind(n);
            match k {
                NodeKind::FunctionDecl
                | NodeKind::FunctionExpr
                | NodeKind::Arrow
                | NodeKind::StaticBlock => continue,
                NodeKind::While
                | NodeKind::DoWhile
                | NodeKind::For
                | NodeKind::ForIn
                | NodeKind::ForOf
                    if !self.ignore.contains(&k) =>
                {
                    loops.push(n)
                }
                _ => {}
            }
            ast.for_each_child(n, |c| stack.push(c));
        }
        if loops.is_empty() {
            return;
        }
        let opts = Options {
            try_end_to_catch: true,
            ..Options::default()
        };
        let cfg = match *ast.node(node) {
            Node::Program { stmts } => Cfg::statements_with(ast, ast.list(stmts), &opts),
            Node::StaticBlock { body } => Cfg::statements_with(ast, ast.list(body), &opts),
            _ => Cfg::function_with(ast, node, &opts),
        };
        let reach = cfg.event_reachability();
        // Loops with a reachable block of their body that goes back to the iteration step.
        let mut by_target: rustc_hash::FxHashMap<u32, Vec<usize>> = Default::default();
        for (i, l) in cfg.loops.iter().enumerate() {
            by_target.entry(l.target).or_default().push(i);
        }
        let mut iterates = rustc_hash::FxHashSet::default();
        for (b, block) in cfg.blocks.iter().enumerate() {
            let b = b as u32;
            if !cfg.is_reachable(b) {
                continue;
            }
            for t in &block.succ {
                for &i in by_target.get(t).into_iter().flatten() {
                    if cfg.loops[i].contains(b) {
                        iterates.insert(cfg.loops[i].node);
                    }
                }
            }
        }
        loops.sort_by_key(|&l| cx.span(l).start);
        for l in loops {
            if reach.get(&l).copied().unwrap_or(false) && !iterates.contains(&l) {
                cx.report_node(l, "Invalid loop. Its body allows only one iteration.")
                    .id("invalid");
            }
        }
    }
}
