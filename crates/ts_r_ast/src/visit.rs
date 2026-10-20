//! Tree traversal: an enter/leave visitor driven by an explicit stack, and a parent map.

use crate::{Ast, NodeId};

/// What to do after [`Visitor::enter`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Walk {
    Continue,
    /// Do not visit this node's children (`leave` is still called).
    SkipChildren,
    /// Stop the whole walk immediately.
    Stop,
}

/// Depth-first visitor. Both methods default to doing nothing.
pub trait Visitor {
    fn enter(&mut self, _ast: &Ast, _id: NodeId) -> Walk {
        Walk::Continue
    }
    fn leave(&mut self, _ast: &Ast, _id: NodeId) {}
}

/// Walks the subtree under `root` in source order, calling `enter` before and `leave` after
/// each node's children. Uses a heap stack, so arbitrarily deep trees are fine.
/// Returns false if the visitor stopped the walk.
pub fn walk<V: Visitor + ?Sized>(ast: &Ast, root: NodeId, v: &mut V) -> bool {
    // (node, children_pushed)
    let mut stack: Vec<(NodeId, bool)> = vec![(root, false)];
    let mut scratch: Vec<NodeId> = Vec::new();
    while let Some(top) = stack.last_mut() {
        let (id, entered) = *top;
        if entered {
            stack.pop();
            v.leave(ast, id);
            continue;
        }
        top.1 = true;
        match v.enter(ast, id) {
            Walk::Stop => return false,
            Walk::SkipChildren => {}
            Walk::Continue => {
                scratch.clear();
                ast.for_each_child(id, |c| scratch.push(c));
                for &c in scratch.iter().rev() {
                    stack.push((c, false));
                }
            }
        }
    }
    true
}

/// Parent of every node, built in one walk. 4 bytes per node.
#[derive(Clone, Debug)]
pub struct ParentMap {
    parents: Vec<Option<NodeId>>,
}

impl ParentMap {
    pub fn build(ast: &Ast) -> ParentMap {
        let mut parents = vec![None; ast.nodes.len()];
        let mut stack = vec![ast.root];
        while let Some(id) = stack.pop() {
            ast.for_each_child(id, |c| {
                if let Some(slot) = parents.get_mut(c.index()) {
                    *slot = Some(id);
                }
                stack.push(c);
            });
        }
        // JSDoc types that fill no field of the tree hang off their host.
        for t in &ast.jsdoc_tags {
            let Some(ty) = t.ty else { continue };
            if parents.get(ty.index()).is_some_and(|p| p.is_none()) {
                parents[ty.index()] = Some(t.host);
                let mut stack = vec![ty];
                while let Some(id) = stack.pop() {
                    ast.for_each_child(id, |c| {
                        if let Some(slot) = parents.get_mut(c.index()) {
                            *slot = Some(id);
                        }
                        stack.push(c);
                    });
                }
            }
        }
        ParentMap { parents }
    }

    /// The parent of `id`, or `None` for the root and for unreachable nodes.
    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.parents.get(id.index()).copied().flatten()
    }

    /// `id`'s ancestors, nearest first.
    pub fn ancestors(&self, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        std::iter::successors(self.parent(id), move |&p| self.parent(p))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{F64Bits, Node, SourceKind, Span};

    fn sample() -> (Ast, NodeId, NodeId, NodeId) {
        let mut ast = Ast::new(SourceKind::Ts, 0);
        let a = ast.add(
            Node::NumberLit {
                value: F64Bits::new(1.0),
            },
            Span::new(1, 2),
        );
        let b = ast.add(
            Node::NumberLit {
                value: F64Bits::new(2.0),
            },
            Span::new(4, 5),
        );
        let list = ast.add_list(&[a, b]);
        let arr = ast.add(Node::ArrayLit { elements: list }, Span::new(0, 6));
        let stmt = ast.add(Node::ExprStmt { expr: arr }, Span::new(0, 6));
        let stmts = ast.add_list(&[stmt]);
        ast.root = ast.add(Node::Program { stmts }, Span::new(0, 6));
        (ast, arr, a, b)
    }

    struct Order(Vec<String>);
    impl Visitor for Order {
        fn enter(&mut self, ast: &Ast, id: NodeId) -> Walk {
            self.0.push(format!("+{}", ast.kind(id).name()));
            if ast.kind(id) == crate::NodeKind::ArrayLit {
                Walk::SkipChildren
            } else {
                Walk::Continue
            }
        }
        fn leave(&mut self, ast: &Ast, id: NodeId) {
            self.0.push(format!("-{}", ast.kind(id).name()));
        }
    }

    #[test]
    fn walk_order_and_skip() {
        let (ast, ..) = sample();
        let mut v = Order(Vec::new());
        assert!(walk(&ast, ast.root, &mut v));
        assert_eq!(
            v.0,
            [
                "+Program",
                "+ExprStmt",
                "+ArrayLit",
                "-ArrayLit",
                "-ExprStmt",
                "-Program"
            ]
        );
    }

    #[test]
    fn walk_stop() {
        struct Stopper(usize);
        impl Visitor for Stopper {
            fn enter(&mut self, _: &Ast, _: NodeId) -> Walk {
                self.0 += 1;
                if self.0 == 2 {
                    Walk::Stop
                } else {
                    Walk::Continue
                }
            }
        }
        let (ast, ..) = sample();
        let mut s = Stopper(0);
        assert!(!walk(&ast, ast.root, &mut s));
        assert_eq!(s.0, 2);
    }

    #[test]
    fn parents() {
        let (ast, arr, a, b) = sample();
        let pm = ParentMap::build(&ast);
        assert_eq!(pm.parent(a), Some(arr));
        assert_eq!(pm.parent(b), Some(arr));
        let anc: Vec<_> = pm.ancestors(a).map(|id| ast.kind(id)).collect();
        assert_eq!(
            anc,
            [
                crate::NodeKind::ArrayLit,
                crate::NodeKind::ExprStmt,
                crate::NodeKind::Program
            ]
        );
    }
}
