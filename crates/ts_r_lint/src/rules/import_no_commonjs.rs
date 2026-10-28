//! `import/no-commonjs`: `module.exports`, `exports.x` (when no `exports` binding is in
//! the innermost scope) and `require("x")` at module level. Options: the legacy
//! `"allow-primitive-modules"` string or `{ allowPrimitiveModules, allowRequire,
//! allowConditionalRequire }`; a `require` under an `if`, `try`, logical or conditional
//! expression is allowed unless `allowConditionalRequire` is false.

use super::import_util::var_scope_kind;
use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "import/no-commonjs",
        "Forbid CommonJS `require` calls and `module.exports` or `exports.*`.",
        NO_PRESET,
        Category::Restriction,
    ),
    create: create::<NoCommonjs>,
};

const EXPORT_MESSAGE: &str = "Expected \"export\" or \"export default\"";
const IMPORT_MESSAGE: &str = "Expected \"import\" instead of \"require()\"";

#[derive(Debug)]
struct NoCommonjs {
    allow_primitive_modules: bool,
    allow_require: bool,
    allow_conditional_require: bool,
}

impl FromOptions for NoCommonjs {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        if opts.iter().any(|v| v == "allow-primitive-modules") {
            return Ok(NoCommonjs {
                allow_primitive_modules: true,
                allow_require: false,
                allow_conditional_require: true,
            });
        }
        let o = obj_opt(opts, 0);
        Ok(NoCommonjs {
            allow_primitive_modules: bool_field(o, "allowPrimitiveModules", false),
            allow_require: bool_field(o, "allowRequire", false),
            allow_conditional_require: bool_field(o, "allowConditionalRequire", true),
        })
    }
}

impl NoCommonjs {
    fn allow_primitive(&self, cx: &Ctx<'_, '_>, member: NodeId) -> bool {
        if !self.allow_primitive_modules {
            return false;
        }
        let ast = cx.ast();
        let Some(p) = parent_skip_parens(ast, cx.file.parents, member) else {
            return false;
        };
        match *ast.node(p) {
            Node::Assign { right, .. } => ast.kind(skip_parens(ast, right)) != NodeKind::ObjectLit,
            _ => false,
        }
    }
}

fn is_conditional(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    ancestors_inclusive(cx.file.parents, node).any(|a| match ast.node(a) {
        Node::If { .. } | Node::Try { .. } | Node::Conditional { .. } => true,
        Node::Binary { op, .. } => matches!(
            op,
            BinaryOp::LogicalAnd | BinaryOp::LogicalOr | BinaryOp::Coalesce
        ),
        _ => false,
    })
}

impl Rule for NoCommonjs {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Member, NodeKind::Index, NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        match *cx.node(node) {
            Node::Member { obj, prop, .. }
            | Node::Index {
                obj, index: prop, ..
            } => {
                let object = ident_name(ast, skip_parens(ast, obj));
                if object == Some("module")
                    && ast.kind(node) == NodeKind::Member
                    && ident_name(ast, prop) == Some("exports")
                {
                    if !self.allow_primitive(cx, node) {
                        cx.report_node(node, EXPORT_MESSAGE);
                    }
                    return;
                }
                if object == Some("exports") {
                    let sem = cx.semantic();
                    let scope = sem.scope(sem.enclosing_scope(node));
                    if !scope.symbols.iter().any(|&s| sem.name(s) == "exports") {
                        cx.report_node(node, EXPORT_MESSAGE);
                    }
                }
            }
            Node::Call { callee, args, .. } => {
                if var_scope_kind(cx, node) != ScopeKind::Module {
                    return;
                }
                let callee = skip_parens(ast, callee);
                if ident_name(ast, callee) != Some("require") {
                    return;
                }
                let &[arg] = ast.list(args) else { return };
                let literal = match ast.node(skip_parens(ast, arg)) {
                    Node::StringLit { .. } => true,
                    Node::TemplateLit { parts } => ast.list(*parts).len() == 1,
                    _ => false,
                };
                if !literal || self.allow_require {
                    return;
                }
                if self.allow_conditional_require
                    && cx.parent(node).is_some_and(|p| is_conditional(cx, p))
                {
                    return;
                }
                cx.report_node(callee, IMPORT_MESSAGE);
            }
            _ => {}
        }
    }
}
