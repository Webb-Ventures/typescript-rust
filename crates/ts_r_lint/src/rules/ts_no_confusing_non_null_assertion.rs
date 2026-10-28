//! `@typescript-eslint/no-confusing-non-null-assertion`: a `!` ending the left operand of
//! `=`, `==`, `===`, `in` or `instanceof` reads like `!=`, `!==` or a negation.
//!
//! - Reports the whole expression when the left operand's last token is `!` and the operand
//!   is not parenthesized (`(a!) == b` is fine). The `!` may belong to a nested operand
//!   (`a + b! == c`).
//! - When the left operand is itself the non-null assertion, a suggestion removes the `!`;
//!   for `in` / `instanceof` (and when the `!` is nested) a suggestion wraps the left
//!   operand in parentheses.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-confusing-non-null-assertion",
        "Disallow non-null assertion in locations that may be confusing",
        TS_STYLISTIC,
        Category::Suspicious,
    )
    .suggestions(),
    create: create_default::<NoConfusingNonNullAssertion>,
};

#[derive(Debug, Default)]
struct NoConfusingNonNullAssertion;

impl Rule for NoConfusingNonNullAssertion {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Binary, NodeKind::Assign]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let (left, op) = match *ast.node(node) {
            Node::Assign {
                left,
                op: AssignOp::Assign,
                ..
            } => (left, "="),
            Node::Binary { left, op, .. } => match op {
                BinaryOp::Eq => (left, "=="),
                BinaryOp::StrictEq => (left, "==="),
                BinaryOp::In => (left, "in"),
                BinaryOp::InstanceOf => (left, "instanceof"),
                _ => return,
            },
            _ => return,
        };
        // A parenthesized operand ends in `)` (and is followed by one, in ESLint's terms).
        let ls = cx.span(left);
        if ls.is_empty() || cx.src().as_bytes().get(ls.end as usize - 1) != Some(&b'!') {
            return;
        }
        let bang = Span::new(ls.end - 1, ls.end);
        let (id, msg) = match op {
            "=" => (
                "confusingAssign",
                "Confusing combination of non-null assertion and assignment like `a! = b`, which looks very similar to `a != b`.".to_owned(),
            ),
            "==" | "===" => (
                "confusingEqual",
                "Confusing combination of non-null assertion and equality test like `a! == b`, which looks very similar to `a !== b`.".to_owned(),
            ),
            _ => (
                "confusingOperator",
                format!(
                    "Confusing combination of non-null assertion and `{op}` operator like `a! {op} b`, which might be misinterpreted as `!(a {op} b)`."
                ),
            ),
        };
        let wrap = || {
            (
                format!(
                    "Wrap the left-hand side in parentheses to avoid confusion with \"{op}\" operator."
                ),
                Fix::insert(ls.start, "(").and(Span::new(ls.end, ls.end), ")"),
            )
        };
        let span = cx.span(node);
        let r = cx.report(span, msg).id(id);
        if matches!(ast.node(left), Node::NonNull { .. }) {
            match op {
                "=" => r.suggest(
                    "notNeedInAssign",
                    "Remove unnecessary non-null assertion (!) in assignment left-hand side.",
                    Fix::delete(bang),
                ),
                "==" | "===" => r.suggest(
                    "notNeedInEqualTest",
                    "Remove unnecessary non-null assertion (!) in equality test.",
                    Fix::delete(bang),
                ),
                _ => {
                    let (desc, fix) = wrap();
                    r.suggest(
                        "notNeedInOperator",
                        format!(
                            "Remove possibly unnecessary non-null assertion (!) in the left operand of the `{op}` operator."
                        ),
                        Fix::delete(bang),
                    )
                    .suggest("wrapUpLeft", desc, fix)
                }
            };
        } else {
            let (desc, fix) = wrap();
            r.suggest("wrapUpLeft", desc, fix);
        }
    }
}
