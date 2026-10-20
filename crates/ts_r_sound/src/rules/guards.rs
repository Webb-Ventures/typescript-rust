//! S-07: user-defined type guards and assertion functions in sound files must be proved by
//! their bodies.
//!
//! The body is walked statement by statement, keeping the flow type of the guarded parameter
//! (narrowed by `if` tests through the checker's own condition narrowing,
//! `Ck::narrow_symbol_by_condition`). At each `return e` of a predicate, `e` assumed true must
//! narrow the parameter to the predicate type, and `e` assumed false must leave nothing a
//! caller's false branch excludes (`Ck::narrow_by_type_predicate`). An assertion function's
//! parameter must have the asserted type (or be truthy) at every normal completion.
//! Shapes the walk does not model (returns inside `switch`, a reassigned parameter) are left
//! unreported rather than guessed.

use ts_r_ast::{Node, NodeId, UnaryOp};
use ts_r_types::{SymId, TypeFlags, TypeId};

use crate::codes;
use crate::ctx::Ctx;

const MAX_DEPTH: u32 = 64;

enum Kind {
    /// `x is T`
    Predicate { target: TypeId, false_side: TypeId },
    /// `asserts x is T` (`Some`) or `asserts x` (`None`).
    Asserts(Option<TypeId>),
}

enum Issue {
    TrueSide(TypeId),
    FalseSide,
    Completion(TypeId),
}

struct Guard {
    sym: SymId,
    name: String,
    declared: TypeId,
    kind: Kind,
    /// Inside a construct the walk does not narrow through (returns there are skipped).
    opaque: u32,
    /// The parameter is assigned in the body: nothing is reported.
    reassigned: bool,
    issue: Option<Issue>,
}

pub fn run(cx: &mut Ctx<'_, '_, '_>) {
    if !cx.has_rule(7) {
        return;
    }
    let nodes = cx.nodes.clone();
    for &n in &nodes {
        if !matches!(
            cx.node(n),
            Node::FunctionDecl { .. } | Node::FunctionExpr { .. } | Node::Arrow { .. }
        ) {
            continue;
        }
        let Some(parts) = ts_r_checker::nodes::fn_parts(cx.ast, n) else {
            continue;
        };
        let (Some(rt), Some(body)) = (parts.return_type, parts.body) else {
            continue;
        };
        let Node::TypePredicate { asserts, param, ty } = cx.node(rt) else {
            continue;
        };
        if !matches!(cx.node(param), Node::Ident { .. }) {
            continue; // `this is T` is not checked (the receiver has no flow type here).
        }
        let name = cx.src(param).to_owned();
        let params = cx.ast.list(parts.params).to_vec();
        let Some(&p) = params.iter().find(
            |&&p| matches!(cx.node(p), Node::Param { pattern, .. } if cx.src(pattern) == name),
        ) else {
            continue;
        };
        let Node::Param { pattern, .. } = cx.node(p) else {
            continue;
        };
        let Some(sym) = cx
            .ck
            .c
            .sym_of_decl(cx.loc(p))
            .or_else(|| cx.ck.c.sym_of_decl(cx.loc(pattern)))
        else {
            continue;
        };
        let pl = cx.loc(p);
        let declared = cx.ck.quietly(|ck| ck.param_type(pl));
        if declared.is_any_like() {
            continue;
        }
        let kind = match (asserts, ty) {
            (false, Some(t)) => {
                let target = cx.type_node(t);
                let false_side = cx
                    .ck
                    .quietly(|ck| ck.narrow_by_type_predicate(declared, target, false));
                Kind::Predicate { target, false_side }
            }
            (true, t) => Kind::Asserts(t.map(|t| cx.type_node(t))),
            (false, None) => continue,
        };
        let mut g = Guard {
            sym,
            name,
            declared,
            kind,
            opaque: 0,
            reassigned: false,
            issue: None,
        };
        let is_expr_body = matches!(cx.node(n), Node::Arrow { .. }) && body_is_expr(cx, body);
        if is_expr_body {
            ret(cx, &mut g, Some(body), declared);
        } else if let Some(end) = stmt(cx, &mut g, body, declared, 0) {
            completion(cx, &mut g, end);
        }
        if g.reassigned {
            continue;
        }
        let Some(issue) = g.issue else {
            continue;
        };
        let pred = cx.src(rt).to_owned();
        let span = cx.span(rt);
        let target_text = ty.map(|t| cx.src(t).to_owned()).unwrap_or_default();
        match issue {
            Issue::TrueSide(t) => {
                let ts = cx.type_str(t);
                cx.report(codes::GUARD_TRUE_SIDE, span, &[&g.name, &target_text, &ts]);
            }
            Issue::FalseSide => {
                cx.report(codes::GUARD_FALSE_SIDE, span, &[&g.name, &target_text]);
            }
            Issue::Completion(t) => {
                let ts = cx.type_str(t);
                cx.report(codes::ASSERTION_NOT_PROVED, span, &[&pred, &g.name, &ts]);
            }
        }
    }
}

