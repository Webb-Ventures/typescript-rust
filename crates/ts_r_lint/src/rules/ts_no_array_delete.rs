//! `@typescript-eslint/no-array-delete`: disallow `delete` on array elements.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-array-delete",
        "Disallow using the `delete` operator on array values",
        TS_REC_TC,
        Category::Correctness,
    )
    .typed()
    .suggestions(),
    create: create_default::<NoArrayDelete>,
};

#[derive(Debug, Default)]
struct NoArrayDelete;

impl Rule for NoArrayDelete {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Unary]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Node::Unary {
            op: UnaryOp::Delete,
            arg,
        } = *cx.node(node)
        else {
            return;
        };
        let ast = cx.ast();
        let Node::Index { obj, index, .. } = *cx.node(skip_parens(ast, arg)) else {
            return;
        };
        let ti = cx.types();
        let Some(t) = constrained_type_at(ti, obj) else {
            return;
        };
        if !is_array_value(ti, t) {
            return;
        }
        let src = cx.src();
        let obj_text = cx.span(skip_parens(ast, obj)).text(src);
        let inner = skip_parens(ast, index);
        let index_text = cx.span(inner).text(src);
        let index_text = if matches!(cx.node(inner), Node::Seq { .. }) {
            format!("({index_text})")
        } else {
            index_text.to_string()
        };
        // Comments that would be lost move in front, one per line.
        let (obj_span, inner_span) = (cx.span(skip_parens(ast, obj)), cx.span(inner));
        let node_span = cx.span(node);
        let line_start = src[..node_span.start as usize]
            .rfind('\n')
            .map_or(0, |i| i + 1);
        let indent: String = src[line_start..node_span.start as usize]
            .chars()
            .take_while(|c| c.is_whitespace())
            .collect();
        let mut text = String::new();
        for c in ast.comments_in(node_span) {
            let inside = |s: Span| c.span.start >= s.start && c.span.end <= s.end;
            if c.span.end > node_span.end || inside(obj_span) || inside(inner_span) {
                continue;
            }
            text.push_str(c.span.text(src));
            text.push('\n');
            text.push_str(&indent);
        }
        text.push_str(&format!("{obj_text}.splice({index_text}, 1)"));
        let fix = Fix::replace(node_span, text);
        cx.report_node(
            node,
            "Using the `delete` operator with an array expression is unsafe.",
        )
        .id("noArrayDelete")
        .suggest("useSplice", "Use `array.splice()` instead.", fix);
    }
}

/// Every union member is an array or tuple (or an intersection with one).
fn is_array_value(ti: &dyn TypeInfo, t: Ty) -> bool {
    if t == Ty::UNRESOLVED || is_any(ti, t) {
        return false;
    }
    let ps = parts(ti, t);
    !ps.is_empty()
        && ps.iter().all(|&p| {
            let p = constrained(ti, p);
            let members = if ti.flags(p).contains(TypeFlags::INTERSECTION) {
                ti.intersection_members(p)
            } else {
                vec![p]
            };
            members
                .into_iter()
                .any(|m| m != Ty::UNRESOLVED && (ti.is_array(m) || ti.is_tuple(m)))
        })
}
