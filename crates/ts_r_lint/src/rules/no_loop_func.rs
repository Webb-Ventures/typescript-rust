//! `no-loop-func`: a function created in a loop that refers to a binding the loop changes.

use rustc_hash::FxHashMap;

use super::prelude::*;
use crate::semantic::{RefId, Reference, Resolution, ScopeId, ScopeKind, Semantic};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-loop-func",
        "Disallow function declarations that contain unsafe references inside loop statements",
        NO_PRESET,
        Category::Suspicious,
    ),
    create: create_default::<NoLoopFunc>,
};

#[derive(Debug, Default)]
struct NoLoopFunc;

fn fn_flags(node: &Node) -> Option<FnFlags> {
    match *node {
        Node::FunctionDecl { flags, .. }
        | Node::FunctionExpr { flags, .. }
        | Node::Arrow { flags, .. } => Some(flags),
        _ => None,
    }
}

struct Walk<'c, 'a, 'f> {
    cx: &'c Ctx<'a, 'f>,
    /// Whether a function is an IIFE that the rule skips (its body counts as the loop's).
    skipped: FxHashMap<NodeId, bool>,
    /// References to each configured global, by name.
    globals: Option<FxHashMap<&'a str, Vec<RefId>>>,
    /// Each function scope's references that resolve outside it (or not at all).
    through: FxHashMap<NodeId, Vec<RefId>>,
}

impl<'c, 'a, 'f> Walk<'c, 'a, 'f> {
    /// The loop `node` runs in, if no function boundary comes first.
    fn containing_loop(&mut self, node: NodeId, depth: u32) -> Option<NodeId> {
        let cx = self.cx;
        let mut cur = node;
        for _ in 0..100_000 {
            let p = cx.parent(cur)?;
            match *cx.node(p) {
                Node::While { .. } | Node::DoWhile { .. } => return Some(p),
                Node::For { init, .. } if init != Some(cur) => return Some(p),
                Node::ForIn { right, .. } | Node::ForOf { right, .. } if right != cur => {
                    return Some(p);
                }
                Node::Arrow { .. } | Node::FunctionExpr { .. } | Node::FunctionDecl { .. } => {
                    if !self.is_skipped(p, depth + 1) {
                        return None;
                    }
                }
                _ => {}
            }
            cur = p;
        }
        None
    }

    /// `f` is a plain IIFE in a loop that doesn't refer to its own name.
    fn is_skipped(&mut self, f: NodeId, depth: u32) -> bool {
        if let Some(&v) = self.skipped.get(&f) {
            return v;
        }
        if depth > 200 {
            return false;
        }
        let cx = self.cx;
        let v = (|| {
            let flags = fn_flags(cx.node(f))?;
            if flags.contains(FnFlags::ASYNC) || flags.contains(FnFlags::GENERATOR) {
                return None;
            }
            if matches!(cx.node(f), Node::FunctionDecl { .. }) {
                return None;
            }
            let mut callee = f;
            let mut p = cx.parent(f)?;
            while matches!(cx.node(p), Node::Paren { .. }) {
                callee = p;
                p = cx.parent(p)?;
            }
            if !matches!(*cx.node(p), Node::Call { callee: c, .. } if c == callee) {
                return None;
            }
            self.containing_loop(f, depth)?;
            if let Node::FunctionExpr { name: Some(n), .. } = *cx.node(f) {
                let name = ident_name(cx.ast(), n)?;
                if self
                    .through(f)
                    .iter()
                    .any(|r| cx.ast().atom(r.name) == name)
                {
                    return None;
                }
            }
            Some(())
        })()
        .is_some();
        self.skipped.insert(f, v);
        v
    }

