//! Constructors for synthesized nodes.
//!
//! Every synthesized node gets an **empty span** at the position of the code it came from
//! (`Span::new(pos, pos)`): codegen prints literals of empty-span nodes from their values,
//! and uses `pos` for the source map. `pos == 0` with an empty span means "unmapped".

use ts_r_ast::{
    AssignOp, Ast, BinaryOp, F64Bits, FnFlags, MethodKind, Modifiers, Node, NodeId, NodeList, Span,
    UnaryOp, VarKind,
};

/// Node factory over a mutable AST. All methods take the source position `at` to map to.
pub(crate) trait Build {
    fn ast_mut(&mut self) -> &mut Ast;

    fn mk(&mut self, node: Node, at: u32) -> NodeId {
        self.ast_mut().add(node, Span::new(at, at))
    }

    fn new_list(&mut self, items: &[NodeId]) -> NodeList {
        self.ast_mut().add_list(items)
    }

    fn ident(&mut self, name: &str, at: u32) -> NodeId {
        let name = self.ast_mut().intern(name);
        self.mk(Node::Ident { name }, at)
    }

    fn ident_atom(&mut self, name: ts_r_ast::Atom, at: u32) -> NodeId {
        self.mk(Node::Ident { name }, at)
    }

    fn string(&mut self, value: &str, at: u32) -> NodeId {
        let value = self.ast_mut().intern(value);
        self.mk(Node::StringLit { value }, at)
    }

    fn number(&mut self, v: f64, at: u32) -> NodeId {
        if v.is_sign_negative() && v != 0.0 && v.is_finite() {
            // `-1` is a unary expression in JS.
            let arg = self.mk(
                Node::NumberLit {
                    value: F64Bits::new(-v),
                },
                at,
            );
            return self.mk(
                Node::Unary {
                    op: UnaryOp::Minus,
                    arg,
                },
                at,
            );
        }
        self.mk(
            Node::NumberLit {
                value: F64Bits::new(v),
            },
            at,
        )
    }

    fn bool_lit(&mut self, value: bool, at: u32) -> NodeId {
        self.mk(Node::BoolLit { value }, at)
    }

    fn null(&mut self, at: u32) -> NodeId {
        self.mk(Node::NullLit {}, at)
    }

    fn this(&mut self, at: u32) -> NodeId {
        self.mk(Node::This {}, at)
    }

    /// `void 0`
    fn void0(&mut self, at: u32) -> NodeId {
        let zero = self.number(0.0, at);
        self.mk(
            Node::Unary {
                op: UnaryOp::Void,
                arg: zero,
            },
            at,
        )
    }

    /// `obj.name`
    fn member(&mut self, obj: NodeId, name: &str, at: u32) -> NodeId {
        let prop = self.ident(name, at);
        self.mk(
            Node::Member {
                obj,
                optional: false,
                prop,
            },
            at,
        )
    }

    /// `obj.name`, or `obj["name"]` when `name` is not an identifier.
    fn member_or_index(&mut self, obj: NodeId, name: &str, at: u32) -> NodeId {
        if ts_r_codegen::literals::is_identifier_name(name) {
            self.member(obj, name, at)
        } else {
            let index = self.string(name, at);
            self.index(obj, index, at)
        }
    }

    /// `a.b.c` from a dotted path.
    fn dotted(&mut self, path: &str, at: u32) -> NodeId {
        let mut parts = path.split('.');
        let first = parts.next().unwrap_or("");
        let mut e = self.ident(first, at);
        for p in parts {
            e = self.member(e, p, at);
        }
        e
    }

    fn index(&mut self, obj: NodeId, index: NodeId, at: u32) -> NodeId {
        self.mk(
            Node::Index {
                obj,
                optional: false,
                index,
            },
            at,
        )
    }

    fn call(&mut self, callee: NodeId, args: &[NodeId], at: u32) -> NodeId {
        let args = self.new_list(args);
        self.mk(
            Node::Call {
                callee,
                optional: false,
                type_args: None,
                args,
            },
            at,
        )
    }
    fn assign(&mut self, left: NodeId, right: NodeId, at: u32) -> NodeId {
        self.mk(
            Node::Assign {
                left,
                op: AssignOp::Assign,
                right,
            },
            at,
        )
    }

    fn binary(&mut self, left: NodeId, op: BinaryOp, right: NodeId, at: u32) -> NodeId {
        self.mk(Node::Binary { left, op, right }, at)
    }

    fn unary(&mut self, op: UnaryOp, arg: NodeId, at: u32) -> NodeId {
        self.mk(Node::Unary { op, arg }, at)
    }

    fn seq(&mut self, exprs: &[NodeId], at: u32) -> NodeId {
        if exprs.len() == 1 {
            return exprs[0];
        }
        let exprs = self.new_list(exprs);
        self.mk(Node::Seq { exprs }, at)
    }

    fn paren(&mut self, expr: NodeId, at: u32) -> NodeId {
        self.mk(Node::Paren { expr }, at)
    }

