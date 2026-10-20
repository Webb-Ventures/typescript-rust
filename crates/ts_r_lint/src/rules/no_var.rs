//! `no-var`: require `let` or `const` instead of `var`.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-var",
        "Require `let` or `const` instead of `var`",
        TS_ESLINT_REC,
        Category::Style,
    )
    .fixable(),
    create: create_default::<NoVar>,
};

#[derive(Debug, Default)]
struct NoVar;

fn is_loop(k: NodeKind) -> bool {
    matches!(
        k,
        NodeKind::For | NodeKind::ForIn | NodeKind::ForOf | NodeKind::While | NodeKind::DoWhile
    )
}

fn is_scope_boundary(k: NodeKind) -> bool {
    is_function(k) || matches!(k, NodeKind::StaticBlock | NodeKind::Program)
}

impl NoVar {
    /// True when the reference `at` may run before the binding `ident` of `declarator` is
    /// initialized.
    fn in_tdz(cx: &Ctx<'_, '_>, at: NodeId, ident: NodeId, declarator: NodeId, depth: u32) -> bool {
        if depth > 8 {
            return true;
        }
        let ast = cx.ast();
        let pos = cx.span(at).start;
        if pos < cx.span(ident).start {
            return true;
        }
        if let Node::VarDeclarator {
            init: Some(init), ..
        } = *ast.node(declarator)
            && cx.span(init).contains_span(cx.span(at))
            && !is_function(ast.kind(skip_parens(ast, init)))
        {
            return true;
        }
        // The default value of the binding itself (`{ a = a }`).
        if let Some(p) = cx.parent(ident) {
            let default = match *ast.node(p) {
                Node::AssignPattern { left, right } if left == ident => Some(right),
                Node::BindingProperty {
                    key,
                    value: None,
                    init: Some(i),
                } if key == ident => Some(i),
                _ => None,
            };
            if default.is_some_and(|d| cx.span(d).contains_span(cx.span(at))) {
                return true;
            }
        }
        // References inside a function declaration run when it is called.
        let sem = cx.semantic();
        let decl_span = cx.span(declarator);
        for a in ancestors_inclusive(cx.file.parents, at) {
            if cx.span(a).contains_span(decl_span) {
                break;
            }
            if let Node::FunctionDecl {
                name: Some(fname), ..
            } = *ast.node(a)
                && let Some(fs) = sem.symbol_of_decl(fname)
            {
                for &r in &sem.symbol(fs).refs {
                    let r = sem.reference(r);
                    if !cx.span(a).contains_span(cx.span(r.node))
                        && Self::in_tdz(cx, r.node, ident, declarator, depth + 1)
                    {
                        return true;
                    }
                }
            }
        }
        false
    }

    fn can_fix(cx: &Ctx<'_, '_>, decl: NodeId) -> bool {
        let ast = cx.ast();
        let sem = cx.semantic();
        let Node::VarDecl { decls, .. } = *ast.node(decl) else {
            return false;
        };
        let Some(parent) = cx.parent(decl) else {
            return false;
        };
        let head_of = match *ast.node(parent) {
            Node::For { init, .. } => init == Some(decl),
            Node::ForIn { left, .. } | Node::ForOf { left, .. } => left == decl,
            _ => false,
        };
        let container = if head_of
            || matches!(
                ast.node(parent),
                Node::Block { .. }
                    | Node::Program { .. }
                    | Node::StaticBlock { .. }
                    | Node::ModuleBlock { .. }
            ) {
            parent
        } else {
            return false;
        };
        let for_in_of =
            head_of && matches!(ast.node(parent), Node::ForIn { .. } | Node::ForOf { .. });
        // Inside a loop (up to the function boundary)?
        let mut loop_node = None;
        for a in ancestors_inclusive(cx.file.parents, decl) {
            let k = ast.kind(a);
            if is_scope_boundary(k) {
                break;
            }
            if is_loop(k) && a != decl {
                loop_node = Some(a);
                break;
            }
        }
        let declarators = ast.list(decls);
        if loop_node.is_some() && !for_in_of {
            let uninit = declarators
                .iter()
                .any(|&d| matches!(ast.node(d), Node::VarDeclarator { init: None, .. }));
            if uninit {
                return false;
            }
        }
        // Enclosing catch parameters with the same name (up to the function boundary).
        let mut catch_names: Vec<&str> = Vec::new();
        for a in ancestors_inclusive(cx.file.parents, decl) {
            let k = ast.kind(a);
            if is_scope_boundary(k) {
                break;
            }
            if let Node::CatchClause { param: Some(p), .. } = *ast.node(a) {
                let mut ids = Vec::new();
                binding_idents(ast, p, &mut ids);
                catch_names.extend(ids.iter().filter_map(|&i| ident_name(ast, i)));
            }
        }
        let container_span = cx.span(container);
        for &d in declarators {
            let Node::VarDeclarator { name, .. } = *ast.node(d) else {
                return false;
            };
            let mut ids = Vec::new();
            binding_idents(ast, name, &mut ids);
            for id in ids {
                let n = ident_name(ast, id).unwrap_or("");
                if n == "let" || catch_names.contains(&n) {
                    return false;
                }
                let Some(sym) = sem.symbol_of_decl(id) else {
                    return false;
                };
                let s = sem.symbol(sym);
                if s.decls.len() > 1 {
                    return false;
                }
                if sem.scope(s.scope).kind == ScopeKind::Global {
                    return false;
                }
                for &r in &s.refs {
                    let r = sem.reference(r);
                    if r.node == id
                        || r.flags.contains(RefFlags::INIT)
                            && cx.span(d).contains_span(cx.span(r.node))
                    {
                        continue;
                    }
                    let rs = cx.span(r.node);
                    if !container_span.contains_span(rs) {
                        return false;
                    }
                    if let Some(l) = loop_node.or(head_of.then_some(parent)) {
                        // Captured by a closure inside the loop.
                        let in_closure = ancestors_inclusive(cx.file.parents, r.node)
                            .take_while(|&a| a != l)
                            .any(|a| is_function(ast.kind(a)));
                        if in_closure && cx.span(l).contains_span(rs) {
                            return false;
                        }
                    }
                    if Self::in_tdz(cx, r.node, id, d, 0) {
                        return false;
                    }
                }
            }
        }
        true
    }
}

impl Rule for NoVar {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::VarDecl]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::VarDecl {
            modifiers,
            kind: VarKind::Var,
            ..
        } = *cx.node(node)
        else {
            return;
        };
        // `declare global { var x }` declares a global.
        let in_global_aug = ancestors_inclusive(cx.file.parents, node).any(|a| {
            matches!(
                ast.node(a),
                Node::Module {
                    kind: ModuleKind::Global,
                    ..
                }
            )
        });
        if in_global_aug {
            return;
        }
        let span = cx.span(node);
        let src = cx.src();
        let kw = decl_keyword_start(src, span);
        let start = if modifiers.contains(Modifiers::DECLARE) {
            src[span.start as usize..kw as usize]
                .find("declare")
                .map_or(kw, |o| span.start + o as u32)
        } else {
            kw
        };
        let fix = Self::can_fix(cx, node).then(|| Fix::replace(Span::new(kw, kw + 3), "let"));
        cx.report(
            Span::new(start, span.end),
            "Unexpected var, use let or const instead.",
        )
        .id("unexpectedVar")
        .fix_opt(fix);
    }
}