    /// References inside `f` that resolve outside it (or not at all).
    fn through(&self, f: NodeId) -> Vec<&'a Reference> {
        let sem = self.cx.semantic();
        self.through
            .get(&f)
            .map(|v| v.iter().map(|&r| sem.reference(r)).collect())
            .unwrap_or_default()
    }

    /// The outermost loop around `node` that starts after `border`.
    fn top_loop(&mut self, node: NodeId, border: u32) -> NodeId {
        let mut ret = node;
        let mut cur = Some(node);
        for _ in 0..100_000 {
            let Some(c) = cur else { break };
            if self.cx.span(c).start < border {
                break;
            }
            ret = c;
            cur = self.containing_loop(c, 0);
        }
        ret
    }

    fn is_safe(&mut self, lp: NodeId, r: &Reference) -> bool {
        let cx = self.cx;
        let sem = cx.semantic();
        let (refs, decl_scope, kind_decl): (
            Vec<RefId>,
            Option<ScopeId>,
            Option<(VarKind, NodeId)>,
        ) = match r.resolution {
            Resolution::Symbol(s) => {
                let sym = sem.symbol(s);
                let kd = sym.decls.first().and_then(|d| match *cx.node(d.parent) {
                    Node::VarDecl { kind, .. } => Some((kind, d.parent)),
                    _ => None,
                });
                (sym.refs.clone(), Some(sym.scope), kd)
            }
            Resolution::Global => {
                let name = cx.ast().atom(r.name);
                let globals = self.globals.get_or_insert_with(|| {
                    let mut m: FxHashMap<&str, Vec<RefId>> = FxHashMap::default();
                    for (i, r) in sem.references().iter().enumerate() {
                        if r.resolution == Resolution::Global {
                            m.entry(cx.ast().atom(r.name))
                                .or_default()
                                .push(RefId(i as u32));
                        }
                    }
                    m
                });
                (
                    globals.get(name).cloned().unwrap_or_default(),
                    Some(sem.global_scope()),
                    None,
                )
            }
            Resolution::Unresolved => return true,
        };
        if let Some((kind, decl)) = kind_decl {
            if matches!(kind, VarKind::Const | VarKind::Using | VarKind::AwaitUsing) {
                return true;
            }
            let (ds, ls) = (cx.span(decl), cx.span(lp));
            if kind == VarKind::Let && ds.start > ls.start && ds.end < ls.end {
                return true;
            }
        }
        let border = match kind_decl {
            Some((VarKind::Let, d)) => cx.span(d).end,
            _ => 0,
        };
        let border = cx.span(self.top_loop(lp, border)).start;
        let vs = decl_scope.map(|s| var_scope(sem, s));
        refs.iter().all(|&id| {
            let u = sem.reference(id);
            !u.is_write() || (vs == Some(var_scope(sem, u.scope)) && cx.span(u.node).start < border)
        })
    }
}

/// For each function scope, the references from inside it that resolve outside it.
fn through_map(sem: &dyn Semantic) -> FxHashMap<NodeId, Vec<RefId>> {
    let mut m: FxHashMap<NodeId, Vec<RefId>> = FxHashMap::default();
    for (i, r) in sem.references().iter().enumerate() {
        let target = r.symbol().map(|s| sem.symbol(s).scope);
        let mut s = Some(r.scope);
        while let Some(id) = s {
            if Some(id) == target {
                break;
            }
            if sem.scope(id).kind == ScopeKind::Function {
                m.entry(sem.scope(id).node)
                    .or_default()
                    .push(RefId(i as u32));
            }
            s = sem.scope(id).parent;
        }
    }
    m
}

fn var_scope(sem: &dyn Semantic, mut s: ScopeId) -> ScopeId {
    for _ in 0..100_000 {
        let sc = sem.scope(s);
        if sc.kind.is_var_scope() {
            return s;
        }
        match sc.parent {
            Some(p) => s = p,
            None => return s,
        }
    }
    s
}

impl Rule for NoLoopFunc {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let sem = cx.semantic();
        let ast = cx.ast();
        let mut fns: Vec<NodeId> = sem
            .scopes()
            .iter()
            .map(|s| s.node)
            .filter(|&n| {
                matches!(
                    cx.node(n),
                    Node::FunctionDecl { body: Some(_), .. }
                        | Node::FunctionExpr { body: Some(_), .. }
                        | Node::Arrow { .. }
                )
            })
            .collect();
        fns.dedup();
        let mut hits: Vec<(NodeId, String)> = Vec::new();
        {
            let mut w = Walk {
                cx,
                skipped: FxHashMap::default(),
                globals: None,
                through: through_map(sem),
            };
            for f in fns {
                let Some(lp) = w.containing_loop(f, 0) else {
                    continue;
                };
                if w.is_skipped(f, 0) {
                    continue;
                }
                let mut names: Vec<&str> = Vec::new();
                for r in w.through(f) {
                    if r.resolution == Resolution::Unresolved || w.is_safe(lp, r) {
                        continue;
                    }
                    let n = ast.atom(r.name);
                    if !names.contains(&n) {
                        names.push(n);
                    }
                }
                if !names.is_empty() {
                    hits.push((f, format!("'{}'", names.join("', '"))));
                }
            }
        }
        for (f, names) in hits {
            cx.report_node(
                f,
                format!(
                    "Function declared in a loop contains unsafe references to variable(s) {names}."
                ),
            )
            .id("unsafeRefs");
        }
    }
}
