//! `no-useless-return`: a bare `return;` after which, were it removed, nothing else would
//! run before the function ends.

use super::prelude::*;
use crate::cfg::{Cfg, Options};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-useless-return",
        "Disallow redundant return statements",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<NoUselessReturn>,
};

#[derive(Debug, Default)]
struct NoUselessReturn;

const LIMIT: usize = 100_000;

fn is_function(n: &Node) -> bool {
    matches!(
        n,
        Node::FunctionDecl { .. }
            | Node::FunctionExpr { .. }
            | Node::Arrow { .. }
            | Node::StaticBlock { .. }
    )
}

/// What running `s` from its start does first.
enum Effect {
    /// Runs a statement ESLint counts (anything but blocks, function declarations, types
    /// and bare returns).
    Runs,
    /// Reaches a bare `return;`.
    Returns,
    /// Leaves through `break` to (after) this statement.
    Breaks(NodeId),
    /// Nothing.
    Nothing,
}

fn break_target(cx: &Ctx<'_, '_>, brk: NodeId, label: Option<NodeId>) -> Option<NodeId> {
    let ast = cx.ast();
    let want = label.and_then(|l| ident_name(ast, l));
    let mut cur = brk;
    for _ in 0..LIMIT {
        let p = cx.parent(cur)?;
        match *ast.node(p) {
            Node::Labeled { label: l, .. } if want.is_some() && ident_name(ast, l) == want => {
                return Some(p);
            }
            Node::Switch { .. } if want.is_none() => return Some(p),
            ref n if is_function(n) => return None,
            _ => {}
        }
        cur = p;
    }
    None
}

fn effect(cx: &Ctx<'_, '_>, s: NodeId) -> Effect {
    let ast = cx.ast();
    match *ast.node(s) {
        Node::Block { stmts } => effects(cx, ast.list(stmts)),
        Node::FunctionDecl { .. } => Effect::Nothing,
        Node::Return { arg: None } => Effect::Returns,
        Node::Break { label } => match break_target(cx, s, label) {
            Some(t) => Effect::Breaks(t),
            None => Effect::Nothing,
        },
        ref n
            if crate::cfg::is_type_kind(ast.kind(s))
                || matches!(n, Node::TypeAlias { .. } | Node::Interface { .. }) =>
        {
            Effect::Nothing
        }
        _ => Effect::Runs,
    }
}

fn effects(cx: &Ctx<'_, '_>, stmts: &[NodeId]) -> Effect {
    for &s in stmts {
        match effect(cx, s) {
            Effect::Nothing => {}
            e => return e,
        }
    }
    Effect::Nothing
}

/// Something ESLint counts would run after `ret` if it fell through.
fn followed(cx: &Ctx<'_, '_>, ret: NodeId) -> bool {
    let ast = cx.ast();
    let mut cur = ret;
    'up: for _ in 0..LIMIT {
        let Some(p) = cx.parent(cur) else {
            return false;
        };
        if is_function(ast.node(p)) {
            return false;
        }
        let mut rest: Vec<&[NodeId]> = Vec::new();
        match *ast.node(p) {
            Node::Block { stmts } | Node::Program { stmts } | Node::ModuleBlock { stmts } => {
                let list = ast.list(stmts);
                if let Some(i) = list.iter().position(|&s| s == cur) {
                    rest.push(&list[i + 1..]);
                }
            }
            Node::SwitchCase { body, .. } => {
                let list = ast.list(body);
                if let Some(i) = list.iter().position(|&s| s == cur) {
                    rest.push(&list[i + 1..]);
                }
                // Fall through into the following cases.
                if let Some(sw) = cx.parent(p)
                    && let Node::Switch { cases, .. } = *ast.node(sw)
                {
                    let cases = ast.list(cases);
                    if let Some(i) = cases.iter().position(|&c| c == p) {
                        for &c in &cases[i + 1..] {
                            if let Node::SwitchCase { body, .. } = *ast.node(c) {
                                rest.push(ast.list(body));
                            }
                        }
                    }
                    cur = sw;
                }
            }
            Node::Try {
                finalizer, handler, ..
            } => {
                // Statements in `catch` and `finally` don't count for the `try` block.
                if handler == Some(cur)
                    && let Some(f) = finalizer
                    && let Node::Block { stmts } = *ast.node(f)
                {
                    rest.push(ast.list(stmts));
                }
            }
            _ => {}
        }
        for list in rest {
            match effects(cx, list) {
                Effect::Runs => return true,
                Effect::Returns => return false,
                Effect::Breaks(t) => {
                    cur = t;
                    continue 'up;
                }
                Effect::Nothing => {}
            }
        }
        if !matches!(ast.node(p), Node::SwitchCase { .. }) {
            cur = p;
        }
    }
    false
}

/// `ret` is inside a loop or a `finally` of its function.
fn in_loop_or_finally(cx: &Ctx<'_, '_>, ret: NodeId) -> bool {
    let ast = cx.ast();
    let mut cur = ret;
    while let Some(p) = cx.parent(cur) {
        match *ast.node(p) {
            ref n if is_function(n) => return false,
            Node::For { .. }
            | Node::ForIn { .. }
            | Node::ForOf { .. }
            | Node::While { .. }
            | Node::DoWhile { .. } => return true,
            Node::Try {
                finalizer: Some(f), ..
            } if f == cur => return true,
            _ => {}
        }
        cur = p;
    }
    false
}

impl Rule for NoUselessReturn {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::Program,
            NodeKind::FunctionDecl,
            NodeKind::FunctionExpr,
            NodeKind::Arrow,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        // The function's own bare returns.
        let mut rets = Vec::new();
        let mut stack = Vec::new();
        ast.for_each_child(node, |c| stack.push(c));
        while let Some(n) = stack.pop() {
            match *ast.node(n) {
                ref x
                    if is_function(x)
                        || matches!(x, Node::ClassDecl { .. } | Node::ClassExpr { .. }) =>
                {
                    continue;
                }
                Node::Return { arg: None } => rets.push(n),
                _ => {}
            }
            ast.for_each_child(n, |c| stack.push(c));
        }
        rets.retain(|&r| !in_loop_or_finally(cx, r) && !followed(cx, r));
        if rets.is_empty() {
            return;
        }
        // ESLint's code paths: a `catch` is reachable from the end of its `try` block.
        let opts = Options {
            try_end_to_catch: true,
            ..Options::default()
        };
        let cfg = match *ast.node(node) {
            Node::Program { stmts } => Cfg::statements_with(ast, ast.list(stmts), &opts),
            _ => Cfg::function_with(ast, node, &opts),
        };
        let reach = cfg.event_reachability();
        rets.sort_by_key(|&r| cx.span(r).start);
        let fspan = cx.span(node);
        for r in rets {
            if !reach.get(&r).copied().unwrap_or(false) {
                continue;
            }
            let span = cx.span(r);
            let removable = cx
                .parent(r)
                .is_some_and(|p| statement_list(ast, p).is_some());
            let fix = (removable && ast.comments_in(span).is_empty()).then(|| {
                Fix::delete(span)
                    .and(Span::at(fspan.start), "")
                    .and(Span::at(fspan.end), "")
            });
            cx.report(span, "Unnecessary return statement.")
                .id("unnecessaryReturn")
                .fix_opt(fix);
        }
    }
}
