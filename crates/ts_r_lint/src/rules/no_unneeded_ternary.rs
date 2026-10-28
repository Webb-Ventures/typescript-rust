//! `no-unneeded-ternary`: disallow `c ? true : false` and (optionally) `x ? x : y`.

use super::prelude::*;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-unneeded-ternary",
        "Disallow ternary operators when simpler alternatives exist",
        NO_PRESET,
        Category::Suspicious,
    )
    .fixable(),
    create: create::<NoUnneededTernary>,
};

#[derive(Debug)]
struct NoUnneededTernary {
    default_assignment: bool,
}

impl FromOptions for NoUnneededTernary {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(NoUnneededTernary {
            default_assignment: bool_field(obj_opt(opts, 0), "defaultAssignment", true),
        })
    }
}

fn bool_value(ast: &Ast, id: NodeId) -> Option<bool> {
    match *ast.node(id) {
        Node::BoolLit { value } => Some(value),
        _ => None,
    }
}

fn inverse(op: BinaryOp) -> Option<&'static str> {
    Some(match op {
        BinaryOp::Eq => "!=",
        BinaryOp::NotEq => "==",
        BinaryOp::StrictEq => "!==",
        BinaryOp::StrictNotEq => "===",
        _ => return None,
    })
}

/// Already a boolean: a comparison or a `!` negation.
fn is_boolean_expr(ast: &Ast, id: NodeId) -> bool {
    use BinaryOp::*;
    match *ast.node(id) {
        Node::Binary { op, .. } => matches!(
            op,
            Eq | NotEq | StrictEq | StrictNotEq | Lt | LtEq | Gt | GtEq | In | InstanceOf
        ),
        Node::Unary { op, .. } => op == UnaryOp::Not,
        _ => false,
    }
}

/// `prefix` applied to `id`, parenthesized when `id` binds looser than a unary operator.
fn negated(ast: &Ast, src: &str, prefix: &str, id: NodeId) -> String {
    let text = ast.span(id).text(src);
    if precedence(ast, id) < 16 {
        format!("{prefix}({text})")
    } else {
        format!("{prefix}{text}")
    }
}

impl NoUnneededTernary {
    fn boolean_fix(
        &self,
        ast: &Ast,
        src: &str,
        test: NodeId,
        cons: bool,
        alt: bool,
    ) -> Option<String> {
        let inner = skip_parens(ast, test);
        if cons == alt {
            return matches!(ast.node(inner), Node::Ident { .. }).then(|| cons.to_string());
        }
        if alt {
            // `test ? false : true`
            if let Node::Binary { left, op, .. } = *ast.node(inner)
                && let Some(inv) = inverse(op)
            {
                let s = ast.span(inner);
                let op_start = skip_trivia_forward(src, ast.span(left).end as usize);
                let op_end = op_start + op.as_str().len();
                return Some(format!(
                    "{}{inv}{}",
                    src.get(s.start as usize..op_start)?,
                    src.get(op_end..s.end as usize)?
                ));
            }
            return Some(negated(ast, src, "!", inner));
        }
        if is_boolean_expr(ast, inner) {
            Some(ast.span(inner).text(src).to_owned())
        } else {
            Some(negated(ast, src, "!!", inner))
        }
    }
}

impl Rule for NoUnneededTernary {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Conditional]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let src = cx.src();
        let Node::Conditional { test, cons, alt } = *ast.node(node) else {
            return;
        };
        let span = cx.span(node);
        if let (Some(c), Some(a)) = (
            bool_value(ast, skip_parens(ast, cons)),
            bool_value(ast, skip_parens(ast, alt)),
        ) {
            let fix = self
                .boolean_fix(ast, src, test, c, a)
                .map(|text| Fix::replace(span, text));
            cx.report(
                span,
                "Unnecessary use of boolean literals in conditional expression.",
            )
            .id("unnecessaryConditionalExpression")
            .fix_opt(fix);
            return;
        }
        if self.default_assignment {
            return;
        }
        let (Some(t), Some(c)) = (
            ident_name(ast, skip_parens(ast, test)),
            ident_name(ast, skip_parens(ast, cons)),
        ) else {
            return;
        };
        if t != c {
            return;
        }
        let inner_alt = skip_parens(ast, alt);
        let alt_text = cx.span(alt).text(src);
        let needs_parens = inner_alt == alt
            && (precedence(ast, alt) < 4
                || matches!(
                    ast.node(alt),
                    Node::Binary {
                        op: BinaryOp::Coalesce,
                        ..
                    }
                ));
        let alt_text = if needs_parens {
            format!("({alt_text})")
        } else {
            alt_text.to_owned()
        };
        let text = format!("{} || {alt_text}", cx.span(test).text(src));
        cx.report(
            span,
            "Unnecessary use of conditional expression for default assignment.",
        )
        .id("unnecessaryConditionalAssignment")
        .fix(Fix::replace(span, text));
    }
}
