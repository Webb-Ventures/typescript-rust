//! `no-undef-init`: disallow initializing a variable to `undefined`.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-undef-init",
        "Disallow initializing variables to `undefined`",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<NoUndefInit>,
};

#[derive(Debug, Default)]
struct NoUndefInit;

impl Rule for NoUndefInit {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::VarDeclarator]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Node::VarDeclarator {
            name,
            type_ann,
            init: Some(init),
            ..
        } = *cx.node(node)
        else {
            return;
        };
        let ast = cx.ast();
        let Some(kind) = cx.parent(node).and_then(|p| match *ast.node(p) {
            Node::VarDecl { kind, .. } => Some(kind),
            _ => None,
        }) else {
            return;
        };
        if !matches!(kind, VarKind::Var | VarKind::Let) {
            return;
        }
        let value = skip_parens(ast, init);
        if !is_undefined(ast, value) || cx.is_shadowed(value, "undefined") {
            return;
        }
        let id_end = type_ann.map_or(cx.span(name).end, |t| cx.span(t).end);
        let span = cx.span(node);
        let fix = (kind == VarKind::Let
            && matches!(ast.node(name), Node::Ident { .. })
            && !ast
                .comments
                .iter()
                .any(|c| c.span.start >= id_end && c.span.end <= cx.span(init).start))
        .then(|| Fix::delete(Span::new(id_end, span.end)));
        let id_text = Span::new(cx.span(name).start, id_end).text(cx.src());
        cx.report(
            span,
            format!("It's not necessary to initialize '{id_text}' to undefined."),
        )
        .id("unnecessaryUndefinedInit")
        .fix_opt(fix);
    }
}
