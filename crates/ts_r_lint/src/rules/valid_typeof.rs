//! `valid-typeof`: enforce comparing `typeof` expressions against valid strings.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "valid-typeof",
        "Enforce comparing `typeof` expressions against valid strings",
        ESLINT_REC,
        Category::Correctness,
    )
    .suggestions(),
    create: create::<ValidTypeof>,
};

#[derive(Debug, Default)]
struct ValidTypeof {
    require_string_literals: bool,
}

impl FromOptions for ValidTypeof {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(ValidTypeof {
            require_string_literals: bool_field(obj_opt(opts, 0), "requireStringLiterals", false),
        })
    }
}

const VALID: &[&str] = &[
    "symbol",
    "undefined",
    "object",
    "boolean",
    "number",
    "string",
    "function",
    "bigint",
];

impl Rule for ValidTypeof {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Unary]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Unary {
            op: UnaryOp::TypeOf,
            ..
        } = *cx.node(node)
        else {
            return;
        };
        let Some(parent) = parent_skip_parens(ast, cx.file.parents, node) else {
            return;
        };
        let Node::Binary { left, op, right } = *ast.node(parent) else {
            return;
        };
        if !matches!(
            op,
            BinaryOp::Eq | BinaryOp::NotEq | BinaryOp::StrictEq | BinaryOp::StrictNotEq
        ) {
            return;
        }
        let (l, r) = (skip_parens(ast, left), skip_parens(ast, right));
        let sibling = if l == node { r } else { l };
        match *ast.node(sibling) {
            Node::StringLit { value } => {
                if !VALID.contains(&ast.atom(value)) {
                    cx.report_node(sibling, "Invalid typeof comparison value.")
                        .id("invalidValue");
                }
            }
            Node::TemplateLit { parts } if ast.list(parts).len() == 1 => {
                if let Node::TemplateElement { cooked, .. } = *ast.node(ast.list(parts)[0])
                    && !VALID.contains(&ast.atom(cooked))
                {
                    cx.report_node(sibling, "Invalid typeof comparison value.")
                        .id("invalidValue");
                }
            }
            Node::Ident { .. } if cx.is_global_ref(sibling, "undefined") => {
                let (id, msg) = if self.require_string_literals {
                    (
                        "notString",
                        "Typeof comparisons should be to string literals.",
                    )
                } else {
                    ("invalidValue", "Invalid typeof comparison value.")
                };
                let span = cx.span(sibling);
                cx.report(span, msg).id(id).suggest(
                    "suggestString",
                    "Use `\"undefined\"` instead of `undefined`.",
                    Fix::replace(span, "\"undefined\""),
                );
            }
            Node::Unary {
                op: UnaryOp::TypeOf,
                ..
            } => {}
            _ => {
                if self.require_string_literals {
                    cx.report_node(sibling, "Typeof comparisons should be to string literals.")
                        .id("notString");
                }
            }
        }
    }
}
