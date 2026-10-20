//! `no-unused-expressions`: disallow unused expressions. Also hosts
//! `@typescript-eslint/no-unused-expressions`.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-unused-expressions",
        "Disallow unused expressions",
        NO_PRESET,
        Category::Suspicious,
    ),
    create: create::<NoUnusedExpressions>,
};

#[derive(Debug)]
pub(crate) struct NoUnusedExpressions {
    short_circuit: bool,
    ternary: bool,
    tagged_templates: bool,
    jsx: bool,
    ignore_directives: bool,
}

impl FromOptions for NoUnusedExpressions {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(NoUnusedExpressions {
            short_circuit: bool_field(o, "allowShortCircuit", false),
            ternary: bool_field(o, "allowTernary", false),
            tagged_templates: bool_field(o, "allowTaggedTemplates", false),
            jsx: bool_field(o, "enforceForJSX", false),
            ignore_directives: bool_field(o, "ignoreDirectives", false),
        })
    }
}

const MSG: &str = "Expected an assignment or function call and instead saw an expression.";

impl NoUnusedExpressions {
    fn valid(&self, ast: &ts_r_ast::Ast, id: NodeId) -> bool {
        match *ast.node(id) {
            Node::Paren { expr }
            | Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::TypeAssertion { expr, .. }
            | Node::NonNull { expr } => self.valid(ast, expr),
            Node::Assign { .. }
            | Node::Call { .. }
            | Node::New { .. }
            | Node::Update { .. }
            | Node::Await { .. }
            | Node::Yield { .. }
            | Node::ImportCall { .. } => true,
            Node::Unary { op, .. } => matches!(op, UnaryOp::Delete | UnaryOp::Void),
            Node::Binary {
                op: BinaryOp::LogicalAnd | BinaryOp::LogicalOr | BinaryOp::Coalesce,
                right,
                ..
            } => self.short_circuit && self.valid(ast, right),
            Node::Conditional { cons, alt, .. } => {
                self.ternary && self.valid(ast, cons) && self.valid(ast, alt)
            }
            Node::TaggedTemplate { .. } => self.tagged_templates,
            Node::JsxElement { .. } | Node::JsxFragment { .. } => !self.jsx,
            _ => false,
        }
    }
}

/// A string statement at the start of a TS module block (parsed as an expression).
fn module_prologue(cx: &Ctx<'_, '_>, stmt: NodeId, expr: NodeId) -> bool {
    let ast = cx.ast();
    if !matches!(ast.node(expr), Node::StringLit { .. }) {
        return false;
    }
    let Some(p) = cx.parent(stmt) else {
        return false;
    };
    let Node::ModuleBlock { stmts } = *ast.node(p) else {
        return false;
    };
    ast.list(stmts)
        .iter()
        .take_while(|&&s| s != stmt)
        .all(|&s| match *ast.node(s) {
            Node::ExprStmt { expr } => {
                matches!(ast.node(expr), Node::StringLit { .. })
                    && cx.span(expr).start == cx.span(s).start
            }
            _ => false,
        })
        && cx.span(expr).start == cx.span(stmt).start
}

impl Rule for NoUnusedExpressions {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::ExprStmt, NodeKind::Directive]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        match *cx.node(node) {
            Node::Directive { .. } => {
                // ES3 has no directives.
                if crate::globals::normalize_ecma_version(cx.env().ecma_version) < 5
                    && !self.ignore_directives
                {
                    cx.report_node(node, MSG).id("unusedExpression");
                }
            }
            Node::ExprStmt { expr } => {
                if self.valid(ast, expr) || module_prologue(cx, node, expr) {
                    return;
                }
                cx.report_node(node, MSG).id("unusedExpression");
            }
            _ => {}
        }
    }
}
