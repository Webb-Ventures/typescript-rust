//! Evolving arrays: `let xs = []` under noImplicitAny (TypeScript files) has no element type
//! of its own; each read gets the array of what the code before it pushed (observed tsc
//! 7.0.2).
//!
//! - `xs.push(a, ...)`, `xs.unshift(a, ...)` (statement calls) and `xs[n] = v` (numeric `n`)
//!   add the widened types of their arguments / value.
//! - `xs = []` starts over; `xs = other` makes the reference that type on that path.
//! - Branches join; loops iterate to a fixed point.
//! - A read where nothing was pushed is `any[]` (tsc also reports TS7034/TS7005 there; not
//!   modelled). `xs.push`, `xs.unshift`, `xs.length` and `xs[n] = v` targets keep the
//!   silent declared type.
//!
//! Anything else (references from nested functions, the start of the function before the
//! declaration, destructuring or compound assignments to the variable) keeps the declared
//! `UNRESOLVED[]`, which reports nothing.

use rustc_hash::FxHashMap;
use ts_r_ast::{AssignOp, Node};
use ts_r_binder::{FlowId, FlowKind};
use ts_r_types::{SymId, TypeFlags, TypeId};

use crate::checker::{Ck, Loc};
use crate::nodes::skip_parens;

/// Bound on the flow nodes visited for one read.
const MAX_STEPS: u32 = 20_000;
/// Iterations of a loop label before giving up.
const MAX_LOOP_ROUNDS: u32 = 6;

/// The value of the variable on the paths reaching a flow node.
#[derive(Clone, Debug, Default, PartialEq)]
struct Ev {
    /// Some path is an evolving array.
    evolving: bool,
    /// Element types pushed on the evolving paths.
    elems: Vec<TypeId>,
    /// Types assigned on the other paths.
    finals: Vec<TypeId>,
}

impl Ev {
    fn empty_array() -> Ev {
        Ev {
            evolving: true,
            ..Ev::default()
        }
    }

    fn join(&mut self, o: &Ev) {
        self.evolving |= o.evolving;
        for &t in &o.elems {
            if !self.elems.contains(&t) {
                self.elems.push(t);
            }
        }
        for &t in &o.finals {
            if !self.finals.contains(&t) {
                self.finals.push(t);
            }
        }
    }
}

struct EvWalk {
    sym: SymId,
    decl: Loc,
    steps: u32,
    memo: FxHashMap<u32, Ev>,
    /// Loop labels being iterated, with their value so far.
    in_loop: FxHashMap<u32, Ev>,
}

impl<'c, 'a> Ck<'c, 'a> {
    /// Whether the variable declared at the binding identifier `decl` is an evolving array:
    /// `let`/`var`/`const xs = []` under noImplicitAny in a TypeScript file, not ambient or
    /// exported.
    pub(crate) fn is_evolving_array_variable(&self, decl: Loc) -> bool {
        // JavaScript declarations may carry a JSDoc type; not modelled here.
        if !self.c.opts.no_implicit_any || self.c.file(decl.file).is_js() {
            return false;
        }
        let Some(p) = self.c.parent(decl) else {
            return false;
        };
        let ast = self.ast(decl.file);
        let Node::VarDeclarator {
            name,
            type_ann: None,
            init: Some(init),
            ..
        } = *ast.node(p.node)
        else {
            return false;
        };
        if name != decl.node || self.is_ambient_node(decl) {
            return false;
        }
        if let Some(list) = self.c.parent(p)
            && let Node::VarDecl { modifiers, .. } = *self.node(list)
            && modifiers.contains(ts_r_ast::Modifiers::EXPORT)
        {
            return false;
        }
        matches!(*ast.node(skip_parens(ast, init)), Node::ArrayLit { elements } if ast.list(elements).is_empty())
    }

    /// `xs.push(..)`, `xs.unshift(..)`, `xs.length` or the `xs` of `xs[n] = v`: the
    /// reference keeps the declared type.
    pub(crate) fn is_evolving_array_operation_target(&self, e: Loc) -> bool {
        let ast = self.ast(e.file);
        // Parentheses are transparent: `(xs).push(..)`.
        let mut e = e;
        let Some(mut p) = self.c.parent(e) else {
            return false;
        };
        while matches!(*ast.node(p.node), Node::Paren { .. }) {
            e = p;
            let Some(g) = self.c.parent(p) else {
                return false;
            };
            p = g;
        }
        match *ast.node(p.node) {
            Node::Member { obj, prop, .. } if obj == e.node => {
                let Some(name) = ast.name_of(prop) else {
                    return false;
                };
                if name == "length" {
                    return true;
                }
                (name == "push" || name == "unshift")
                    && self.c.parent(p).is_some_and(|g| {
                        matches!(*ast.node(g.node), Node::Call { callee, .. } if callee == p.node)
                    })
            }
            Node::Index { obj, .. } if obj == e.node => self.c.parent(p).is_some_and(|g| {
                matches!(*ast.node(g.node), Node::Assign { left, op: AssignOp::Assign, .. } if left == p.node)
            }),
            _ => false,
        }
    }

