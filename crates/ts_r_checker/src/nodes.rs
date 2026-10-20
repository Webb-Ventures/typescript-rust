//! Small AST helpers shared by the checker modules.

use ts_r_ast::{Ast, FnFlags, KeywordTypeKind, Modifiers, Node, NodeId, NodeList, UnaryOp};
use ts_r_binder::js_number_to_string;

/// The parts of any function-like node.
#[derive(Clone, Copy, Debug)]
pub struct FnParts {
    pub type_params: Option<NodeId>,
    pub params: NodeList,
    pub return_type: Option<NodeId>,
    pub body: Option<NodeId>,
    pub flags: FnFlags,
    pub is_arrow: bool,
    pub is_construct: bool,
}

/// Function-like parts of `n` (functions, arrows, method values, signatures, function
/// types). `MethodDef` resolves to its `FunctionExpr` value.
pub fn fn_parts(ast: &Ast, n: NodeId) -> Option<FnParts> {
    Some(match *ast.node(n) {
        Node::FunctionDecl {
            flags,
            type_params,
            params,
            return_type,
            body,
            ..
        }
        | Node::FunctionExpr {
            flags,
            type_params,
            params,
            return_type,
            body,
            ..
        } => FnParts {
            type_params,
            params,
            return_type,
            body,
            flags,
            is_arrow: false,
            is_construct: false,
        },
        Node::Arrow {
            flags,
            type_params,
            params,
            return_type,
            body,
        } => FnParts {
            type_params,
            params,
            return_type,
            body: Some(body),
            flags,
            is_arrow: true,
            is_construct: false,
        },
        Node::MethodDef { value, .. } => return fn_parts(ast, value),
        Node::MethodSignature {
            type_params,
            params,
            return_type,
            ..
        }
        | Node::CallSignature {
            type_params,
            params,
            return_type,
        } => FnParts {
            type_params,
            params,
            return_type,
            body: None,
            flags: FnFlags::NONE,
            is_arrow: false,
            is_construct: false,
        },
        Node::ConstructSignature {
            type_params,
            params,
            return_type,
        } => FnParts {
            type_params,
            params,
            return_type,
            body: None,
            flags: FnFlags::NONE,
            is_arrow: false,
            is_construct: true,
        },
        Node::FunctionType {
            type_params,
            params,
            return_type,
        } => FnParts {
            type_params,
            params,
            return_type: Some(return_type),
            body: None,
            flags: FnFlags::NONE,
            is_arrow: false,
            is_construct: false,
        },
        Node::ConstructorType {
            type_params,
            params,
            return_type,
            ..
        } => FnParts {
            type_params,
            params,
            return_type: Some(return_type),
            body: None,
            flags: FnFlags::NONE,
            is_arrow: false,
            is_construct: true,
        },
        _ => return None,
    })
}

pub fn is_function_like(ast: &Ast, n: NodeId) -> bool {
    matches!(
        ast.node(n),
        Node::FunctionDecl { .. } | Node::FunctionExpr { .. } | Node::Arrow { .. }
    )
}

/// The `TypeParams` node of a declaration that has one.
pub fn type_params_of(ast: &Ast, n: NodeId) -> Option<NodeId> {
    match *ast.node(n) {
        Node::ClassDecl { type_params, .. }
        | Node::ClassExpr { type_params, .. }
        | Node::Interface { type_params, .. }
        | Node::TypeAlias { type_params, .. } => type_params,
        Node::MappedType { .. } | Node::InferType { .. } => None,
        _ => fn_parts(ast, n).and_then(|p| p.type_params),
    }
}

pub fn skip_parens(ast: &Ast, mut e: NodeId) -> NodeId {
    let mut guard = 0;
    while let Node::Paren { expr } = *ast.node(e) {
        e = expr;
        guard += 1;
        if guard > 10_000 {
            break;
        }
    }
    e
}

pub fn skip_type_parens(ast: &Ast, mut e: NodeId) -> NodeId {
    let mut guard = 0;
    while let Node::ParenType { ty } = *ast.node(e) {
        e = ty;
        guard += 1;
        if guard > 10_000 {
            break;
        }
    }
    e
}

/// Skips parentheses, `as`, `satisfies`, `!` and type assertions.
pub fn skip_outer_expressions(ast: &Ast, mut e: NodeId) -> NodeId {
    for _ in 0..10_000 {
        match *ast.node(e) {
            Node::Paren { expr }
            | Node::NonNull { expr }
            | Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::TypeAssertion { expr, .. } => e = expr,
            _ => break,
        }
    }
    e
}

