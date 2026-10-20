//! `@typescript-eslint/no-unnecessary-boolean-literal-compare`: disallow comparing boolean
//! values to `true`/`false`.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-unnecessary-boolean-literal-compare",
        "Disallow unnecessary equality comparisons against boolean literals",
        TS_STRICT_TC,
        Category::Style,
    )
    .typed()
    .fixable(),
    create: create::<NoUnnecessaryBooleanLiteralCompare>,
};

#[derive(Debug)]
struct NoUnnecessaryBooleanLiteralCompare {
    nullable_true: bool,
    nullable_false: bool,
}

impl FromOptions for NoUnnecessaryBooleanLiteralCompare {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(NoUnnecessaryBooleanLiteralCompare {
            nullable_true: bool_field(o, "allowComparingNullableBooleansToTrue", true),
            nullable_false: bool_field(o, "allowComparingNullableBooleansToFalse", true),
        })
    }
}

const DIRECT: &str = "This expression unnecessarily compares a boolean value to a boolean instead of using it directly.";
const NEGATED: &str =
    "This expression unnecessarily compares a boolean value to a boolean instead of negating it.";
const NULL_TRUE_DIRECT: &str = "This expression unnecessarily compares a nullable boolean value to true instead of using it directly.";
const NULL_TRUE_NEGATED: &str = "This expression unnecessarily compares a nullable boolean value to true instead of negating it.";
const NULL_FALSE: &str = "This expression unnecessarily compares a nullable boolean value to false instead of using the ?? operator to provide a default.";

impl Rule for NoUnnecessaryBooleanLiteralCompare {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Binary]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Node::Binary { left, op, right } = *cx.node(node) else {
            return;
        };
        let negated_op = match op {
            BinaryOp::StrictEq | BinaryOp::Eq => false,
            BinaryOp::StrictNotEq | BinaryOp::NotEq => true,
            _ => return,
        };
        let ast = cx.ast();
        let (literal, expr) = match (
            cx.node(skip_parens(ast, left)),
            cx.node(skip_parens(ast, right)),
        ) {
            (_, Node::BoolLit { value }) => (*value, left),
            (Node::BoolLit { value }, _) => (*value, right),
            _ => return,
        };
        let ti = cx.types();
        let Some(t) = constrained_type_at(ti, expr) else {
            return;
        };
        let mut boolean = false;
        let mut nullable = false;
        for p in parts(ti, t) {
            let p = constrained(ti, p);
            if p == Ty::UNRESOLVED {
                return;
            }
            let f = ti.flags(p);
            if f.intersects(TypeFlags::BOOLEAN | TypeFlags::BOOLEAN_LITERAL) {
                boolean = true;
            } else if f.intersects(TypeFlags::NULL | TypeFlags::UNDEFINED) {
                nullable = true;
            } else {
                return;
            }
        }
        if !boolean {
            return;
        }
        let src = cx.src();
        let text = cx.span(expr).text(src).to_string();
        let paren_expr = skip_parens(ast, expr) != expr;
        let low = !paren_expr && precedence(ast, expr) < 16;
        let wrapped = if low {
            format!("({text})")
        } else {
            text.clone()
        };
        // `!(cmp)`: the fix may replace the negation too.
        let (outer_not, paren) = outer_not(cx, node);
        let span = cx.span(node);

        if !nullable {
            let negate = literal == negated_op;
            let msg = if negated_op { NEGATED } else { DIRECT };
            let id = if negated_op { "negated" } else { "direct" };
            let fix = match (outer_not, negate) {
                (Some(not), true) => {
                    // The `!(..)` goes; drop the operand's own parentheses where the
                    // context allows.
                    let bare = cx.span(skip_parens(ast, expr)).text(src).to_string();
                    let t = if paren_expr && !needs_wrap(cx, not) {
                        bare
                    } else {
                        text
                    };
                    Fix::replace(cx.span(not), t)
                }
                (Some(_), false) => Fix::replace(cx.span(paren.unwrap_or(node)), wrapped),
                (None, true) => Fix::replace(span, format!("!{wrapped}")),
                (None, false) => Fix::replace(span, text),
            };
            cx.report_node(node, msg).id(id).fix(fix);
            return;
        }
        if literal {
            if self.nullable_true {
                return;
            }
            if negated_op {
                // `x !== true` is `!x`.
                let fix = match outer_not {
                    Some(not) => {
                        let value = format!("{text} ?? false");
                        Fix::replace(
                            cx.span(not),
                            if needs_wrap(cx, not) {
                                format!("({value})")
                            } else {
                                value
                            },
                        )
                    }
                    None => Fix::replace(span, format!("!{wrapped}")),
                };
                cx.report_node(node, NULL_TRUE_NEGATED)
                    .id("comparingNullableToTrueNegated")
                    .fix(fix);
            } else {
                let fix = match outer_not {
                    Some(_) => Fix::replace(cx.span(paren.unwrap_or(node)), wrapped),
                    None if boolean_context(cx, node) => Fix::replace(span, text),
                    None => {
                        let value = format!("{text} ?? false");
                        Fix::replace(
                            span,
                            if needs_wrap(cx, node) {
                                format!("({value})")
                            } else {
                                value
                            },
                        )
                    }
                };
                cx.report_node(node, NULL_TRUE_DIRECT)
                    .id("comparingNullableToTrueDirect")
                    .fix(fix);
            }
            return;
        }
        if self.nullable_false {
            return;
        }
        let defaulted = format!("({text} ?? true)");
        let fix = match (outer_not, negated_op) {
            (Some(not), false) => Fix::replace(cx.span(not), defaulted),
            (Some(_), true) => {
                Fix::replace(cx.span(paren.unwrap_or(node)), format!("!{defaulted}"))
            }
            (None, false) => Fix::replace(span, format!("!{defaulted}")),
            (None, true) => Fix::replace(span, defaulted),
        };
        cx.report_node(node, NULL_FALSE)
            .id("comparingNullableToFalse")
            .fix(fix);
    }
}

