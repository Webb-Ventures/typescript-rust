//! `no-dupe-args`: disallow duplicate arguments in function definitions.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-dupe-args",
        "Disallow duplicate arguments in `function` definitions",
        ESLINT_REC,
        Category::Correctness,
    ),
    create: create_default::<NoDupeArgs>,
};

#[derive(Debug, Default)]
struct NoDupeArgs;

fn collect_names(ast: &ts_r_ast::Ast, id: NodeId, out: &mut Vec<NodeId>) {
    match *ast.node(id) {
        Node::Ident { .. } => out.push(id),
        Node::Param { pattern, .. } => collect_names(ast, pattern, out),
        Node::ObjectPattern { props } => {
            for &p in ast.list(props) {
                collect_names(ast, p, out);
            }
        }
        Node::ArrayPattern { elements } => {
            for &p in ast.list(elements) {
                collect_names(ast, p, out);
            }
        }
        Node::BindingProperty { key, value, .. } => match value {
            Some(v) => collect_names(ast, v, out),
            None => collect_names(ast, key, out),
        },
        Node::AssignPattern { left, .. } => collect_names(ast, left, out),
        Node::Rest { arg } => collect_names(ast, arg, out),
        _ => {}
    }
}

impl Rule for NoDupeArgs {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::FunctionDecl, NodeKind::FunctionExpr]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let (Node::FunctionDecl { params, .. } | Node::FunctionExpr { params, .. }) =
            *cx.node(node)
        else {
            return;
        };
        let ps = ast.list(params);
        if ps.is_empty() {
            return;
        }
        let mut names = Vec::new();
        for &p in ps {
            collect_names(ast, p, &mut names);
        }
        let mut reported: Vec<&str> = Vec::new();
        let first = ast.span(ps[0]).start;
        let last = ast.span(ps[ps.len() - 1]).end;
        let open = cx.src()[..first as usize]
            .rfind('(')
            .map_or(first, |i| i as u32);
        let close = find_char(cx.src(), last, b')').map_or(last, |c| c + 1);
        for (i, &n) in names.iter().enumerate() {
            let Some(name) = ident_name(ast, n) else {
                continue;
            };
            if name == "this" || reported.contains(&name) {
                continue;
            }
            if names[..i].iter().any(|&m| ident_name(ast, m) == Some(name)) {
                reported.push(name);
                cx.report(Span::new(open, close), format!("Duplicate param '{name}'."))
                    .id("unexpected");
            }
        }
    }
}
