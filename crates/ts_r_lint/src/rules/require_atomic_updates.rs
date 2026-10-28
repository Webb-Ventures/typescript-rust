//! `require-atomic-updates`: in an async function or generator, an assignment whose target
//! was read before an `await` / `yield` and is written after it.
//!
//! Reads and suspensions flow through the function's control-flow graph in one forward pass
//! (loop back edges don't carry state, as in ESLint's code-path segments).

use super::prelude::*;
use crate::cfg::Cfg;
use rustc_hash::{FxHashMap, FxHashSet};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "require-atomic-updates",
        "Disallow assignments that can lead to race conditions due to usage of `await` or `yield`",
        NO_PRESET,
        Category::Suspicious,
    ),
    create: create::<RequireAtomicUpdates>,
};

#[derive(Debug)]
struct RequireAtomicUpdates {
    allow_properties: bool,
}

impl FromOptions for RequireAtomicUpdates {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(RequireAtomicUpdates {
            allow_properties: bool_field(obj_opt(opts, 0), "allowProperties", false),
        })
    }
}

/// A variable: a symbol, or a global (by name, with the high bit set).
type Var = u64;

/// Read-before-suspension state: `(outdated, fresh)` variables, sorted.
type State = (Vec<Var>, Vec<Var>);

fn insert(v: &mut Vec<Var>, s: Var) {
    if let Err(i) = v.binary_search(&s) {
        v.insert(i, s);
    }
}

fn remove(v: &mut Vec<Var>, s: Var) {
    if let Ok(i) = v.binary_search(&s) {
        v.remove(i);
    }
}

fn union(a: &mut Vec<Var>, b: &[Var]) {
    for &s in b {
        insert(a, s);
    }
}

/// An `await` or `yield` in `n` outside nested functions.
fn contains_suspension(cx: &Ctx<'_, '_>, n: NodeId) -> bool {
    let ast = cx.ast();
    let mut stack = vec![n];
    while let Some(x) = stack.pop() {
        match cx.node(x) {
            Node::Await { .. } | Node::Yield { .. } => return true,
            Node::FunctionExpr { .. } | Node::Arrow { .. } | Node::FunctionDecl { .. } => continue,
            _ => ast.for_each_child(x, |c| stack.push(c)),
        }
    }
    false
}

/// A write target: the reference's identifier, the assignment, and whether the identifier is
/// the whole target (not an object written through).
struct Target {
    ident: NodeId,
    sym: Var,
    assign: NodeId,
    direct: bool,
    compound: bool,
}

