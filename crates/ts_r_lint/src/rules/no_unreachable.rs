//! `no-unreachable`: disallow unreachable code after `return`, `throw`, `continue`, and
//! `break` statements.

use super::prelude::*;
use crate::cfg::Cfg;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-unreachable",
        "Disallow unreachable code after `return`, `throw`, `continue`, and `break` statements",
        ESLINT_REC,
        Category::Correctness,
    ),
    create: create_default::<NoUnreachable>,
};

#[derive(Debug, Default)]
struct NoUnreachable;

/// Statements that are never reported (hoisted or without runtime effect).
fn ignored(ast: &ts_r_ast::Ast, id: NodeId) -> bool {
    match *ast.node(id) {
        Node::FunctionDecl { .. }
        | Node::Empty {}
        | Node::Directive { .. }
        | Node::ImportDecl { .. }
        | Node::ExportNamed { .. }
        | Node::ExportAll { .. }
        | Node::Interface { .. }
        | Node::TypeAlias { .. }
        | Node::ImportEquals { .. } => true,
        Node::Module { modifiers, .. }
        | Node::Enum { modifiers, .. }
        | Node::ClassDecl { modifiers, .. } => modifiers.contains(Modifiers::DECLARE),
        Node::VarDecl {
            decls, modifiers, ..
        } => {
            modifiers.contains(Modifiers::DECLARE)
                || ast
                    .list(decls)
                    .iter()
                    .all(|&d| matches!(ast.node(d), Node::VarDeclarator { init: None, .. }))
        }
        _ => false,
    }
}

/// Merges spans separated only by whitespace and comments.
fn merge(cx: &Ctx<'_, '_>, mut spans: Vec<Span>) -> Vec<Span> {
    spans.sort_by_key(|s| (s.start, std::cmp::Reverse(s.end)));
    let src = cx.src();
    let mut out: Vec<Span> = Vec::new();
    for s in spans {
        if let Some(last) = out.last_mut() {
            if s.start < last.end {
                last.end = last.end.max(s.end);
                continue;
            }
            if skip_trivia_forward(src, last.end as usize) >= s.start as usize {
                last.end = s.end;
                continue;
            }
        }
        out.push(s);
    }
    out
}

fn has_super_call(ast: &ts_r_ast::Ast, cfg: &Cfg) -> bool {
    cfg.blocks.iter().enumerate().any(|(i, b)| {
        cfg.is_reachable(i as u32)
            && b.events.iter().any(|&e| {
                matches!(*ast.node(e), Node::Call { callee, .. } if matches!(ast.node(callee), Node::Super {}))
            })
    })
}

impl NoUnreachable {
    fn check_body(cx: &mut Ctx<'_, '_>, cfg: &Cfg, out: &mut Vec<Span>) {
        let ast = cx.ast();
        for (node, reachable) in cfg.event_reachability() {
            if !reachable && ast.node(node).is_statement() && !ignored(ast, node) {
                out.push(cx.span(node));
            }
        }
    }
}

impl Rule for NoUnreachable {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let mut spans = Vec::new();
        for i in 0..ast.nodes.len() {
            let Some(id) = NodeId::from_raw(i as u32) else {
                continue;
            };
            let cfg = match *ast.node(id) {
                Node::Program { stmts } => Cfg::statements(ast, ast.list(stmts)),
                Node::StaticBlock { body } => Cfg::statements(ast, ast.list(body)),
                Node::FunctionDecl { body: Some(_), .. }
                | Node::FunctionExpr { body: Some(_), .. }
                | Node::Arrow { .. } => Cfg::function(ast, id),
                Node::ClassDecl {
                    heritage, members, ..
                }
                | Node::ClassExpr {
                    heritage, members, ..
                } => {
                    // Instance fields are initialized by `super()`; without a reachable call
                    // in an explicit constructor they never run.
                    let extends = ast.list(heritage).iter().any(|&h| {
                        matches!(
                            ast.node(h),
                            Node::HeritageClause {
                                kind: ts_r_ast::HeritageKind::Extends,
                                ..
                            }
                        )
                    });
                    let members = ast.list(members);
                    let ctor = members.iter().find_map(|&m| match *ast.node(m) {
                        Node::MethodDef {
                            kind: MethodKind::Constructor,
                            value,
                            ..
                        } => Some(value),
                        _ => None,
                    });
                    if extends
                        && let Some(ctor) = ctor
                        && matches!(ast.node(ctor), Node::FunctionExpr { body: Some(_), .. })
                        && !has_super_call(ast, &Cfg::function(ast, ctor))
                    {
                        let mut fields = Vec::new();
                        for &m in members {
                            if let Node::PropertyDef { modifiers, .. } = *ast.node(m)
                                && !modifiers.intersects(Modifiers::STATIC | Modifiers::DECLARE)
                            {
                                fields.push(cx.span(m));
                            }
                        }
                        spans.extend(
                            merge(cx, fields)
                                .into_iter()
                                .map(|s| (s, true))
                                .map(|x| x.0),
                        );
                    }
                    continue;
                }
                _ => continue,
            };
            let mut found = Vec::new();
            Self::check_body(cx, &cfg, &mut found);
            spans.extend(merge(cx, found));
        }
        spans.sort_by_key(|s| s.start);
        spans.dedup();
        for s in spans {
            cx.report(s, "Unreachable code.").id("unreachableCode");
        }
    }
}
