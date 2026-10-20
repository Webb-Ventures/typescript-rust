//! `prefer-const`: require `const` declarations for variables that are never reassigned
//! after declared.

use rustc_hash::{FxHashMap, FxHashSet};

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "prefer-const",
        "Require `const` declarations for variables that are never reassigned after declared",
        TS_ESLINT_REC,
        Category::Style,
    )
    .fixable(),
    create: create::<PreferConst>,
};

#[derive(Debug, Default)]
struct PreferConst {
    /// `destructuring: "all"`.
    all: bool,
    ignore_read_before_assign: bool,
}

impl FromOptions for PreferConst {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(PreferConst {
            all: str_field(o, "destructuring") == Some("all"),
            ignore_read_before_assign: bool_field(o, "ignoreReadBeforeAssign", false),
        })
    }
}

/// Where a variable's single write happens.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Host {
    /// A declarator with a destructuring pattern.
    Declarator(NodeId),
    /// A destructuring assignment (the outermost `Assign`).
    Assign(NodeId),
    /// A plain binding.
    Single(SymbolId),
}

impl PreferConst {
    /// The identifier to report when `sym` should be `const`, and where it is written.
    fn check_symbol(&self, cx: &Ctx<'_, '_>, sym: SymbolId) -> Option<(NodeId, Host)> {
        let ast = cx.ast();
        let sem = cx.semantic();
        let s = sem.symbol(sym);
        let decl = s.decls.first()?;
        if decl.kind != DeclKind::Let || s.decls.len() != 1 {
            return None;
        }
        if sem.scope(s.scope).kind == ScopeKind::Global
            && cx.file.exported.iter().any(|e| e == ast.atom(s.name))
        {
            return None;
        }
        // `for (let i = 0; ...)` initializers never qualify.
        if let Some(p) = cx.parent(decl.parent)
            && let Node::For { init: Some(i), .. } = *ast.node(p)
            && i == decl.parent
        {
            return None;
        }
        let mut writer = None;
        let mut read_before = false;
        for &r in &s.refs {
            let r = sem.reference(r);
            if r.is_type() {
                continue;
            }
            if r.is_write() {
                if writer.is_some() {
                    return None;
                }
                writer = Some(r);
            } else if r.is_read() && writer.is_none() {
                if self.ignore_read_before_assign {
                    return None;
                }
                read_before = true;
            }
        }
        let w = writer?;
        if w.scope != s.scope {
            return None;
        }
        let host = self.writer_host(cx, w.node, sym)?;
        if let Host::Assign(assign) = host {
            // Every target must be a `let` of the same scope, and no member targets.
            let Node::Assign { left, .. } = *ast.node(assign) else {
                return None;
            };
            let mut targets = Vec::new();
            assign_targets(ast, left, &mut targets);
            for t in targets {
                if !matches!(ast.node(t), Node::Ident { .. }) {
                    return None;
                }
                // Undeclared (implicit global) targets do not matter.
                let Some(ts) = sem.symbol_of_ident(t) else {
                    continue;
                };
                let tsym = sem.symbol(ts);
                if tsym.scope != w.scope
                    || tsym.decls.first().map(|d| d.kind) != Some(DeclKind::Let)
                {
                    return None;
                }
            }
        }
        let at = if read_before { decl.name } else { w.node };
        Some((at, host))
    }

    /// Classifies the write at `ident`; `None` when it cannot become a declaration.
    fn writer_host(&self, cx: &Ctx<'_, '_>, ident: NodeId, sym: SymbolId) -> Option<Host> {
        let ast = cx.ast();
        let mut cur = ident;
        let mut in_pattern = false;
        while let Some(p) = cx.parent(cur) {
            match *ast.node(p) {
                Node::VarDeclarator { name, .. } => {
                    if name != cur {
                        return None;
                    }
                    return Some(if in_pattern {
                        Host::Declarator(p)
                    } else {
                        Host::Single(sym)
                    });
                }
                Node::ObjectPattern { .. }
                | Node::ArrayPattern { .. }
                | Node::Rest { .. }
                | Node::ArrayLit { .. }
                | Node::ObjectLit { .. }
                | Node::Spread { .. } => in_pattern = true,
                Node::BindingProperty { key, value, .. } => {
                    if value.is_some() && key == cur {
                        return None;
                    }
                    in_pattern = true;
                }
                Node::Property { value, .. } => {
                    if value != cur {
                        return None;
                    }
                    in_pattern = true;
                }
                Node::ShorthandProp { name, .. } => {
                    if name != cur {
                        return None;
                    }
                    in_pattern = true;
                }
                Node::AssignPattern { left, .. } => {
                    if left != cur {
                        return None;
                    }
                }
                Node::Paren { .. } => {}
                Node::Assign { left, op, .. } => {
                    if left != cur || op != AssignOp::Assign {
                        return None;
                    }
                    let mut top = p;
                    // A default inside an outer pattern: keep climbing.
                    if let Some(pp) = cx.parent(p)
                        && matches!(
                            ast.node(pp),
                            Node::ArrayLit { .. } | Node::Property { .. } | Node::ObjectLit { .. }
                        )
                    {
                        cur = p;
                        continue;
                    }
                    while let Some(pp) = cx.parent(top) {
                        if matches!(ast.node(pp), Node::Paren { .. }) {
                            top = pp;
                        } else {
                            break;
                        }
                    }
                    let stmt = cx.parent(top)?;
                    if !matches!(ast.node(stmt), Node::ExprStmt { .. }) {
                        return None;
                    }
                    // Only a statement in a statement list can become a declaration.
                    let list = cx.parent(stmt)?;
                    if !matches!(
                        ast.node(list),
                        Node::Block { .. }
                            | Node::Program { .. }
                            | Node::StaticBlock { .. }
                            | Node::SwitchCase { .. }
                            | Node::ModuleBlock { .. }
                    ) {
                        return None;
                    }
                    return Some(if in_pattern {
                        Host::Assign(p)
                    } else {
                        Host::Single(sym)
                    });
                }
                _ => return None,
            }
            cur = p;
        }
        None
    }
}