/// `!(node)`: the `!` and the parenthesis around `node`.
fn outer_not(cx: &Ctx<'_, '_>, node: NodeId) -> (Option<NodeId>, Option<NodeId>) {
    let mut cur = node;
    let mut paren = None;
    while let Some(p) = cx.parent(cur) {
        match *cx.node(p) {
            Node::Paren { .. } => {
                paren = Some(p);
                cur = p;
            }
            Node::Unary {
                op: UnaryOp::Not, ..
            } => return (Some(p), paren),
            _ => break,
        }
    }
    (None, None)
}

/// `node` is (part of a `&&`/`||` chain that is) the test of an `if`, loop or conditional.
fn boolean_context(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let mut cur = node;
    for _ in 0..10_000 {
        let Some(p) = cx.parent(cur) else {
            return false;
        };
        match *cx.node(p) {
            Node::Paren { .. }
            | Node::Binary {
                op: BinaryOp::LogicalAnd | BinaryOp::LogicalOr,
                ..
            } => {}
            Node::If { test, .. }
            | Node::While { test, .. }
            | Node::DoWhile { test, .. }
            | Node::Conditional { test, .. } => return test == cur,
            Node::For { test, .. } => return test == Some(cur),
            _ => return false,
        }
        cur = p;
    }
    false
}

/// `x ?? false` replacing `node` needs parentheses in its context.
fn needs_wrap(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    cx.parent(node).is_some_and(|p| {
        matches!(
            cx.node(p),
            Node::Binary { .. }
                | Node::Conditional { .. }
                | Node::Unary { .. }
                | Node::Member { .. }
                | Node::Index { .. }
                | Node::Await { .. }
                | Node::As { .. }
                | Node::Satisfies { .. }
        )
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn negations_of_nullable_compares_keep_their_context() {
        let rules = r#"{"@typescript-eslint/no-unnecessary-boolean-literal-compare":
            ["error", {"allowComparingNullableBooleansToTrue": false}]}"#;
        let code = "declare const x: boolean | undefined;\ndeclare const gate: boolean;\n\
                    export const a = gate && !(x !== true);\nexport const b = !(x !== true);\n";
        let Some(r) = crate::typed::lint_rules_for_test(code, rules) else {
            return;
        };
        let fixes: Vec<_> = r
            .diagnostics
            .iter()
            .filter_map(|d| d.fix.as_ref())
            .collect();
        let fixed = crate::fix::apply_fixes(code, &fixes).0;
        assert!(
            fixed.contains("export const a = gate && (x ?? false);\n"),
            "{fixed}"
        );
        assert!(fixed.contains("export const b = x ?? false;\n"), "{fixed}");
    }
}
