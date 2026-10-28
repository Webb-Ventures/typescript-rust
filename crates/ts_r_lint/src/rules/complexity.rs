//! `complexity`: at most `max` (20) branches in a function, class field initializer or
//! static block (`classic` counts each `case`, `modified` each `switch`).

use super::func_names::own_name_with_kind;
use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "complexity",
        "Enforce a maximum cyclomatic complexity allowed in a program",
        NO_PRESET,
        Category::Suspicious,
    ),
    create: create::<Complexity>,
};

#[derive(Debug, Default)]
struct Complexity {
    max: u64,
    modified: bool,
}

impl FromOptions for Complexity {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        let max = match opts.first() {
            Some(Value::Number(n)) => n.as_u64(),
            _ => o
                .get("maximum")
                .and_then(Value::as_u64)
                .filter(|&m| m > 0)
                .or_else(|| o.get("max").and_then(Value::as_u64)),
        }
        .unwrap_or(20);
        Ok(Complexity {
            max,
            modified: str_field(o, "variant") == Some("modified"),
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Origin {
    Program,
    Function,
    Field,
    StaticBlock,
}

impl Complexity {
    fn weight(&self, n: &Node) -> u64 {
        let hit = match *n {
            Node::CatchClause { .. }
            | Node::Conditional { .. }
            | Node::For { .. }
            | Node::ForIn { .. }
            | Node::ForOf { .. }
            | Node::If { .. }
            | Node::While { .. }
            | Node::DoWhile { .. }
            | Node::AssignPattern { .. }
            | Node::Param { init: Some(_), .. }
            | Node::BindingProperty { init: Some(_), .. }
            | Node::ShorthandProp { init: Some(_), .. } => true,
            Node::Binary { op, .. } => matches!(
                op,
                BinaryOp::LogicalOr | BinaryOp::LogicalAnd | BinaryOp::Coalesce
            ),
            Node::Assign { op, .. } => matches!(
                op,
                AssignOp::OrAssign | AssignOp::AndAssign | AssignOp::CoalesceAssign
            ),
            Node::Member { optional, .. }
            | Node::Index { optional, .. }
            | Node::Call { optional, .. } => optional,
            Node::SwitchCase { test, .. } => !self.modified && test.is_some(),
            Node::Switch { .. } => self.modified,
            _ => false,
        };
        u64::from(hit)
    }
}

impl Rule for Complexity {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let mut paths: Vec<(NodeId, Origin, u64)> = vec![(ast.root, Origin::Program, 1)];
        let mut stack = vec![(ast.root, 0usize)];
        while let Some((n, path)) = stack.pop() {
            let node = ast.node(n);
            let origin = match node {
                Node::FunctionDecl { body: Some(_), .. }
                | Node::FunctionExpr { .. }
                | Node::Arrow { .. } => Some(Origin::Function),
                Node::StaticBlock { .. } => Some(Origin::StaticBlock),
                _ => None,
            };
            let path = match origin {
                Some(o) if n != ast.root => {
                    paths.push((n, o, 1));
                    paths.len() - 1
                }
                _ => path,
            };
            paths[path].2 += self.weight(node);
            let field = match *node {
                Node::PropertyDef { value: Some(v), .. } => Some(v),
                _ => None,
            };
            ast.for_each_child(n, |c| {
                if Some(c) == field {
                    paths.push((c, Origin::Field, 1));
                    stack.push((c, paths.len() - 1));
                } else {
                    stack.push((c, path));
                }
            });
        }
        // A field's value that is itself a function: the function is its own path, and the
        // initializer path holds only it.
        let mut hits: Vec<(NodeId, Origin, u64)> = paths
            .into_iter()
            .filter(|&(_, o, c)| o != Origin::Program && c > self.max)
            .collect();
        hits.sort_by_key(|&(n, o, _)| (cx.span(n).start, o == Origin::Function));
        for (n, origin, c) in hits {
            let (name, span) = match origin {
                Origin::Field => ("Class field initializer".to_owned(), cx.span(n)),
                Origin::StaticBlock => {
                    let s = cx.span(n).start;
                    ("Class static block".to_owned(), Span::new(s, s + 6))
                }
                _ => {
                    let name = own_name_with_kind(cx, n);
                    let mut ch = name.chars();
                    let name = match ch.next() {
                        Some(h) => h.to_uppercase().chain(ch).collect(),
                        None => name,
                    };
                    let mut head = function_head_span(ast, cx.src(), cx.file.parents, n);
                    // A field's function is "method 'x'", reported from the field to `(`.
                    let mut name: String = name;
                    if let Some(p) = cx.parent(n)
                        && let Node::PropertyDef { modifiers, .. } = *ast.node(p)
                        && !modifiers.contains(Modifiers::ACCESSOR)
                    {
                        let span = cx.span(n);
                        let paren = cx
                            .text(n)
                            .find('(')
                            .map_or(span.start, |i| span.start + i as u32);
                        head = Span::new(cx.span(p).start, paren);
                        name = name.replacen("Arrow function", "Method", 1);
                    }
                    (name, head)
                }
            };
            cx.report(
                span,
                format!(
                    "{name} has a complexity of {c}. Maximum allowed is {}.",
                    self.max
                ),
            )
            .id("complex");
        }
    }
}
