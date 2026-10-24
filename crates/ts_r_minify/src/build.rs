//! Helpers that append synthesized nodes to an [`Ast`].
//!
//! Synthesized nodes get an empty span; `at` is the source position used as the
//! source-map hint (`0` = unmapped), as `ts_r_codegen` expects.

use ts_r_ast::{
    AssignOp, Ast, BinaryOp, F64Bits, FnFlags, Modifiers, Node, NodeId, NodeList, Span, UnaryOp,
    VarKind,
};

#[inline]
fn sp(at: u32) -> Span {
    Span::new(at, at)
}

pub fn ident(ast: &mut Ast, name: &str, at: u32) -> NodeId {
    let name = ast.intern(name);
    ast.add(Node::Ident { name }, sp(at))
}

pub fn string(ast: &mut Ast, value: &str, at: u32) -> NodeId {
    let value = ast.intern(value);
    ast.add(Node::StringLit { value }, sp(at))
}

pub fn number(ast: &mut Ast, value: f64, at: u32) -> NodeId {
    ast.add(
        Node::NumberLit {
            value: F64Bits::new(value),
        },
        sp(at),
    )
}

pub fn boolean(ast: &mut Ast, value: bool, at: u32) -> NodeId {
    ast.add(Node::BoolLit { value }, sp(at))
}

pub fn null(ast: &mut Ast, at: u32) -> NodeId {
    ast.add(Node::NullLit {}, sp(at))
}

/// `void 0`
pub fn undefined(ast: &mut Ast, at: u32) -> NodeId {
    let zero = number(ast, 0.0, at);
    ast.add(
        Node::Unary {
            op: UnaryOp::Void,
            arg: zero,
        },
        sp(at),
    )
}

pub fn unary(ast: &mut Ast, op: UnaryOp, arg: NodeId, at: u32) -> NodeId {
    ast.add(Node::Unary { op, arg }, sp(at))
}

pub fn binary(ast: &mut Ast, left: NodeId, op: BinaryOp, right: NodeId, at: u32) -> NodeId {
    ast.add(Node::Binary { left, op, right }, sp(at))
}

pub fn assign(ast: &mut Ast, left: NodeId, right: NodeId, at: u32) -> NodeId {
    ast.add(
        Node::Assign {
            left,
            op: AssignOp::Assign,
            right,
        },
        sp(at),
    )
}

/// `obj.prop`
pub fn member(ast: &mut Ast, obj: NodeId, prop: &str, at: u32) -> NodeId {
    let prop = ident(ast, prop, at);
    ast.add(
        Node::Member {
            obj,
            optional: false,
            prop,
        },
        sp(at),
    )
}

/// `obj[index]`
pub fn index(ast: &mut Ast, obj: NodeId, index: NodeId, at: u32) -> NodeId {
    ast.add(
        Node::Index {
            obj,
            optional: false,
            index,
        },
        sp(at),
    )
}

/// `obj.prop`, or `obj["prop"]` when `prop` is not an identifier name.
pub fn prop_access(ast: &mut Ast, obj: NodeId, prop: &str, at: u32) -> NodeId {
    if crate::names::is_identifier(prop) {
        member(ast, obj, prop, at)
    } else {
        let s = string(ast, prop, at);
        index(ast, obj, s, at)
    }
}

pub fn call(ast: &mut Ast, callee: NodeId, args: &[NodeId], at: u32) -> NodeId {
    let args = ast.add_list(args);
    ast.add(
        Node::Call {
            callee,
            optional: false,
            type_args: None,
            args,
        },
        sp(at),
    )
}

/// `name(args)`
pub fn call_name(ast: &mut Ast, name: &str, args: &[NodeId], at: u32) -> NodeId {
    let callee = ident(ast, name, at);
    call(ast, callee, args, at)
}

pub fn seq(ast: &mut Ast, exprs: &[NodeId], at: u32) -> NodeId {
    if exprs.len() == 1 {
        return exprs[0];
    }
    let exprs = ast.add_list(exprs);
    ast.add(Node::Seq { exprs }, sp(at))
}

pub fn paren(ast: &mut Ast, expr: NodeId, at: u32) -> NodeId {
    ast.add(Node::Paren { expr }, sp(at))
}

pub fn expr_stmt(ast: &mut Ast, expr: NodeId, at: u32) -> NodeId {
    ast.add(Node::ExprStmt { expr }, sp(at))
}

pub fn block(ast: &mut Ast, stmts: &[NodeId], at: u32) -> NodeId {
    let stmts = ast.add_list(stmts);
    ast.add(Node::Block { stmts }, sp(at))
}

pub fn ret(ast: &mut Ast, arg: Option<NodeId>, at: u32) -> NodeId {
    ast.add(Node::Return { arg }, sp(at))
}

pub fn empty(ast: &mut Ast, at: u32) -> NodeId {
    ast.add(Node::Empty {}, sp(at))
}

