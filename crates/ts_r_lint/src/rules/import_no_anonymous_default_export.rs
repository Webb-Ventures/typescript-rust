//! `import/no-anonymous-default-export`: `export default` of an anonymous value: array,
//! arrow function, call (allowed by default), anonymous class or function declaration,
//! literal or template, object, or `new` expression. Each kind has an `allow*` option.
//! A parenthesized function or class expression is not a declaration and is not reported.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "import/no-anonymous-default-export",
        "Forbid anonymous values as default exports.",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<NoAnonymousDefaultExport>,
};

#[derive(Debug, Default)]
struct NoAnonymousDefaultExport {
    allow_array: bool,
    allow_arrow_function: bool,
    allow_call_expression: bool,
    allow_anonymous_class: bool,
    allow_anonymous_function: bool,
    allow_literal: bool,
    allow_object: bool,
    allow_new: bool,
}

impl FromOptions for NoAnonymousDefaultExport {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(NoAnonymousDefaultExport {
            allow_array: bool_field(o, "allowArray", false),
            allow_arrow_function: bool_field(o, "allowArrowFunction", false),
            allow_call_expression: bool_field(o, "allowCallExpression", true),
            allow_anonymous_class: bool_field(o, "allowAnonymousClass", false),
            allow_anonymous_function: bool_field(o, "allowAnonymousFunction", false),
            allow_literal: bool_field(o, "allowLiteral", false),
            allow_object: bool_field(o, "allowObject", false),
            allow_new: bool_field(o, "allowNew", false),
        })
    }
}

impl Rule for NoAnonymousDefaultExport {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::ExportDefaultExpr,
            NodeKind::FunctionDecl,
            NodeKind::ClassDecl,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let (allowed, message) = match *cx.node(node) {
            Node::FunctionDecl {
                modifiers,
                name: None,
                body: Some(_),
                ..
            } if modifiers.contains(Modifiers::DEFAULT) => (
                self.allow_anonymous_function,
                "Unexpected default export of anonymous function",
            ),
            Node::ClassDecl {
                modifiers,
                name: None,
                ..
            } if modifiers.contains(Modifiers::DEFAULT) => (
                self.allow_anonymous_class,
                "Unexpected default export of anonymous class",
            ),
            Node::ExportDefaultExpr { expr } => {
                let expr = skip_parens(ast, expr);
                match ast.node(expr) {
                    Node::ArrayLit { .. } => (
                        self.allow_array,
                        "Assign array to a variable before exporting as module default",
                    ),
                    Node::Arrow { .. } => (
                        self.allow_arrow_function,
                        "Assign arrow function to a variable before exporting as module default",
                    ),
                    Node::Call { .. } if !chain_has_optional(ast, expr) => (
                        self.allow_call_expression,
                        "Assign call result to a variable before exporting as module default",
                    ),
                    Node::StringLit { .. }
                    | Node::NumberLit { .. }
                    | Node::BigIntLit { .. }
                    | Node::BoolLit { .. }
                    | Node::NullLit {}
                    | Node::RegexLit { .. }
                    | Node::TemplateLit { .. } => (
                        self.allow_literal,
                        "Assign literal to a variable before exporting as module default",
                    ),
                    Node::ObjectLit { .. } => (
                        self.allow_object,
                        "Assign object to a variable before exporting as module default",
                    ),
                    Node::New { .. } => (
                        self.allow_new,
                        "Assign instance to a variable before exporting as module default",
                    ),
                    _ => return,
                }
            }
            _ => return,
        };
        if !allowed {
            cx.report_node(node, message);
        }
    }
}
