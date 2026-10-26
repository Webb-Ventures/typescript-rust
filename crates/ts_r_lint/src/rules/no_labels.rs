//! `no-labels`: disallow labeled statements and labeled `break`/`continue`.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-labels",
        "Disallow labeled statements",
        NO_PRESET,
        Category::Restriction,
    ),
    create: create::<NoLabels>,
};

#[derive(Debug, Default)]
struct NoLabels {
    allow_loop: bool,
    allow_switch: bool,
}

impl FromOptions for NoLabels {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(NoLabels {
            allow_loop: bool_field(o, "allowLoop", false),
            allow_switch: bool_field(o, "allowSwitch", false),
        })
    }
}

impl NoLabels {
    /// Whether a label on `body` is allowed by the options.
    fn allowed(&self, ast: &ts_r_ast::Ast, body: NodeId) -> bool {
        match ast.node(body) {
            Node::For { .. }
            | Node::ForIn { .. }
            | Node::ForOf { .. }
            | Node::While { .. }
            | Node::DoWhile { .. } => self.allow_loop,
            Node::Switch { .. } => self.allow_switch,
            _ => false,
        }
    }
}

impl Rule for NoLabels {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Labeled, NodeKind::Break, NodeKind::Continue]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let (label, id, msg) = match *ast.node(node) {
            Node::Labeled { body, .. } => {
                if !self.allowed(ast, body) {
                    cx.report(cx.span(node), "Unexpected labeled statement.")
                        .id("unexpectedLabel");
                }
                return;
            }
            Node::Break { label: Some(l) } => (
                l,
                "unexpectedLabelInBreak",
                "Unexpected label in break statement.",
            ),
            Node::Continue { label: Some(l) } => (
                l,
                "unexpectedLabelInContinue",
                "Unexpected label in continue statement.",
            ),
            _ => return,
        };
        let name = ident_name(ast, label);
        let target = cx
            .file
            .parents
            .ancestors(node)
            .find_map(|a| match *ast.node(a) {
                Node::Labeled { label: l, body } if ident_name(ast, l) == name => Some(body),
                _ => None,
            });
        if target.is_some_and(|b| self.allowed(ast, b)) {
            return;
        }
        cx.report(cx.span(node), msg).id(id);
    }
}
