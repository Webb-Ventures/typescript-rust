//! Abstract properties read through `this` while the object is being constructed (TS2715).
//!
//! Rules observed from tsc 7.0.2: in a constructor body or an instance property
//! initializer (not inside a nested function, arrow or class), `this.p` (read, write or
//! `++`) and each property named by an object destructuring of `this`
//! (`let { x, y: z } = this`, `({ x, "y": z } = this)`) is reported when `p` resolves to an
//! abstract property or accessor; abstract methods are not. The message names the class
//! that declares the abstract member (a class expression assigned to a variable by the
//! variable's name). Other objects (`other.p`) are not checked.

use ts_r_ast::{AssignOp, MethodKind, Modifiers, Node, NodeId};
use ts_r_diagnostics::FileId;
use ts_r_types::SymId;

use crate::checker::{Ck, Loc};
use crate::diag::dg;
use crate::nodes::skip_parens;

impl<'c, 'a> Ck<'c, 'a> {
    /// TS2715 for the property access `obj.name` at `pl`, whose property resolved to `p`.
    pub(crate) fn check_abstract_this_access(
        &mut self,
        e: Loc,
        obj: NodeId,
        pl: Loc,
        p: SymId,
        name: &str,
    ) {
        if !self.reporting(e.file) {
            return;
        }
        let ast = self.ast(e.file);
        if !matches!(ast.node(skip_parens(ast, obj)), Node::This {}) {
            return;
        }
        if !self.in_construction(e) {
            return;
        }
        self.report_abstract_property(pl, p, name);
    }

    /// TS2715 for the object destructuring of `this` declared or assigned by `n` (a
    /// variable declarator or an assignment).
    pub(crate) fn check_abstract_this_destructuring(&mut self, file: FileId, n: NodeId) {
        if !self.reporting(file) {
            return;
        }
        let ast = self.ast(file);
        let (pattern, source) = match *ast.node(n) {
            Node::VarDeclarator {
                name,
                init: Some(init),
                ..
            } => (name, init),
            Node::Assign {
                left,
                op: AssignOp::Assign,
                right,
            } => (skip_parens(ast, left), right),
            _ => return,
        };
        if !matches!(ast.node(skip_parens(ast, source)), Node::This {}) {
            return;
        }
        let keys: Vec<NodeId> = match *ast.node(pattern) {
            Node::ObjectPattern { props } => ast
                .list(props)
                .iter()
                .filter_map(|&p| match *ast.node(p) {
                    Node::BindingProperty { key, .. } => Some(key),
                    _ => None,
                })
                .collect(),
            Node::ObjectLit { props } => ast
                .list(props)
                .iter()
                .filter_map(|&p| match *ast.node(p) {
                    Node::Property { key, .. } => Some(key),
                    Node::ShorthandProp { name, .. } => Some(name),
                    _ => None,
                })
                .collect(),
            _ => return,
        };
        if keys.is_empty() || !self.in_construction(Loc::new(file, n)) {
            return;
        }
        let this_t = self.quietly(|ck| ck.check_expr(Loc::new(file, source)));
        for k in keys {
            let name = match *ast.node(k) {
                Node::Ident { name } | Node::StringLit { value: name } => ast.atom(name),
                _ => continue,
            };
            if let Some(p) = self.property_of_type(this_t, name) {
                self.report_abstract_property(Loc::new(file, k), p, name);
            }
        }
    }

    fn report_abstract_property(&mut self, at: Loc, p: SymId, name: &str) {
        let p = match self.c.synth_sym(p) {
            Some(x) if x.origins.len() == 1 => x.origins[0],
            Some(_) => return,
            None => p,
        };
        let Some(d) = self.c.value_decl(p).or_else(|| self.c.first_decl(p)) else {
            return;
        };
        let ast = self.ast(d.file);
        let abstract_property = match *ast.node(d.node) {
            Node::PropertyDef { modifiers, .. } => modifiers.contains(Modifiers::ABSTRACT),
            Node::MethodDef {
                modifiers,
                kind: MethodKind::Get | MethodKind::Set,
                ..
            } => modifiers.contains(Modifiers::ABSTRACT),
            _ => false,
        };
        if !abstract_property {
            return;
        }
        let Some(class) = self.c.file(d.file).parents().parent(d.node) else {
            return;
        };
        let own = match *ast.node(class) {
            Node::ClassDecl { name, .. } | Node::ClassExpr { name, .. } => name,
            _ => return,
        };
        let class_name = self.class_display_name(Loc::new(d.file, class), own);
        self.error(
            at,
            dg::ABSTRACT_PROPERTY_0_IN_CLASS_1_CANNOT_BE_ACCESSED_IN_THE_CONSTRUCTOR_2715,
            &[name, &class_name],
        );
    }

    /// `e` runs while an instance is constructed: in a constructor body or an instance
    /// property initializer, not in a nested function or class.
    fn in_construction(&self, e: Loc) -> bool {
        let ast = self.ast(e.file);
        let parents = self.c.file(e.file).parents();
        let mut prev = e.node;
        let mut cur = parents.parent(e.node);
        for _ in 0..4096 {
            let Some(n) = cur else { return false };
            match *ast.node(n) {
                Node::FunctionExpr { .. } => {
                    return parents.parent(n).is_some_and(|m| {
                        matches!(
                            ast.node(m),
                            Node::MethodDef {
                                kind: MethodKind::Constructor,
                                ..
                            }
                        )
                    });
                }
                Node::PropertyDef {
                    value, modifiers, ..
                } if value == Some(prev) => return !modifiers.contains(Modifiers::STATIC),
                Node::FunctionDecl { .. }
                | Node::Arrow { .. }
                | Node::MethodDef { .. }
                | Node::ClassDecl { .. }
                | Node::ClassExpr { .. }
                | Node::StaticBlock { .. } => return false,
                _ => {}
            }
            prev = n;
            cur = parents.parent(n);
        }
        false
    }
}
