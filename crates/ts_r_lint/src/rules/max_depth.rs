//! `max-depth`: limit how deeply blocks (`if`, loops, `switch`, `try`, `with`) nest.

use super::prelude::*;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "max-depth",
        "Enforce a maximum depth that blocks can be nested",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<MaxDepth>,
};

#[derive(Debug, Default)]
struct MaxDepth {
    max: u64,
}

impl FromOptions for MaxDepth {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(MaxDepth {
            max: max_option(opts, 4),
        })
    }
}

const KINDS: &[NodeKind] = &[
    NodeKind::If,
    NodeKind::Switch,
    NodeKind::Try,
    NodeKind::DoWhile,
    NodeKind::While,
    NodeKind::With,
    NodeKind::For,
    NodeKind::ForIn,
    NodeKind::ForOf,
];

/// The keyword a nesting statement starts with, or `None` when `id` does not add a level
/// (an `else if` continues its parent's level).
fn keyword(ast: &Ast, parents: &ts_r_ast::ParentMap, id: NodeId) -> Option<&'static str> {
    Some(match *ast.node(id) {
        Node::If { .. } => {
            let is_else_if = parents
                .parent(id)
                .is_some_and(|p| matches!(*ast.node(p), Node::If { alt: Some(a), .. } if a == id));
            if is_else_if {
                return None;
            }
            "if"
        }
        Node::Switch { .. } => "switch",
        Node::Try { .. } => "try",
        Node::DoWhile { .. } => "do",
        Node::While { .. } => "while",
        Node::With { .. } => "with",
        Node::For { .. } | Node::ForIn { .. } | Node::ForOf { .. } => "for",
        _ => return None,
    })
}

impl Rule for MaxDepth {
    fn kinds(&self) -> &'static [NodeKind] {
        KINDS
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let parents = cx.file.parents;
        let Some(kw) = keyword(ast, parents, node) else {
            return;
        };
        let mut depth = 1u64;
        for a in parents.ancestors(node) {
            let k = ast.node(a).kind();
            if is_function(k) || k == NodeKind::StaticBlock {
                break;
            }
            if keyword(ast, parents, a).is_some() {
                depth += 1;
            }
        }
        if depth > self.max {
            let start = cx.span(node).start;
            cx.report(
                Span::new(start, start + kw.len() as u32),
                format!(
                    "Blocks are nested too deeply ({depth}). Maximum allowed is {}.",
                    self.max
                ),
            )
            .id("tooDeeply");
        }
    }
}
