//! `unicorn/require-css-escape`: an interpolation in the template-literal selector of
//! `x.querySelector(…)` (also `querySelectorAll`, `closest`, `matches`) that isn't already
//! `CSS.escape(…)`. By default only interpolations inside an attribute selector count
//! (an unclosed `[` before, a `]` after); `checkAllSelectors` checks all. The fix wraps the
//! value in `CSS.escape(…)` (a sequence gets its own parentheses).

use super::prelude::*;
use super::unicorn_ast::method_call;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/require-css-escape",
        "Require `CSS.escape()` for interpolated values in CSS selectors.",
        NO_PRESET,
        Category::Suspicious,
    )
    .fixable(),
    create: create::<RequireCssEscape>,
};

#[derive(Debug, Default)]
struct RequireCssEscape {
    check_all: bool,
}

impl FromOptions for RequireCssEscape {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(RequireCssEscape {
            check_all: bool_field(obj_opt(opts, 0), "checkAllSelectors", false),
        })
    }
}

/// `isNodeValueNotDomNode`: a literal, function, class, array, object, template or
/// `undefined`.
fn is_not_dom_node(ast: &ts_r_ast::Ast, id: NodeId) -> bool {
    match ast.node(id) {
        Node::ArrayLit { .. }
        | Node::Arrow { .. }
        | Node::ClassExpr { .. }
        | Node::FunctionExpr { .. }
        | Node::StringLit { .. }
        | Node::NumberLit { .. }
        | Node::BigIntLit { .. }
        | Node::BoolLit { .. }
        | Node::NullLit {}
        | Node::RegexLit { .. }
        | Node::ObjectLit { .. }
        | Node::TemplateLit { .. } => true,
        Node::Ident { .. } => ident_name(ast, id) == Some("undefined"),
        _ => false,
    }
}

impl Rule for RequireCssEscape {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some(call) = method_call(ast, node) else {
            return;
        };
        if !matches!(
            call.name,
            "closest" | "matches" | "querySelector" | "querySelectorAll"
        ) || is_not_dom_node(ast, skip_parens(ast, call.obj))
        {
            return;
        }
        let Some(&arg) = call.args.first() else {
            return;
        };
        if matches!(ast.node(arg), Node::Spread { .. }) {
            return;
        }
        let Node::TemplateLit { parts } = *ast.node(skip_parens(ast, arg)) else {
            return;
        };
        let parts = ast.list(parts);
        let cooked: Vec<&str> = parts
            .iter()
            .step_by(2)
            .map(|&q| match *ast.node(q) {
                Node::TemplateElement { cooked, .. } => ast.atom(cooked),
                _ => "",
            })
            .collect();
        let mut reports = Vec::new();
        for (index, &expr) in parts.iter().skip(1).step_by(2).enumerate() {
            let inner = skip_parens(ast, expr);
            if method_call(ast, inner).is_some_and(|c| {
                c.name == "escape"
                    && c.plain()
                    && c.args.len() == 1
                    && !matches!(ast.node(c.args[0]), Node::Spread { .. })
                    && ident_name(ast, skip_parens(ast, c.obj)) == Some("CSS")
            }) {
                continue;
            }
            if !self.check_all {
                let before: String = cooked[..=index].concat();
                let unclosed = match (before.rfind('['), before.rfind(']')) {
                    (Some(o), Some(c)) => o > c,
                    (Some(_), None) => true,
                    _ => false,
                };
                if !unclosed || !cooked[index + 1..].iter().any(|q| q.contains(']')) {
                    continue;
                }
            }
            reports.push(inner);
        }
        for expr in reports {
            let span = cx.span(expr);
            let seq = matches!(cx.node(expr), Node::Seq { .. });
            let (open, close) = if seq {
                ("CSS.escape((", "))")
            } else {
                ("CSS.escape(", ")")
            };
            let fix = Fix::replace(Span::at(span.start), open).and(Span::at(span.end), close);
            cx.report(
                span,
                "Use `CSS.escape()` for interpolated values in CSS selectors.",
            )
            .id("require-css-escape")
            .fix(fix);
        }
    }
}
