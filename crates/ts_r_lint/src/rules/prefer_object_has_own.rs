//! `prefer-object-has-own`: `Object.prototype.hasOwnProperty.call(o, k)` -> `Object.hasOwn(o, k)`.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "prefer-object-has-own",
        "Disallow use of `Object.prototype.hasOwnProperty.call()` and prefer use of `Object.hasOwn()`",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<PreferObjectHasOwn>,
};

#[derive(Debug, Default)]
struct PreferObjectHasOwn;

impl Rule for PreferObjectHasOwn {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let src = cx.src();
        let Node::Call { callee, .. } = *ast.node(node) else {
            return;
        };
        let callee = skip_parens(ast, callee);
        let Some((has_own, call)) = member_parts(ast, src, callee) else {
            return;
        };
        let Some((base, has_own_name)) = member_parts(ast, src, has_own) else {
            return;
        };
        if call != "call" || has_own_name != "hasOwnProperty" {
            return;
        }
        // `{}`, `Object` or `Object.prototype` before `.hasOwnProperty`.
        let base = skip_parens(ast, base);
        let object = match *ast.node(base) {
            Node::ObjectLit { props, .. } => ast.list(props).is_empty(),
            _ => {
                let target = match member_parts(ast, src, base) {
                    Some((obj, name)) if name == "prototype" => skip_parens(ast, obj),
                    _ => base,
                };
                ident_name(ast, target) == Some("Object")
            }
        };
        if !object || !cx.global_available(node, "Object") {
            return;
        }
        let callee_span = ast.span(callee);
        let fix = ast.comments_in(callee_span).is_empty().then(|| {
            Fix::replace(
                callee_span,
                space_before(src, callee_span.start, "Object.hasOwn".to_owned()),
            )
        });
        let span = cx.span(node);
        cx.report(
            span,
            "Use 'Object.hasOwn()' instead of 'Object.prototype.hasOwnProperty.call()'.",
        )
        .id("useHasOwn")
        .fix_opt(fix);
    }
}