/// The static text of a property key (identifier, string, number, private name, literal
/// computed key).
pub fn key_text(ast: &Ast, key: NodeId) -> Option<String> {
    match *ast.node(key) {
        Node::Ident { name } | Node::StringLit { value: name } => Some(ast.atom(name).to_owned()),
        Node::PrivateName { name } => Some(format!("#{}", ast.atom(name))),
        Node::NumberLit { value } => Some(js_number_to_string(value.get())),
        Node::BigIntLit { value } => Some(ast.atom(value).to_owned()),
        // A template without substitutions (`` o[`kind`] ``).
        Node::TemplateLit { parts } => match ast.list(parts) {
            [p] => match *ast.node(*p) {
                Node::TemplateElement { cooked, .. } => Some(ast.atom(cooked).to_owned()),
                _ => None,
            },
            _ => None,
        },
        Node::ComputedKey { expr } => {
            let e = skip_parens(ast, expr);
            match *ast.node(e) {
                Node::StringLit { .. } | Node::NumberLit { .. } => key_text(ast, e),
                Node::TemplateLit { parts } => match ast.list(parts) {
                    [p] => match *ast.node(*p) {
                        Node::TemplateElement { cooked, .. } => Some(ast.atom(cooked).to_owned()),
                        _ => None,
                    },
                    _ => None,
                },
                _ => None,
            }
        }
        _ => None,
    }
}

/// Whether a computed key's expression may late-bind a member name: an identifier or a
/// dotted chain of identifiers (`k`, `Symbol.iterator`, `ns.C.key`), with no parentheses.
pub fn is_entity_name_expr(ast: &Ast, n: NodeId) -> bool {
    let mut cur = n;
    for _ in 0..1000 {
        match *ast.node(cur) {
            Node::Ident { .. } => return true,
            Node::Member {
                obj,
                optional: false,
                prop,
            } if matches!(ast.node(prop), Node::Ident { .. }) => cur = obj,
            _ => return false,
        }
    }
    false
}

/// Whether a computed key names its member statically (string, number or no-substitution
/// template literal, or a signed number literal like `[-1]`).
pub fn is_literal_computed_key(ast: &Ast, key: NodeId) -> bool {
    let Node::ComputedKey { expr } = *ast.node(key) else {
        return true;
    };
    match *ast.node(expr) {
        Node::StringLit { .. } | Node::NumberLit { .. } => true,
        Node::TemplateLit { parts } => ast.list(parts).len() == 1,
        Node::Unary { op, arg } => {
            matches!(op, UnaryOp::Minus | UnaryOp::Plus)
                && matches!(ast.node(arg), Node::NumberLit { .. })
        }
        _ => false,
    }
}

pub fn ident_text(ast: &Ast, n: NodeId) -> Option<&str> {
    match *ast.node(n) {
        Node::Ident { name } => Some(ast.atom(name)),
        _ => None,
    }
}

pub fn is_keyword(ast: &Ast, n: NodeId, k: KeywordTypeKind) -> bool {
    matches!(*ast.node(n), Node::KeywordType { kind } if kind == k)
}

pub fn has_modifier(ast: &Ast, n: NodeId, m: Modifiers) -> bool {
    ast.node(n).modifiers().intersects(m)
}

/// `this` as a parameter name.
pub fn is_this_param(ast: &Ast, p: NodeId) -> bool {
    match *ast.node(p) {
        Node::Param { pattern, .. } => {
            matches!(*ast.node(pattern), Node::Ident { name } if ast.atom(name) == "this")
        }
        _ => false,
    }
}

/// The text of an entity name (`A.B.C`) or property access chain.
pub fn entity_text(ast: &Ast, n: NodeId) -> String {
    match *ast.node(n) {
        Node::Ident { name } => ast.atom(name).to_owned(),
        Node::QualifiedName { left, right } => {
            format!("{}.{}", entity_text(ast, left), entity_text(ast, right))
        }
        Node::Member { obj, prop, .. } => {
            format!("{}.{}", entity_text(ast, obj), entity_text(ast, prop))
        }
        Node::This {} => "this".to_owned(),
        Node::PrivateName { name } => format!("#{}", ast.atom(name)),
        Node::ExprWithTypeArgs { expr, .. } => entity_text(ast, expr),
        Node::Paren { expr } => entity_text(ast, expr),
        _ => String::new(),
    }
}