/// `kind name = init` with one declarator (`name` is an identifier or a pattern node).
pub fn var_decl(
    ast: &mut Ast,
    kind: VarKind,
    name: NodeId,
    init: Option<NodeId>,
    at: u32,
) -> NodeId {
    var_decl_many(ast, kind, &[(name, init)], at)
}

/// `kind a = 1, b, ...`
pub fn var_decl_many(
    ast: &mut Ast,
    kind: VarKind,
    decls: &[(NodeId, Option<NodeId>)],
    at: u32,
) -> NodeId {
    let mut ids = Vec::with_capacity(decls.len());
    for &(name, init) in decls {
        ids.push(ast.add(
            Node::VarDeclarator {
                name,
                definite: false,
                type_ann: None,
                init,
            },
            sp(at),
        ));
    }
    let decls = ast.add_list(&ids);
    ast.add(
        Node::VarDecl {
            modifiers: Modifiers::NONE,
            kind,
            decls,
        },
        sp(at),
    )
}

/// A plain parameter.
pub fn param(ast: &mut Ast, pattern: NodeId, at: u32) -> NodeId {
    ast.add(
        Node::Param {
            decorators: NodeList::EMPTY,
            modifiers: Modifiers::NONE,
            rest: false,
            pattern,
            optional: false,
            type_ann: None,
            init: None,
        },
        sp(at),
    )
}

/// `(params) => { body }` (or an async arrow).
pub fn arrow(ast: &mut Ast, params: &[NodeId], body: &[NodeId], is_async: bool, at: u32) -> NodeId {
    let params = ast.add_list(params);
    let body = block(ast, body, at);
    let flags = if is_async {
        FnFlags::ASYNC
    } else {
        FnFlags::NONE
    };
    ast.add(
        Node::Arrow {
            flags,
            type_params: None,
            params,
            return_type: None,
            body,
        },
        sp(at),
    )
}

/// `() => expr`
pub fn arrow_expr(ast: &mut Ast, params: &[NodeId], expr: NodeId, at: u32) -> NodeId {
    let params = ast.add_list(params);
    ast.add(
        Node::Arrow {
            flags: FnFlags::EXPRESSION_BODY,
            type_params: None,
            params,
            return_type: None,
            body: expr,
        },
        sp(at),
    )
}

/// `function (params) { body }`
pub fn function_expr(ast: &mut Ast, params: &[NodeId], body: &[NodeId], at: u32) -> NodeId {
    let params = ast.add_list(params);
    let body = block(ast, body, at);
    ast.add(
        Node::FunctionExpr {
            modifiers: Modifiers::NONE,
            flags: FnFlags::NONE,
            name: None,
            type_params: None,
            params,
            return_type: None,
            body: Some(body),
        },
        sp(at),
    )
}

/// `{ key: value, ... }` with identifier-or-string keys. A `__proto__` key is computed
/// (`["__proto__"]`) so it creates a property instead of setting the prototype.
pub fn object(ast: &mut Ast, props: &[(&str, NodeId)], at: u32) -> NodeId {
    let mut ids = Vec::with_capacity(props.len());
    for &(k, v) in props {
        let key = if k == "__proto__" {
            let expr = string(ast, k, at);
            ast.add(Node::ComputedKey { expr }, sp(at))
        } else if crate::names::is_identifier(k) {
            ident(ast, k, at)
        } else {
            string(ast, k, at)
        };
        ids.push(ast.add(
            Node::Property {
                modifiers: Modifiers::NONE,
                key,
                value: v,
            },
            sp(at),
        ));
    }
    let props = ast.add_list(&ids);
    ast.add(Node::ObjectLit { props }, sp(at))
}

pub fn array(ast: &mut Ast, elements: &[NodeId], at: u32) -> NodeId {
    let elements = ast.add_list(elements);
    ast.add(Node::ArrayLit { elements }, sp(at))
}

/// Replaces the node in slot `slot` with `node` (parents keep pointing at the slot).
pub fn replace(ast: &mut Ast, slot: NodeId, node: NodeId) {
    if slot == node {
        return;
    }
    let n = *ast.node(node);
    let span = ast.span(node);
    *ast.node_mut(slot) = n;
    ast.set_span(slot, span);
}

/// Copies the node in `slot` to a new slot (children shared) and returns the copy. Use it
/// before wrapping a node in place: `replace(slot, wrapper(copy))`.
pub fn move_out(ast: &mut Ast, slot: NodeId) -> NodeId {
    let n = *ast.node(slot);
    let span = ast.span(slot);
    ast.add(n, span)
}

/// Sets the statement list of the `Program` root.
pub fn set_program(ast: &mut Ast, stmts: &[NodeId]) {
    let list = ast.add_list(stmts);
    let root = ast.root;
    if let Node::Program { stmts } = ast.node_mut(root) {
        *stmts = list;
    }
}

/// The statement list of the `Program` root.
pub fn program_stmts(ast: &Ast) -> Vec<NodeId> {
    match *ast.node(ast.root) {
        Node::Program { stmts } => ast.list(stmts).to_vec(),
        _ => Vec::new(),
    }
}
