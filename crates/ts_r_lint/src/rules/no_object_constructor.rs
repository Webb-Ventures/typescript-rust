//! `no-object-constructor`: disallow `Object()` / `new Object()` without arguments.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-object-constructor",
        "Disallow calls to the `Object` constructor without an argument",
        NO_PRESET,
        Category::Style,
    )
    .suggestions(),
    create: create_default::<NoObjectConstructor>,
};

#[derive(Debug, Default)]
struct NoObjectConstructor;

impl Rule for NoObjectConstructor {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call, NodeKind::New]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let (Node::Call { callee, args, .. } | Node::New { callee, args, .. }) = *ast.node(node)
        else {
            return;
        };
        let inner = skip_parens(ast, callee);
        if !ast.list(args).is_empty()
            || ident_name(ast, inner) != Some("Object")
            || !cx.is_global_ref(inner, "Object")
        {
            return;
        }
        let parents = cx.file.parents;
        let start = expr_start(ast, parents, node);
        let parens = matches!(start, Some(ExprStart::Statement | ExprStart::ArrowBody));
        let semi = parens
            && start == Some(ExprStart::Statement)
            && ancestors_inclusive(parents, node)
                .find_map(|a| match *ast.node(a) {
                    Node::ExprStmt { expr } => Some(expr),
                    _ => None,
                })
                .is_some_and(|e| needs_leading_semicolon(ast, cx.src(), parents, e));
        let (text, id, desc) = match (parens, semi) {
            (true, true) => (
                ";({})",
                "useLiteralAfterSemicolon",
                "Replace with '({})', add preceding semicolon.",
            ),
            (true, false) => ("({})", "useLiteral", "Replace with '({})'."),
            _ => ("{}", "useLiteral", "Replace with '{}'."),
        };
        let span = cx.span(node);
        cx.report(span, "The object literal notation {} is preferable.")
            .id("preferLiteral")
            .suggest(id, desc, Fix::replace(span, text));
    }
}