impl Rule for PreferConst {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let sem = cx.semantic();
        // Group candidates by their write host.
        let mut groups: FxHashMap<Host, Vec<Option<NodeId>>> = FxHashMap::default();
        let mut order: Vec<Host> = Vec::new();
        for (i, sym) in sem.symbols().iter().enumerate() {
            if sym.decls.first().map(|d| d.kind) != Some(DeclKind::Let) {
                continue;
            }
            let id = SymbolId(i as u32);
            let result = self.check_symbol(cx, id);
            // Which group does this symbol belong to?
            let host = match result {
                Some((_, h)) => h,
                None => {
                    // A failing member of a destructuring declaration still counts for `all`.
                    let decl = sym.decls[0];
                    let Node::VarDeclarator { name, .. } = *ast.node(decl.node) else {
                        continue;
                    };
                    if matches!(ast.node(name), Node::Ident { .. }) {
                        continue;
                    }
                    Host::Declarator(decl.node)
                }
            };
            if !groups.contains_key(&host) {
                order.push(host);
            }
            groups.entry(host).or_default().push(result.map(|r| r.0));
        }
        // Destructuring assignments: targets that are not candidates count as failures.
        let mut reported: Vec<NodeId> = Vec::new();
        for host in order {
            let nodes = &groups[&host];
            let mut total = nodes.len();
            if let Host::Assign(a) = host
                && let Node::Assign { left, .. } = *ast.node(a)
            {
                let mut t = Vec::new();
                assign_targets(ast, left, &mut t);
                total = total.max(t.len());
            }
            let ok: Vec<NodeId> = nodes.iter().flatten().copied().collect();
            if ok.is_empty() || (self.all && ok.len() != total) {
                continue;
            }
            reported.extend(ok);
        }
        reported.sort_by_key(|n| cx.span(*n).start);
        reported.dedup();
        // Fixable declarations: every binding reported and every declarator initialized.
        let reported_syms: FxHashSet<SymbolId> = reported
            .iter()
            .filter_map(|&n| sem.symbol_of_ident(n))
            .collect();
        let mut fixes: FxHashMap<NodeId, Option<Fix>> = FxHashMap::default();
        for &n in &reported {
            let Some(sym) = sem.symbol_of_ident(n) else {
                continue;
            };
            let decl = sem.symbol(sym).decls[0].parent;
            fixes.entry(decl).or_insert_with(|| {
                let Node::VarDecl { decls, .. } = *ast.node(decl) else {
                    return None;
                };
                let for_in_of = cx.parent(decl).is_some_and(|p| {
                    matches!(ast.node(p), Node::ForIn { .. } | Node::ForOf { .. })
                });
                for &d in ast.list(decls) {
                    let Node::VarDeclarator { name, init, .. } = *ast.node(d) else {
                        return None;
                    };
                    if init.is_none() && !for_in_of {
                        return None;
                    }
                    let mut ids = Vec::new();
                    binding_idents(ast, name, &mut ids);
                    for id in ids {
                        let s = sem.symbol_of_decl(id)?;
                        if !reported_syms.contains(&s) {
                            return None;
                        }
                        // The reported node must be the declaration itself.
                        if !reported.contains(&id) {
                            return None;
                        }
                    }
                }
                let span = cx.span(decl);
                let kw = decl_keyword_start(cx.src(), span);
                let text = span.text(cx.src());
                let off = (kw - span.start) as usize;
                Some(Fix::replace(
                    span,
                    format!("{}const{}", &text[..off], &text[off + 3..]),
                ))
            });
        }
        for n in reported {
            let fix = sem
                .symbol_of_ident(n)
                .and_then(|s| fixes.get(&sem.symbol(s).decls[0].parent).cloned().flatten());
            let name = ident_name(ast, n).unwrap_or("").to_owned();
            cx.report_node(
                n,
                format!("'{name}' is never reassigned. Use 'const' instead."),
            )
            .id("useConst")
            .fix_opt(fix);
        }
    }
}