    /// The type of a read `e` of the evolving array variable `sym` declared at `decl`, and
    /// whether it is an implicit `any[]` (nothing pushed on any path); `None` when the read
    /// is not modelled.
    pub(crate) fn evolving_array_read_type(
        &mut self,
        e: Loc,
        sym: SymId,
        decl: Loc,
    ) -> Option<(TypeId, bool)> {
        let flow = self.c.bound(e.file).flow.flow_at(e.node)?;
        let mut w = EvWalk {
            sym,
            decl,
            steps: 0,
            memo: FxHashMap::default(),
            in_loop: FxHashMap::default(),
        };
        let ev = self.ev_walk(&mut w, e.file, flow)?;
        let implicit = ev.evolving && ev.elems.is_empty() && ev.finals.is_empty();
        let mut types = ev.finals.clone();
        if ev.evolving {
            let el = if ev.elems.is_empty() {
                TypeId::ANY
            } else {
                self.union(&ev.elems)
            };
            types.push(self.create_array(el, false));
        }
        if types.is_empty() {
            return None;
        }
        Some((self.union(&types), implicit))
    }

    fn ev_walk(
        &mut self,
        w: &mut EvWalk,
        file: ts_r_diagnostics::FileId,
        start: FlowId,
    ) -> Option<Ev> {
        let mut flow = start;
        // Mutations seen on the way back to the node that decides the value.
        let mut pushed: Vec<TypeId> = Vec::new();
        loop {
            w.steps += 1;
            if w.steps > MAX_STEPS {
                return None;
            }
            if flow.is_unreachable() {
                return Some(Ev::default());
            }
            if let Some(v) = w.memo.get(&flow.0) {
                let mut v = v.clone();
                add_pushed(&mut v, &pushed);
                return Some(v);
            }
            let graph = &self.c.bound(file).flow;
            let node = *graph.nodes.get(flow.index())?;
            match node.kind {
                FlowKind::Unreachable => return Some(Ev::default()),
                FlowKind::Start => return None,
                FlowKind::Assignment => {
                    let Some(target) = node.node else {
                        flow = FlowId(node.a);
                        continue;
                    };
                    let tl = Loc::new(file, target);
                    match self.ev_assignment(w, tl)? {
                        Some(mut v) => {
                            add_pushed(&mut v, &pushed);
                            return Some(v);
                        }
                        None => flow = FlowId(node.a),
                    }
                }
                FlowKind::ArrayMutation => {
                    if let Some(n) = node.node {
                        pushed.extend(self.ev_mutation(w, Loc::new(file, n))?);
                    }
                    flow = FlowId(node.a);
                }
                FlowKind::TrueCondition
                | FlowKind::FalseCondition
                | FlowKind::SwitchClause
                | FlowKind::Call
                | FlowKind::ReduceLabel => flow = FlowId(node.a),
                FlowKind::BranchLabel => {
                    let antes: Vec<FlowId> = graph.antecedents(flow).to_vec();
                    let mut v = Ev::default();
                    for a in antes {
                        let av = self.ev_walk(w, file, a)?;
                        v.join(&av);
                    }
                    w.memo.insert(flow.0, v.clone());
                    add_pushed(&mut v, &pushed);
                    return Some(v);
                }
                FlowKind::LoopLabel => {
                    if let Some(v) = w.in_loop.get(&flow.0) {
                        let mut v = v.clone();
                        add_pushed(&mut v, &pushed);
                        return Some(v);
                    }
                    let antes: Vec<FlowId> = graph.antecedents(flow).to_vec();
                    let mut cur = Ev::default();
                    let mut done = false;
                    for _ in 0..MAX_LOOP_ROUNDS {
                        w.in_loop.insert(flow.0, cur.clone());
                        // Memoized nodes inside the loop depend on the value so far.
                        let saved = w.memo.clone();
                        let mut next = Ev::default();
                        for &a in &antes {
                            let av = self.ev_walk(w, file, a)?;
                            next.join(&av);
                        }
                        w.memo = saved;
                        if next == cur {
                            done = true;
                            break;
                        }
                        cur = next;
                    }
                    w.in_loop.remove(&flow.0);
                    if !done {
                        return None;
                    }
                    w.memo.insert(flow.0, cur.clone());
                    add_pushed(&mut cur, &pushed);
                    return Some(cur);
                }
            }
        }
    }

