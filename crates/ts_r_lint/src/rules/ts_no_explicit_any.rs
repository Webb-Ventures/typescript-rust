//! `@typescript-eslint/no-explicit-any`: disallow the `any` type.

use super::prelude::*;
use ts_r_ast::TypeOperatorKind;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-explicit-any",
        "Disallow the `any` type",
        TS_REC,
        Category::Restriction,
    )
    .fixable()
    .suggestions(),
    create: create::<NoExplicitAny>,
};

#[derive(Debug)]
struct NoExplicitAny {
    fix_to_unknown: bool,
    ignore_rest_args: bool,
}

impl FromOptions for NoExplicitAny {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(NoExplicitAny {
            fix_to_unknown: bool_field(o, "fixToUnknown", false),
            ignore_rest_args: bool_field(o, "ignoreRestArgs", false),
        })
    }
}

fn is_rest_param(cx: &Ctx<'_, '_>, id: Option<NodeId>, ty: NodeId) -> bool {
    id.is_some_and(
        |p| matches!(*cx.node(p), Node::Param { rest: true, type_ann: Some(t), .. } if t == ty),
    )
}

/// `any` as the element of a rest parameter's array type (`...a: any[]`,
/// `readonly any[]`, `Array<any>`, `ReadonlyArray<any>`).
fn is_rest_element(cx: &Ctx<'_, '_>, any: NodeId) -> bool {
    let Some(p) = cx.parent(any) else {
        return false;
    };
    let array = match *cx.node(p) {
        Node::ArrayType { .. } => p,
        Node::TypeArgs { .. } => {
            let Some(r) = cx.parent(p) else {
                return false;
            };
            match *cx.node(r) {
                Node::TypeRef { name, .. }
                    if matches!(cx.text(name), "Array" | "ReadonlyArray") =>
                {
                    r
                }
                _ => return false,
            }
        }
        _ => return false,
    };
    let mut ty = array;
    let mut up = cx.parent(array);
    if let Some(u) = up
        && matches!(
            *cx.node(u),
            Node::TypeOperator {
                op: TypeOperatorKind::Readonly,
                ..
            }
        )
    {
        ty = u;
        up = cx.parent(u);
    }
    is_rest_param(cx, up, ty)
}

impl Rule for NoExplicitAny {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::KeywordType]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        if !matches!(
            *cx.node(node),
            Node::KeywordType {
                kind: KeywordTypeKind::Any
            }
        ) {
            return;
        }
        if self.ignore_rest_args && is_rest_element(cx, node) {
            return;
        }
        let span = cx.span(node);
        let parent = cx.parent(node);
        const MSG: &str = "Unexpected any. Specify a different type.";
        if let Some(p) = parent
            && matches!(
                *cx.node(p),
                Node::TypeOperator {
                    op: TypeOperatorKind::KeyOf,
                    ..
                }
            )
        {
            let fix = Fix::replace(cx.span(p), "PropertyKey");
            let r = cx.report(span, MSG).id("unexpectedAny");
            let r = if self.fix_to_unknown {
                r.fix(fix.clone())
            } else {
                r
            };
            let _ = r.suggest(
                "suggestPropertyKey",
                "Use `PropertyKey` instead, this is more explicit than `keyof any`.",
                fix,
            );
            return;
        }
        let unknown = if is_rest_param(cx, parent, node) {
            "unknown[]"
        } else {
            "unknown"
        };
        let r = cx.report(span, MSG).id("unexpectedAny");
        let r = if self.fix_to_unknown {
            r.fix(Fix::replace(span, unknown))
        } else {
            r
        };
        let _ = r
            .suggest(
                "suggestUnknown",
                "Use `unknown` instead, this will force you to explicitly, and safely assert the type is correct.",
                Fix::replace(span, unknown),
            )
            .suggest(
                "suggestNever",
                "Use `never` instead, this is useful when instantiating generic type parameters that you don't need to know the type of.",
                Fix::replace(span, "never"),
            );
    }
}