fn body_is_expr(cx: &Ctx<'_, '_, '_>, body: NodeId) -> bool {
    !matches!(cx.node(body), Node::Block { .. })
}

fn narrow(cx: &mut Ctx<'_, '_, '_>, g: &Guard, t: TypeId, cond: NodeId, assume: bool) -> TypeId {
    let l = cx.loc(cond);
    let (sym, declared) = (g.sym, g.declared);
    let name = g.name.clone();
    cx.ck
        .quietly(|ck| ck.narrow_symbol_by_condition(sym, &name, declared, t, l, assume))
}

fn join(cx: &mut Ctx<'_, '_, '_>, a: Option<TypeId>, b: Option<TypeId>) -> Option<TypeId> {
    match (a, b) {
        (Some(a), Some(b)) if a == b => Some(a),
        (Some(a), Some(b)) => Some(cx.ck.quietly(|ck| ck.union(&[a, b]))),
        (Some(a), None) | (None, Some(a)) => Some(a),
        (None, None) => None,
    }
}

/// Walks statement `s` with the parameter at type `t`; the type after it, `None` when it
/// never completes normally.
fn stmt(
    cx: &mut Ctx<'_, '_, '_>,
    g: &mut Guard,
    s: NodeId,
    t: TypeId,
    depth: u32,
) -> Option<TypeId> {
    if depth > MAX_DEPTH || g.issue.is_some() {
        return Some(t);
    }
    match cx.node(s) {
        Node::Block { stmts } => {
            let mut cur = t;
            for &x in cx.ast.list(stmts).to_vec().iter() {
                cur = stmt(cx, g, x, cur, depth + 1)?;
            }
            Some(cur)
        }
        Node::If { test, cons, alt } => {
            note_assignments(cx, g, test);
            let tt = narrow(cx, g, t, test, true);
            let ff = narrow(cx, g, t, test, false);
            let a = stmt(cx, g, cons, tt, depth + 1);
            let b = match alt {
                Some(alt) => stmt(cx, g, alt, ff, depth + 1),
                None => Some(ff),
            };
            join(cx, a, b)
        }
        Node::Return { arg } => {
            ret(cx, g, arg, t);
            None
        }
        Node::Throw { .. } => None,
        Node::ExprStmt { expr } => {
            note_assignments(cx, g, expr);
            Some(t)
        }
        Node::While { test, body }
        | Node::For {
            test: Some(test),
            body,
            ..
        } => {
            note_assignments(cx, g, test);
            let tt = narrow(cx, g, t, test, true);
            let out = stmt(cx, g, body, tt, depth + 1);
            let ff = narrow(cx, g, t, test, false);
            join(cx, Some(ff), out.map(|_| t))
        }
        Node::For { body, .. }
        | Node::ForOf { body, .. }
        | Node::ForIn { body, .. }
        | Node::DoWhile { body, .. } => {
            note_assignments(cx, g, s);
            let _ = stmt(cx, g, body, t, depth + 1);
            Some(t)
        }
        Node::Try {
            block,
            handler,
            finalizer,
        } => {
            let a = stmt(cx, g, block, t, depth + 1);
            let b = match handler {
                Some(h) => match cx.node(h) {
                    Node::CatchClause { body, .. } => stmt(cx, g, body, t, depth + 1),
                    _ => Some(t),
                },
                None => None,
            };
            let out = join(cx, a, b);
            if let Some(f) = finalizer {
                let _ = stmt(cx, g, f, t, depth + 1)?;
            }
            out
        }
        Node::FunctionDecl { .. } | Node::ClassDecl { .. } => Some(t),
        Node::VarDecl { .. } => {
            note_assignments(cx, g, s);
            Some(t)
        }
        _ => {
            // `switch`, labels, `with`: returns inside are not narrowed by the walk.
            note_assignments(cx, g, s);
            g.opaque += 1;
            let mut kids = Vec::new();
            cx.ast.for_each_child(s, |c| kids.push(c));
            let mut completes = true;
            for k in kids {
                if is_statement(cx, k) && stmt(cx, g, k, t, depth + 1).is_none() {
                    completes = matches!(cx.node(s), Node::Switch { .. } | Node::Labeled { .. });
                }
            }
            g.opaque -= 1;
            completes.then_some(t)
        }
    }
}