    /// The effect of an assignment flow node on the variable: `Some(Some(ev))` decides the
    /// value, `Some(None)` is unrelated, `None` is not modelled.
    fn ev_assignment(&mut self, w: &EvWalk, target: Loc) -> Option<Option<Ev>> {
        if target == w.decl {
            return Some(Some(Ev::empty_array()));
        }
        let ast = self.ast(target.file);
        if !matches!(*ast.node(target.node), Node::Ident { .. }) {
            return Some(None);
        }
        let Some(name) = ast.name_of(target.node) else {
            return Some(None);
        };
        if self.c.sym_name(w.sym) != name {
            return Some(None);
        }
        if self.ident_ref_symbol(target) != Some(w.sym) {
            return Some(None);
        }
        let p = self.c.parent(target)?;
        let Node::Assign {
            left,
            op: AssignOp::Assign,
            right,
        } = *self.node(p)
        else {
            return None;
        };
        if left != target.node {
            return None;
        }
        let r = skip_parens(ast, right);
        if let Node::ArrayLit { elements } = *ast.node(r)
            && ast.list(elements).is_empty()
        {
            return Some(Some(Ev::empty_array()));
        }
        let t = self.quietly(|ck| ck.check_expr(Loc::new(target.file, right)));
        let t = self.widen_literal(t);
        let t = self.widened_type(t);
        Some(Some(Ev {
            finals: vec![t],
            ..Ev::default()
        }))
    }

    /// Element types an array mutation node adds to the variable (empty when it mutates
    /// something else); `None` when not modelled.
    fn ev_mutation(&mut self, w: &EvWalk, n: Loc) -> Option<Vec<TypeId>> {
        let ast = self.ast(n.file);
        match *ast.node(n.node) {
            Node::Call { callee, args, .. } => {
                let Node::Member { obj, .. } = *ast.node(skip_parens(ast, callee)) else {
                    return Some(Vec::new());
                };
                if !self.ev_is_var(w, Loc::new(n.file, skip_parens(ast, obj))) {
                    return Some(Vec::new());
                }
                let mut out = Vec::new();
                for &a in ast.list(args) {
                    let t = match *ast.node(a) {
                        Node::Spread { arg } => {
                            let st = self.quietly(|ck| ck.check_expr(Loc::new(n.file, arg)));
                            self.iterated_type(st, false)?
                        }
                        _ => self.quietly(|ck| ck.check_expr(Loc::new(n.file, a))),
                    };
                    out.push(self.ev_element(t));
                }
                Some(out)
            }
            Node::Index { obj, index, .. } => {
                if !self.ev_is_var(w, Loc::new(n.file, skip_parens(ast, obj))) {
                    return Some(Vec::new());
                }
                let p = self.c.parent(n)?;
                let Node::Assign {
                    left,
                    op: AssignOp::Assign,
                    right,
                } = *self.node(p)
                else {
                    return None;
                };
                if left != n.node {
                    return None;
                }
                let it = self.quietly(|ck| ck.check_expr(Loc::new(n.file, index)));
                if !self.store().flags(it).intersects(TypeFlags::NUMBER_LIKE) {
                    return Some(Vec::new());
                }
                let t = self.quietly(|ck| ck.check_expr(Loc::new(n.file, right)));
                Some(vec![self.ev_element(t)])
            }
            _ => Some(Vec::new()),
        }
    }

    fn ev_element(&mut self, t: TypeId) -> TypeId {
        let t = self.widen_literal(t);
        self.widened_type(t)
    }

    fn ev_is_var(&mut self, w: &EvWalk, l: Loc) -> bool {
        let ast = self.ast(l.file);
        match ast.name_of(l.node) {
            Some(name) if matches!(*ast.node(l.node), Node::Ident { .. }) => {
                self.c.sym_name(w.sym) == name && self.ident_ref_symbol(l) == Some(w.sym)
            }
            _ => false,
        }
    }
}

/// Adds element types pushed after the deciding node to the evolving paths of `v`.
fn add_pushed(v: &mut Ev, pushed: &[TypeId]) {
    if !v.evolving {
        return;
    }
    for &t in pushed {
        if !v.elems.contains(&t) {
            v.elems.push(t);
        }
    }
}
