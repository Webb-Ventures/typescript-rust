//! More helpers for the eslint-plugin-unicorn rules: reference equality (`isSameReference`)
//! and flattening `&&` / `||` chains.

use super::prelude::*;
use super::unicorn_static::{Static, static_value};
use ts_r_ast::Ast;

/// Nesting limit for [`is_same_reference`].
const MAX_DEPTH: u32 = 200;

/// Strips parentheses, TS wrappers (`as`, `satisfies`, `!`, `<T>x`, `f<T>`).
pub(crate) fn unwrap_ts(ast: &Ast, mut id: NodeId) -> NodeId {
    loop {
        match *ast.node(id) {
            Node::Paren { expr }
            | Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::NonNull { expr }
            | Node::TypeAssertion { expr, .. }
            | Node::Instantiation { expr, .. } => id = expr,
            _ => return id,
        }
    }
}

/// A string literal's value or a substitution-free template's cooked text.
fn static_string(ast: &Ast, id: NodeId) -> Option<String> {
    match *ast.node(id) {
        Node::StringLit { value } => Some(ast.atom(value).to_owned()),
        Node::TemplateLit { parts } => match ast.list(parts) {
            &[only] => match *ast.node(only) {
                Node::TemplateElement {
                    cooked,
                    invalid_escape: false,
                } => Some(ast.atom(cooked).to_owned()),
                _ => None,
            },
            _ => None,
        },
        _ => None,
    }
}

/// The static name of a member access: `a.b`, `a["b"]`, `a[0]`, `a[`b`]`.
fn static_property_name(cx: &Ctx<'_, '_>, id: NodeId) -> Option<String> {
    match *cx.node(id) {
        Node::Member { prop, .. } => match *cx.node(prop) {
            Node::Ident { name } => Some(cx.ast().atom(name).to_owned()),
            _ => None,
        },
        Node::Index { index, .. } => static_value(cx, index).and_then(|v| v.to_js_string()),
        _ => None,
    }
}

/// `isSameReference`: both sides (through parentheses and TS wrappers) are the same static
/// string, `this`, `super`, the same name, equal literals, or member accesses with the same
/// object and the same (static, or same computed) property.
pub(crate) fn is_same_reference(cx: &Ctx<'_, '_>, a: NodeId, b: NodeId) -> bool {
    same_ref(cx, a, b, 0)
}

fn same_ref(cx: &Ctx<'_, '_>, a: NodeId, b: NodeId, depth: u32) -> bool {
    if depth > MAX_DEPTH {
        return false;
    }
    let ast = cx.ast();
    let (a, b) = (unwrap_ts(ast, a), unwrap_ts(ast, b));
    if let Some(s) = static_string(ast, a) {
        return static_string(ast, b).as_ref() == Some(&s);
    }
    let member = |id| matches!(cx.node(id), Node::Member { .. } | Node::Index { .. });
    if member(a) && member(b) {
        let (oa, ob) = match (*cx.node(a), *cx.node(b)) {
            (
                Node::Member { obj: x, .. } | Node::Index { obj: x, .. },
                Node::Member { obj: y, .. } | Node::Index { obj: y, .. },
            ) => (x, y),
            _ => return false,
        };
        if let Some(name) = static_property_name(cx, a) {
            return same_ref(cx, oa, ob, depth + 1)
                && static_property_name(cx, b).as_ref() == Some(&name);
        }
        return match (*cx.node(a), *cx.node(b)) {
            (Node::Member { prop: p, .. }, Node::Member { prop: q, .. })
            | (Node::Index { index: p, .. }, Node::Index { index: q, .. }) => {
                same_ref(cx, oa, ob, depth + 1) && same_ref(cx, p, q, depth + 1)
            }
            _ => false,
        };
    }
    match (*cx.node(a), *cx.node(b)) {
        (Node::This {}, Node::This {}) | (Node::Super {}, Node::Super {}) => true,
        (Node::Ident { name: x }, Node::Ident { name: y })
        | (Node::PrivateName { name: x }, Node::PrivateName { name: y }) => x == y,
        (Node::NumberLit { .. }, Node::NumberLit { .. })
        | (Node::BigIntLit { .. }, Node::BigIntLit { .. })
        | (Node::BoolLit { .. }, Node::BoolLit { .. })
        | (Node::NullLit {}, Node::NullLit {})
        | (Node::RegexLit { .. }, Node::RegexLit { .. }) => literal_eq(cx, a, b),
        _ => false,
    }
}

/// Literal equality as the plugin compares them: regexes and bigints by their text,
/// everything else by value.
fn literal_eq(cx: &Ctx<'_, '_>, a: NodeId, b: NodeId) -> bool {
    match (*cx.node(a), *cx.node(b)) {
        (
            Node::RegexLit {
                pattern: p,
                flags: f,
            },
            Node::RegexLit {
                pattern: q,
                flags: g,
            },
        ) => p == q && f == g,
        (Node::BigIntLit { value: x }, Node::BigIntLit { value: y }) => x == y,
        _ => match (static_value(cx, a), static_value(cx, b)) {
            (Some(Static::Num(x)), Some(Static::Num(y))) => x == y,
            (Some(Static::Bool(x)), Some(Static::Bool(y))) => x == y,
            (Some(Static::Null), Some(Static::Null)) => true,
            _ => false,
        },
    }
}

/// The operands of a chain of `op` (left to right, through parentheses, not through TS
/// wrappers): `a && (b && c)` gives `[a, b, c]`.
pub(crate) fn logical_operands(ast: &Ast, id: NodeId, op: BinaryOp) -> Vec<NodeId> {
    let mut out = Vec::new();
    let mut stack = vec![id];
    while let Some(n) = stack.pop() {
        match *ast.node(skip_parens(ast, n)) {
            Node::Binary { left, op: o, right } if o == op => {
                stack.push(right);
                stack.push(left);
            }
            _ => out.push(skip_parens(ast, n)),
        }
    }
    out
}

/// A logical expression that is not an operand of the same operator around it.
pub(crate) fn is_outermost_logical(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let Node::Binary { op, .. } = *cx.node(id) else {
        return true;
    };
    let ast = cx.ast();
    let parent = parent_skip_parens(ast, cx.file.parents, id);
    !parent.is_some_and(|p| matches!(*ast.node(p), Node::Binary { op: o, .. } if o == op))
}