fn is_statement(cx: &Ctx<'_, '_, '_>, n: NodeId) -> bool {
    matches!(
        cx.node(n),
        Node::Block { .. }
            | Node::If { .. }
            | Node::Return { .. }
            | Node::Throw { .. }
            | Node::ExprStmt { .. }
            | Node::While { .. }
            | Node::For { .. }
            | Node::ForOf { .. }
            | Node::ForIn { .. }
            | Node::DoWhile { .. }
            | Node::Try { .. }
            | Node::Switch { .. }
            | Node::SwitchCase { .. }
            | Node::Labeled { .. }
            | Node::VarDecl { .. }
    )
}

/// Marks the guard unprovable when the parameter is assigned anywhere in `n`.
fn note_assignments(cx: &Ctx<'_, '_, '_>, g: &mut Guard, n: NodeId) {
    let mut stack = vec![n];
    let mut steps = 0u32;
    while let Some(x) = stack.pop() {
        steps += 1;
        if steps > 100_000 {
            return;
        }
        match cx.node(x) {
            Node::Assign { left, .. } | Node::Update { arg: left, .. }
                if cx.src(cx.skip_parens(left)) == g.name =>
            {
                g.reassigned = true;
                return;
            }
            _ => {}
        }
        cx.ast.for_each_child(x, |c| stack.push(c));
    }
}

fn bool_lit(cx: &Ctx<'_, '_, '_>, e: NodeId) -> Option<bool> {
    match cx.node(cx.skip_parens(e)) {
        Node::BoolLit { value } => Some(value),
        Node::Unary {
            op: UnaryOp::Not,
            arg,
        } => bool_lit(cx, arg).map(|b| !b),
        _ => None,
    }
}

fn ret(cx: &mut Ctx<'_, '_, '_>, g: &mut Guard, arg: Option<NodeId>, t: TypeId) {
    if g.issue.is_some() || g.opaque > 0 {
        return;
    }
    match g.kind {
        Kind::Asserts(_) => completion(cx, g, t),
        Kind::Predicate { target, false_side } => {
            let Some(e) = arg else {
                return;
            };
            note_assignments(cx, g, e);
            let (tt, ff) = match bool_lit(cx, e) {
                Some(true) => (Some(t), None),
                Some(false) => (None, Some(t)),
                None => (
                    Some(narrow(cx, g, t, e, true)),
                    Some(narrow(cx, g, t, e, false)),
                ),
            };
            if let Some(tt) = tt
                && !cx.flags(tt).contains(TypeFlags::NEVER)
                && !cx.is_assignable(tt, target)
            {
                g.issue = Some(Issue::TrueSide(tt));
                return;
            }
            if cx.info.config.policy.guard_false_side
                && let Some(ff) = ff
                && !cx.flags(ff).contains(TypeFlags::NEVER)
                && !false_side_ok(cx, ff, false_side)
            {
                g.issue = Some(Issue::FalseSide);
            }
        }
    }
}

/// Whether the false-side type `ff` fits what a caller's false branch assumes. Type
/// parameter members are skipped: `T` left on the false side of `x !== undefined` only means
/// `T` itself may include `undefined`, which the caller's narrowing allows too.
fn false_side_ok(cx: &mut Ctx<'_, '_, '_>, ff: TypeId, false_side: TypeId) -> bool {
    let members: Vec<TypeId> = if cx.flags(ff).contains(TypeFlags::UNION) {
        cx.ck.store().members_of_union(ff).to_vec()
    } else {
        vec![ff]
    };
    members
        .into_iter()
        .all(|m| cx.flags(m).contains(TypeFlags::TYPE_PARAMETER) || cx.is_assignable(m, false_side))
}

/// A normal completion of an assertion function with the parameter at type `t`.
fn completion(cx: &mut Ctx<'_, '_, '_>, g: &mut Guard, t: TypeId) {
    if g.issue.is_some() || g.opaque > 0 {
        return;
    }
    let Kind::Asserts(target) = g.kind else {
        return;
    };
    if cx.flags(t).contains(TypeFlags::NEVER) {
        return;
    }
    let ok = match target {
        Some(target) => cx.is_assignable(t, target),
        None => {
            if cx.flags(t).intersects(TypeFlags::ANY_OR_UNKNOWN) {
                false
            } else {
                let falsy = cx.ck.quietly(|ck| ck.extract_definitely_falsy(t));
                cx.flags(falsy).contains(TypeFlags::NEVER)
            }
        }
    };
    if !ok {
        g.issue = Some(Issue::Completion(t));
    }
}
