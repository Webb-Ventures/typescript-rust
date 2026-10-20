//! `@typescript-eslint/no-unnecessary-type-constraint`: disallow unnecessary constraints
//! on generic types.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-unnecessary-type-constraint",
        "Disallow unnecessary constraints on generic types",
        TS_REC,
        Category::Suspicious,
    )
    .suggestions(),
    create: create_default::<NoUnnecessaryConstraint>,
};

#[derive(Debug, Default)]
struct NoUnnecessaryConstraint;

impl Rule for NoUnnecessaryConstraint {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::TypeParam]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Node::TypeParam {
            name,
            constraint: Some(c),
            default,
            ..
        } = *cx.node(node)
        else {
            return;
        };
        let kw = match *cx.node(c) {
            Node::KeywordType {
                kind: KeywordTypeKind::Any,
            } => "any",
            Node::KeywordType {
                kind: KeywordTypeKind::Unknown,
            } => "unknown",
            _ => return,
        };
        let name_end = cx.span(name).end;
        let mut fix = Fix::delete(Span::new(name_end, cx.span(c).end));
        // `<T,>` keeps a single arrow type parameter from reading as JSX.
        let path = cx.file.path;
        let jsx_like = [".tsx", ".mts", ".cts"].iter().any(|e| path.ends_with(e));
        if jsx_like {
            let list = cx.parent(node);
            let is_arrow = list
                .and_then(|l| cx.parent(l))
                .is_some_and(|f| matches!(cx.node(f), Node::Arrow { .. }));
            let single = list.is_some_and(
                |l| matches!(*cx.node(l), Node::TypeParams { params } if cx.ast().list(params).len() == 1),
            );
            let after = skip_trivia_forward(cx.src(), cx.span(node).end as usize);
            if is_arrow
                && single
                && default.is_none()
                && cx.src().as_bytes().get(after) != Some(&b',')
            {
                fix = fix.and(Span::new(cx.span(node).end, cx.span(node).end), ",");
            }
        }
        let tname = cx.text(name);
        cx.report_node(
            node,
            format!(
                "Constraining the generic type `{tname}` to `{kw}` does nothing and is unnecessary."
            ),
        )
        .id("unnecessaryConstraint")
        .suggest(
            "removeUnnecessaryConstraint",
            format!("Remove the unnecessary `{kw}` constraint."),
            fix,
        );
    }
}
