//! `@typescript-eslint/no-unsafe-call`: disallow calling a value typed `any` or `Function`.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-unsafe-call",
        "Disallow calling a value with type `any`",
        TS_REC_TC,
        Category::Suspicious,
    )
    .typed(),
    create: create_default::<NoUnsafeCall>,
};

#[derive(Debug, Default)]
struct NoUnsafeCall;

const THIS_HINT: &str = "\nYou can try to fix this by turning on the `noImplicitThis` compiler option, or adding a `this` parameter to the function.";

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Call,
    New,
    Tag,
}

impl Rule for NoUnsafeCall {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call, NodeKind::New, NodeKind::TaggedTemplate]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let (callee, kind) = match *cx.node(node) {
            Node::Call { callee, .. } => (callee, Kind::Call),
            Node::New { callee, .. } => (callee, Kind::New),
            Node::TaggedTemplate { tag, .. } => (tag, Kind::Tag),
            _ => return,
        };
        if matches!(cx.node(callee), Node::Super {} | Node::ImportCall { .. }) {
            return;
        }
        let ti = cx.types();
        let Some(t) = constrained_type_at(ti, callee) else {
            return;
        };
        let at = if kind == Kind::New { node } else { callee };
        let span = cx.span(at);
        if let Some(any) = any_kind(ti, t) {
            if kind == Kind::Call
                && let Some(this) = this_root(cx.ast(), callee)
                && let Some(this_ty) = ti.type_at(this)
                && let Some(this_any) = any_kind(ti, this_ty)
            {
                match this_any {
                    AnyKind::Error => {
                        cx.report(
                            span,
                            "Unsafe call of a `this` type that could not be resolved.",
                        )
                        .id("errorCallThis");
                    }
                    AnyKind::Any => {
                        cx.report(
                            span,
                            format!(
                                "Unsafe call of an `any` typed value. `this` is typed as an `any`.{THIS_HINT}"
                            ),
                        )
                        .id("unsafeCallThis");
                    }
                }
                return;
            }
            match (any, kind) {
                (AnyKind::Error, Kind::Call) => {
                    cx.report(span, "Unsafe call of a type that could not be resolved.")
                        .id("errorCall");
                }
                (AnyKind::Error, Kind::New) => {
                    cx.report(
                        span,
                        "Unsafe construction of a type that could not be resolved.",
                    )
                    .id("errorNew");
                }
                (AnyKind::Error, Kind::Tag) => {
                    cx.report(
                        span,
                        "Unsafe use of a template tag whose type could not be resolved.",
                    )
                    .id("errorTemplateTag");
                }
                (AnyKind::Any, k) => report(cx, span, k, "an `any`"),
            }
            return;
        }
        if is_unsafe_function_type(ti, t, kind == Kind::New) {
            report(cx, span, kind, "a `Function`");
        }
    }
}

fn report(cx: &mut Ctx<'_, '_>, span: Span, kind: Kind, ty: &str) {
    match kind {
        Kind::Call => {
            cx.report(span, format!("Unsafe call of {ty} typed value."))
                .id("unsafeCall");
        }
        Kind::New => {
            cx.report(span, format!("Unsafe construction of {ty} typed value."))
                .id("unsafeNew");
        }
        Kind::Tag => {
            cx.report(span, format!("Unsafe use of {ty} typed template tag."))
                .id("unsafeTemplateTag");
        }
    }
}