impl RequireAtomicUpdates {
    fn check_function(&self, cx: &mut Ctx<'_, '_>, f: NodeId) {
        let ast = cx.ast();
        let sem = cx.semantic();
        let parents = cx.file.parents;
        let var_scope = |mut s: ScopeId| {
            for _ in 0..4096 {
                let sc = sem.scope(s);
                match sc.kind {
                    ScopeKind::Function
                    | ScopeKind::ClassFieldInit
                    | ScopeKind::StaticBlock
                    | ScopeKind::Global
                    | ScopeKind::Module => return s,
                    _ => match sc.parent {
                        Some(p) => s = p,
                        None => return s,
                    },
                }
            }
            s
        };
        // Every reference of a symbol is scanned once, not once per assignment to it.
        let mut escapes: FxHashMap<(SymbolId, bool), bool> = FxHashMap::default();
        let mut local_without_escape = |sym: SymbolId, member: bool| {
            *escapes.entry((sym, member)).or_insert_with(|| {
                let s = sem.symbol(sym);
                if member && s.decls.iter().any(|d| d.kind == DeclKind::Param) {
                    return false;
                }
                let home = var_scope(s.scope);
                s.refs
                    .iter()
                    .all(|&r| var_scope(sem.reference(r).scope) == home)
            })
        };
        // Pending checks, keyed by the assigned expression.
        let mut targets: FxHashMap<NodeId, Vec<Target>> = FxHashMap::default();
        let mut reads: FxHashMap<NodeId, (Var, bool)> = FxHashMap::default();
        let cfg = Cfg::function(ast, f);
        let mut events: Vec<NodeId> = cfg
            .blocks
            .iter()
            .flat_map(|b| b.events.iter().copied())
            .filter(|&e| matches!(cx.node(e), Node::Ident { .. }))
            .collect();
        events.sort_unstable();
        events.dedup();
        for r in events
            .iter()
            .filter_map(|&e| sem.reference_at(e))
            .map(|r| sem.reference(r))
        {
            // Globals are never local to the function.
            let (var, sym) = match r.resolution {
                Resolution::Symbol(s) => (s.0 as Var, Some(s)),
                Resolution::Global => ((1 << 32) | r.name.0 as Var, None),
                Resolution::Unresolved => continue,
            };
            if r.flags.contains(RefFlags::INIT) {
                continue;
            }
            // The assigned expression: `x = e`, `x.y = e`, `x.y.z += e`.
            let mut n = r.node;
            let mut write = None;
            for _ in 0..4096 {
                let Some(p) = parent_skip_parens(ast, parents, n) else {
                    break;
                };
                match *cx.node(p) {
                    Node::Assign { left, right, op } if skip_parens(ast, left) == n => {
                        write = Some((p, right, op));
                        break;
                    }
                    Node::Member { obj, .. } | Node::Index { obj, .. }
                        if skip_parens(ast, obj) == n =>
                    {
                        n = p
                    }
                    _ => break,
                }
            }
            let member = parent_skip_parens(ast, parents, r.node)
                .is_some_and(|p| matches!(cx.node(p), Node::Member { .. } | Node::Index { .. }));
            let is_read = r.flags.intersects(RefFlags::READ | RefFlags::COMPOUND);
            let plain = write.is_some_and(|(_, _, op)| op == AssignOp::Assign);
            if is_read && !plain {
                let compound_direct = write.is_some_and(|(a, _, _)| {
                    matches!(*cx.node(a), Node::Assign { left, .. } if skip_parens(ast, left) == r.node)
                });
                reads.insert(r.node, (var, compound_direct));
            }
            if let Some((assign, right, op)) = write
                && !sym.is_some_and(|s| local_without_escape(s, member))
            {
                let direct = skip_parens(
                    ast,
                    match *cx.node(assign) {
                        Node::Assign { left, .. } => left,
                        _ => assign,
                    },
                ) == r.node;
                targets
                    .entry(skip_parens(ast, right))
                    .or_default()
                    .push(Target {
                        ident: r.node,
                        sym: var,
                        assign,
                        direct,
                        compound: op != AssignOp::Assign,
                    });
            }
        }
        if targets.is_empty() {
            return;
        }
        // Forward pass in reverse postorder, ignoring DFS back edges.
        let n = cfg.blocks.len();
        let mut order = Vec::with_capacity(n);
        let mut color = vec![0u8; n];
        let mut back: FxHashSet<(u32, u32)> = FxHashSet::default();
        let mut stack = vec![(cfg.entry, 0usize)];
        color[cfg.entry as usize] = 1;
        while let Some(&mut (b, ref mut i)) = stack.last_mut() {
            let succ = &cfg.blocks[b as usize].succ;
            if *i < succ.len() {
                let t = succ[*i];
                *i += 1;
                match color[t as usize] {
                    0 => {
                        color[t as usize] = 1;
                        stack.push((t, 0));
                    }
                    1 => {
                        back.insert((b, t));
                    }
                    _ => {}
                }
            } else {
                color[b as usize] = 2;
                order.push(b);
                stack.pop();
            }
        }
        order.reverse();
        let mut preds: Vec<Vec<u32>> = vec![Vec::new(); n];
        for &b in &order {
            for &t in &cfg.blocks[b as usize].succ {
                if !back.contains(&(b, t)) {
                    preds[t as usize].push(b);
                }
            }
        }
        let mut exit: Vec<Option<State>> = vec![None; n];
        let mut reported: FxHashSet<NodeId> = FxHashSet::default();
        let mut out: Vec<(Span, &'static str, String)> = Vec::new();
        for &b in &order {
            let mut st: State = (Vec::new(), Vec::new());
            for &p in &preds[b as usize] {
                if let Some((o, fr)) = &exit[p as usize] {
                    union(&mut st.0, o);
                    union(&mut st.1, fr);
                }
            }
            for &e in &cfg.blocks[b as usize].events {
                if let Some(&(sym, compound_direct)) = reads.get(&e) {
                    // `x += e` reads `x` before evaluating `e`.
                    let suspended = compound_direct
                        && matches!(*cx.node(parent_skip_parens(ast, parents, e).unwrap_or(e)), Node::Assign { right, .. } if contains_suspension(cx, right));
                    if suspended {
                        insert(&mut st.0, sym);
                        remove(&mut st.1, sym);
                    } else {
                        insert(&mut st.1, sym);
                        remove(&mut st.0, sym);
                    }
                }
                if matches!(cx.node(e), Node::Await { .. } | Node::Yield { .. }) {
                    let fresh = std::mem::take(&mut st.1);
                    union(&mut st.0, &fresh);
                }
                let Some(ts) = targets.get(&e) else { continue };
                if !reported.insert(e) {
                    continue;
                }
                for t in ts {
                    let outdated = if t.direct && t.compound {
                        contains_suspension(cx, e)
                    } else {
                        st.0.binary_search(&t.sym).is_ok()
                    };
                    if !outdated {
                        continue;
                    }
                    let name = ident_name(ast, t.ident).unwrap_or("");
                    let span = ast.span(t.assign);
                    if t.direct {
                        out.push((span, "nonAtomicUpdate", format!("Possible race condition: `{name}` might be reassigned based on an outdated value of `{name}`.")));
                    } else if !self.allow_properties {
                        let left = match *cx.node(t.assign) {
                            Node::Assign { left, .. } => cx.text(left),
                            _ => "",
                        };
                        out.push((span, "nonAtomicObjectUpdate", format!("Possible race condition: `{left}` might be assigned based on an outdated state of `{name}`.")));
                    }
                }
            }
            exit[b as usize] = Some(st);
        }
        for (span, id, msg) in out {
            cx.report(span, msg).id(id);
        }
    }
}

impl Rule for RequireAtomicUpdates {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::FunctionDecl,
            NodeKind::FunctionExpr,
            NodeKind::Arrow,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let flags = match *cx.node(node) {
            Node::FunctionDecl {
                flags,
                body: Some(_),
                ..
            }
            | Node::FunctionExpr {
                flags,
                body: Some(_),
                ..
            } => flags,
            Node::Arrow { flags, .. } => flags,
            _ => return,
        };
        if flags.intersects(FnFlags::ASYNC | FnFlags::GENERATOR) {
            self.check_function(cx, node);
        }
    }
}