    fn expr_stmt(&mut self, expr: NodeId, at: u32) -> NodeId {
        self.mk(Node::ExprStmt { expr }, at)
    }

    fn block(&mut self, stmts: &[NodeId], at: u32) -> NodeId {
        let stmts = self.new_list(stmts);
        self.mk(Node::Block { stmts }, at)
    }

    fn ret(&mut self, arg: Option<NodeId>, at: u32) -> NodeId {
        self.mk(Node::Return { arg }, at)
    }

    /// `kind name = init;`
    fn var(
        &mut self,
        kind: VarKind,
        modifiers: Modifiers,
        name: NodeId,
        init: Option<NodeId>,
        at: u32,
    ) -> NodeId {
        let d = self.mk(
            Node::VarDeclarator {
                name,
                definite: false,
                type_ann: None,
                init,
            },
            at,
        );
        let decls = self.new_list(&[d]);
        self.mk(
            Node::VarDecl {
                modifiers,
                kind,
                decls,
            },
            at,
        )
    }

    /// A plain parameter `name`.
    fn param(&mut self, pattern: NodeId, at: u32) -> NodeId {
        self.mk(
            Node::Param {
                decorators: NodeList::EMPTY,
                modifiers: Modifiers::NONE,
                rest: false,
                pattern,
                optional: false,
                type_ann: None,
                init: None,
            },
            at,
        )
    }
    /// `function (params) { body }`
    fn function_expr(
        &mut self,
        name: Option<NodeId>,
        params: &[NodeId],
        body: &[NodeId],
        at: u32,
    ) -> NodeId {
        let params = self.new_list(params);
        let body = self.block(body, at);
        self.mk(
            Node::FunctionExpr {
                modifiers: Modifiers::NONE,
                flags: FnFlags::NONE,
                name,
                type_params: None,
                params,
                return_type: None,
                body: Some(body),
            },
            at,
        )
    }

    /// `(params) => body` with an expression body.
    fn arrow_expr(&mut self, params: &[NodeId], body: NodeId, at: u32) -> NodeId {
        let params = self.new_list(params);
        self.mk(
            Node::Arrow {
                flags: FnFlags::EXPRESSION_BODY,
                type_params: None,
                params,
                return_type: None,
                body,
            },
            at,
        )
    }
    fn object(&mut self, props: &[NodeId], at: u32) -> NodeId {
        let props = self.new_list(props);
        self.mk(Node::ObjectLit { props }, at)
    }

    fn array(&mut self, elements: &[NodeId], at: u32) -> NodeId {
        let elements = self.new_list(elements);
        self.mk(Node::ArrayLit { elements }, at)
    }

    /// `key: value` with an identifier (or quoted, if needed) key.
    fn prop(&mut self, key: &str, value: NodeId, at: u32) -> NodeId {
        let k = if ts_r_codegen::literals::is_identifier_name(key) {
            self.ident(key, at)
        } else {
            self.string(key, at)
        };
        self.mk(
            Node::Property {
                modifiers: Default::default(),
                key: k,
                value,
            },
            at,
        )
    }

    fn prop_node(&mut self, key: NodeId, value: NodeId, at: u32) -> NodeId {
        self.mk(
            Node::Property {
                modifiers: Default::default(),
                key,
                value,
            },
            at,
        )
    }

    fn spread(&mut self, arg: NodeId, at: u32) -> NodeId {
        self.mk(Node::Spread { arg }, at)
    }

    /// A method definition `kind key(params) { body }` (class or object literal).
    fn method(
        &mut self,
        modifiers: Modifiers,
        kind: MethodKind,
        key: NodeId,
        params: &[NodeId],
        body: &[NodeId],
        at: u32,
    ) -> NodeId {
        let value = self.function_expr(None, params, body, at);
        self.mk(
            Node::MethodDef {
                decorators: NodeList::EMPTY,
                modifiers,
                kind,
                key,
                optional: false,
                value,
            },
            at,
        )
    }
}

impl Build for Ast {
    fn ast_mut(&mut self) -> &mut Ast {
        self
    }
}

/// Copies node `from` into slot `to` (content and span), so every reference to `to` now sees
/// `from`'s node. This is how transforms replace a node without knowing its parent.
pub(crate) fn replace(ast: &mut Ast, to: NodeId, from: NodeId) {
    if to == from {
        return;
    }
    let n = *ast.node(from);
    let s = ast.span(from);
    *ast.node_mut(to) = n;
    ast.set_span(to, s);
}

/// Copies a node into a fresh slot (shallow), returning the new id.
pub(crate) fn clone_node(ast: &mut Ast, id: NodeId) -> NodeId {
    let n = *ast.node(id);
    let s = ast.span(id);
    ast.add(n, s)
}

/// Deep-copies a subtree (expressions used twice, e.g. `E || (E = {})`).
/// Children are cloned first, in source order, so the copy keeps children before parents.
pub(crate) fn deep_clone(ast: &mut Ast, id: NodeId) -> NodeId {
    let new = ast.map_children(id, &mut deep_clone);
    let s = ast.span(id);
    ast.add(new, s)
}
