//! `no-unmodified-loop-condition`: a variable in a `while` / `do-while` / `for` test that the
//! loop never writes (directly, or through a function declaration called in the loop).

use super::prelude::*;
use rustc_hash::FxHashMap;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-unmodified-loop-condition",
        "Disallow unmodified loop conditions",
        NO_PRESET,
        Category::Suspicious,
    ),
    create: create::<NoUnmodifiedLoopCondition>,
};

#[derive(Debug)]
struct NoUnmodifiedLoopCondition {
    check_conditional: bool,
}

impl FromOptions for NoUnmodifiedLoopCondition {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(NoUnmodifiedLoopCondition {
            check_conditional: bool_field(obj_opt(opts, 0), "checkConditionalExpressions", false),
        })
    }
}

struct Condition {
    ident: NodeId,
    group: Option<NodeId>,
    /// The loop span, and the `for` init span (not part of the loop).
    loop_span: Span,
    init: Option<Span>,
    modified: bool,
}

fn inside(outer: Span, inner: Span) -> bool {
    outer.start <= inner.start && inner.end <= outer.end
}

/// Contains a call, member access, `new`, tagged template or `yield` outside nested
/// functions and classes.
fn has_dynamic(cx: &Ctx<'_, '_>, root: NodeId) -> bool {
    let ast = cx.ast();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        match cx.node(n) {
            Node::Call { .. }
            | Node::Member { .. }
            | Node::Index { .. }
            | Node::New { .. }
            | Node::TaggedTemplate { .. }
            | Node::Yield { .. } => return true,
            Node::Arrow { .. } | Node::FunctionExpr { .. } | Node::ClassExpr { .. } => continue,
            _ => ast.for_each_child(n, |c| stack.push(c)),
        }
    }
    false
}

impl NoUnmodifiedLoopCondition {
    /// The loop whose test `ident` is part of, with the outermost static binary or
    /// conditional expression on the way (its group).
    fn condition(&self, cx: &Ctx<'_, '_>, ident: NodeId) -> Option<Condition> {
        let ast = cx.ast();
        let mut group = None;
        let mut child = ident;
        let mut node = cx.parent(ident)?;
        for _ in 0..4096 {
            match *cx.node(node) {
                Node::While { test, .. } | Node::DoWhile { test, .. } => {
                    return (skip_parens(ast, test) == child).then(|| Condition {
                        ident,
                        group,
                        loop_span: ast.span(node),
                        init: None,
                        modified: false,
                    });
                }
                Node::For { test, init, .. } => {
                    return (test.map(|t| skip_parens(ast, t)) == Some(child)).then(|| Condition {
                        ident,
                        group,
                        loop_span: ast.span(node),
                        init: init.map(|i| ast.span(i)),
                        modified: false,
                    });
                }
                Node::Paren { .. } => {
                    node = cx.parent(node)?;
                    continue;
                }
                Node::Binary { op, .. }
                    if !matches!(
                        op,
                        BinaryOp::LogicalAnd | BinaryOp::LogicalOr | BinaryOp::Coalesce
                    ) =>
                {
                    if has_dynamic(cx, node) {
                        return None;
                    }
                    group = Some(node);
                }
                Node::Conditional { .. } if !self.check_conditional => {
                    if has_dynamic(cx, node) {
                        return None;
                    }
                    group = Some(node);
                }
                Node::Binary { .. }
                | Node::Conditional { .. }
                | Node::Unary { .. }
                | Node::Update { .. }
                | Node::Assign { .. }
                | Node::Seq { .. }
                | Node::Await { .. }
                | Node::Spread { .. }
                | Node::TemplateLit { .. }
                | Node::TaggedTemplate { .. }
                | Node::ArrayLit { .. }
                | Node::ObjectLit { .. }
                | Node::Property { .. }
                | Node::ShorthandProp { .. }
                | Node::ComputedKey { .. }
                | Node::As { .. }
                | Node::Satisfies { .. }
                | Node::NonNull { .. }
                | Node::TypeAssertion { .. }
                | Node::Instantiation { .. } => {}
                _ => return None,
            }
            child = node;
            node = cx.parent(node)?;
        }
        None
    }
}

impl Rule for NoUnmodifiedLoopCondition {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let sem = cx.semantic();
        let in_loop = |c: &Condition, at: Span| {
            inside(c.loop_span, at) && !c.init.is_some_and(|i| inside(i, at))
        };
        let mut report: Vec<NodeId> = Vec::new();
        let mut groups: FxHashMap<NodeId, Vec<(NodeId, bool)>> = FxHashMap::default();
        let mut group_order: Vec<NodeId> = Vec::new();
        for s in sem.symbols() {
            let refs: Vec<_> = s.refs.iter().map(|&r| sem.reference(r)).collect();
            let mut conds: Vec<Condition> = refs
                .iter()
                .filter(|r| !r.flags.contains(RefFlags::INIT))
                .filter_map(|r| self.condition(cx, r.node))
                .collect();
            if conds.is_empty() {
                continue;
            }
            let first_is_var = s.decls.first().is_some_and(|d| d.kind == DeclKind::Var);
            let modifiers: Vec<NodeId> = refs
                .iter()
                .filter(|r| {
                    r.flags.contains(RefFlags::WRITE)
                        && (!r.flags.contains(RefFlags::INIT) || first_is_var)
                })
                .map(|r| r.node)
                .collect();
            for c in &mut conds {
                c.modified = modifiers.iter().any(|&m| {
                    if in_loop(c, ast.span(m)) {
                        return true;
                    }
                    // A write in a named function declaration called inside the loop.
                    let Some(f) = ancestors_inclusive(cx.file.parents, m)
                        .find(|&a| matches!(cx.node(a), Node::FunctionDecl { .. }))
                    else {
                        return false;
                    };
                    let Node::FunctionDecl { name: Some(n), .. } = *cx.node(f) else {
                        return false;
                    };
                    sem.symbol_of_decl(n).is_some_and(|fs| {
                        sem.symbol(fs)
                            .refs
                            .iter()
                            .any(|&r| in_loop(c, ast.span(sem.reference(r).node)))
                    })
                });
                match c.group {
                    Some(g) => {
                        let e = groups.entry(g).or_default();
                        if e.is_empty() {
                            group_order.push(g);
                        }
                        e.push((c.ident, c.modified));
                    }
                    None if !c.modified => report.push(c.ident),
                    None => {}
                }
            }
        }
        for g in group_order {
            let members = &groups[&g];
            if members.iter().all(|&(_, m)| !m) {
                report.extend(members.iter().map(|&(i, _)| i));
            }
        }
        report.sort_by_key(|&i| ast.span(i).start);
        for i in report {
            let name = ident_name(ast, i).unwrap_or("").to_owned();
            cx.report(
                ast.span(i),
                format!("'{name}' is not modified in this loop."),
            )
            .id("loopConditionNotModified");
        }
    }
}
