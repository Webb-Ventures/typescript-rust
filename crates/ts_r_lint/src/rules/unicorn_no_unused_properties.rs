//! `unicorn/no-unused-properties`: properties of an object literal (or of a type literal
//! annotating a variable or parameter) that no use of the variable can read.
//!
//! For each non-global binding with one declaration and a read, its uses are followed
//! property by property: a use through `.key` (or `[key]`), an assignment or call of a
//! member, an unpredictable computed member, or a destructuring that can take the key
//! (same key, computed key, rest) passes on to the next level; any other use counts as
//! reading everything. A property no use reaches is reported; a property some use reaches
//! is checked the same way when its value is an object literal (or its type a type literal).
//! Objects with methods, accessors or `function` values are skipped, and so are exported
//! variables.

use super::prelude::*;
use super::unicorn_text::static_string_value;
use crate::semantic::{DeclKind, RefFlags, ScopeKind};
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-unused-properties",
        "Disallow unused object properties.",
        NO_PRESET,
        Category::Suspicious,
    ),
    create: create::<NoUnusedProperties>,
};

#[derive(Debug, Default)]
struct NoUnusedProperties;

impl FromOptions for NoUnusedProperties {
    fn from_options(_opts: &[Value]) -> Result<Self, String> {
        Ok(NoUnusedProperties)
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Key {
    Str(String),
    Num(f64),
}

impl Key {
    fn display(&self) -> String {
        match self {
            Key::Str(s) => s.clone(),
            Key::Num(n) => {
                if *n == n.trunc() && n.abs() < 1e21 {
                    format!("{}", *n as i64)
                } else {
                    format!("{n}")
                }
            }
        }
    }
}

fn key_name(ast: &Ast, key: NodeId) -> Option<Key> {
    match *ast.node(key) {
        Node::Ident { name } => Some(Key::Str(ast.atom(name).to_owned())),
        Node::StringLit { value } => Some(Key::Str(ast.atom(value).to_owned())),
        Node::NumberLit { value } => Some(Key::Num(value.get())),
        Node::ComputedKey { expr } => {
            let expr = skip_parens(ast, expr);
            match *ast.node(expr) {
                Node::StringLit { .. } | Node::NumberLit { .. } => key_name(ast, expr),
                _ => static_string_value(ast, expr).map(Key::Str),
            }
        }
        _ => static_string_value(ast, key).map(Key::Str),
    }
}

/// A computed key that isn't a literal or static string (`{[key]: value}`).
fn is_dynamic_key(ast: &Ast, key: NodeId) -> bool {
    matches!(ast.node(key), Node::ComputedKey { .. }) && key_name(ast, key).is_none()
}

fn keys_equal(ast: &Ast, a: NodeId, b: NodeId) -> bool {
    key_name(ast, a).is_some_and(|k| Some(k) == key_name(ast, b))
}

/// The member's property as a key: a name, or a computed literal/static string.
fn member_key(ast: &Ast, member: NodeId) -> Option<Key> {
    match *ast.node(member) {
        Node::Member { prop, .. } | Node::JsxMemberExpr { prop, .. } => key_name(ast, prop),
        Node::Index { index, .. } => {
            let index = skip_parens(ast, index);
            match *ast.node(index) {
                Node::StringLit { .. } | Node::NumberLit { .. } | Node::Ident { .. } => {
                    key_name(ast, index)
                }
                _ => static_string_value(ast, index).map(Key::Str),
            }
        }
        _ => None,
    }
}

/// The node and its parent, through parentheses and TS expression wrappers.
fn outer_parent(cx: &Ctx<'_, '_>, id: NodeId) -> (NodeId, Option<NodeId>) {
    let mut cur = id;
    for _ in 0..100_000 {
        match cx.parent(cur) {
            Some(p)
                if matches!(
                    cx.node(p),
                    Node::Paren { .. }
                        | Node::As { .. }
                        | Node::Satisfies { .. }
                        | Node::NonNull { .. }
                        | Node::TypeAssertion { .. }
                ) =>
            {
                cur = p;
            }
            p => return (cur, p),
        }
    }
    (cur, None)
}

fn pattern_matches_key(ast: &Ast, pattern: NodeId, key: NodeId) -> bool {
    let props = match *ast.node(pattern) {
        Node::ObjectPattern { props } | Node::ObjectLit { props } => props,
        _ => return false,
    };
    ast.list(props).iter().any(|&p| match *ast.node(p) {
        Node::Rest { .. } | Node::Spread { .. } => true,
        Node::BindingProperty { key: k, .. } | Node::Property { key: k, .. } => {
            is_dynamic_key(ast, k) || keys_equal(ast, k, key)
        }
        Node::ShorthandProp { name, .. } => keys_equal(ast, name, key),
        _ => false,
    })
}

/// A use of the object (a reference, or a member or declarator standing for one) followed
/// to `key`: `Some(next)` when it can reach the property, with what stands for it next.
fn follow(cx: &Ctx<'_, '_>, node: NodeId, key: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let (outer, parent) = outer_parent(cx, node);
    let Some(parent) = parent else {
        return Some(node);
    };
    match *ast.node(parent) {
        Node::Member { .. } | Node::Index { .. } | Node::JsxMemberExpr { .. } => {
            let grand = parent_skip_parens(ast, cx.file.parents, parent);
            let assigned = grand.is_some_and(|g| matches!(ast.node(g), Node::Assign { .. }));
            let called = grand.is_some_and(|g| {
                matches!(*ast.node(g), Node::Call { callee, .. } if skip_parens(ast, callee) == parent)
            });
            let unpredictable = matches!(*ast.node(parent), Node::Index { index, .. }
                if !matches!(ast.node(skip_parens(ast, index)), Node::StringLit { .. } | Node::NumberLit { .. } | Node::BigIntLit { .. } | Node::BoolLit { .. } | Node::NullLit {} | Node::RegexLit { .. })
                    && static_string_value(ast, skip_parens(ast, index)).is_none());
            let same_key = member_key(ast, parent).is_some_and(|k| Some(k) == key_name(ast, key));
            (assigned || called || unpredictable || same_key).then_some(parent)
        }
        Node::VarDeclarator {
            name,
            init: Some(init),
            ..
        } if skip_parens(ast, init) == outer || init == outer => {
            if matches!(ast.node(name), Node::ObjectPattern { .. }) {
                pattern_matches_key(ast, name, key).then_some(parent)
            } else {
                Some(node)
            }
        }
        Node::Assign { left, .. }
            if matches!(
                ast.node(left),
                Node::ObjectPattern { .. } | Node::ObjectLit { .. }
            ) && left != outer =>
        {
            pattern_matches_key(ast, left, key).then_some(parent)
        }
        _ => Some(node),
    }
}

/// The object or type literal whose properties `node` defines: a declarator's
/// (unwrapped) object initializer or, with an initializer, its type-literal annotation; a
/// parameter's type literal; a property's object value or type literal.
fn container(cx: &Ctx<'_, '_>, node: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let object = |v: Option<NodeId>| {
        v.map(|v| super::unicorn_conditions::unwrap_ts_and_parens(ast, v))
            .filter(|&v| matches!(ast.node(v), Node::ObjectLit { .. }))
    };
    let type_literal =
        |t: Option<NodeId>| t.filter(|&t| matches!(ast.node(t), Node::TypeLiteral { .. }));
    match *ast.node(node) {
        Node::VarDeclarator { init, type_ann, .. } => {
            object(init).or_else(|| init.and(type_literal(type_ann)))
        }
        Node::Param { type_ann, .. } => type_literal(type_ann),
        Node::Property { value, .. } => object(Some(value)),
        Node::PropertySignature { type_ann, .. } => type_literal(type_ann),
        _ => None,
    }
}

fn has_method(cx: &Ctx<'_, '_>, container: NodeId) -> bool {
    let ast = cx.ast();
    let Node::ObjectLit { props } = *ast.node(container) else {
        return false;
    };
    ast.list(props).iter().any(|&p| match *ast.node(p) {
        Node::MethodDef { .. } => true,
        Node::Property { value, .. } => {
            matches!(ast.node(skip_parens(ast, value)), Node::FunctionExpr { .. })
        }
        _ => false,
    })
}

fn properties(ast: &Ast, container: NodeId) -> Vec<(NodeId, NodeId)> {
    match *ast.node(container) {
        Node::ObjectLit { props } => ast
            .list(props)
            .iter()
            .filter_map(|&p| match *ast.node(p) {
                Node::Property { key, .. } => Some((p, key)),
                Node::ShorthandProp { name, .. } => Some((p, name)),
                Node::MethodDef { key, .. } => Some((p, key)),
                _ => None,
            })
            .collect(),
        Node::TypeLiteral { members } => ast
            .list(members)
            .iter()
            .filter_map(|&m| match *ast.node(m) {
                Node::PropertySignature { key, .. } => Some((m, key)),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn check_object(cx: &mut Ctx<'_, '_>, node: NodeId, uses: &[NodeId], depth: u32) {
    if depth > 100 {
        return;
    }
    let Some(container) = container(cx, node) else {
        return;
    };
    if has_method(cx, container) {
        return;
    }
    let ast = cx.ast();
    for (property, key) in properties(ast, container) {
        if is_dynamic_key(ast, key) || key_name(ast, key) == Some(Key::Str("__proto__".to_owned()))
        {
            continue;
        }
        let next: Vec<NodeId> = uses.iter().filter_map(|&u| follow(cx, u, key)).collect();
        if next.is_empty() {
            let name = key_name(ast, key).map(|k| k.display()).unwrap_or_default();
            let span = cx.span(property);
            cx.report(
                span,
                format!("Property `{name}` is defined but never used."),
            )
            .id("no-unused-properties");
        } else {
            check_object(cx, property, &next, depth + 1);
        }
    }
}

impl Rule for NoUnusedProperties {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let sem = cx.semantic();
        let symbols = sem.symbols().len();
        for s in 0..symbols {
            let sem = cx.semantic();
            let sym = &sem.symbols()[s];
            if sem.scope(sym.scope).kind == ScopeKind::Global || sym.decls.len() != 1 {
                continue;
            }
            let refs: Vec<_> = sym.refs.iter().map(|&r| sem.reference(r)).collect();
            if !refs.iter().any(|r| r.is_read()) {
                continue;
            }
            let decl = &sym.decls[0];
            let node = match decl.kind {
                DeclKind::Var | DeclKind::Let | DeclKind::Const | DeclKind::Using => {
                    let exported = cx.parent(decl.node).is_some_and(|d| {
                        matches!(*ast.node(d), Node::VarDecl { modifiers, .. } if modifiers.contains(Modifiers::EXPORT))
                            || cx.parent(d).is_some_and(|e| matches!(ast.node(e), Node::ExportNamed { .. }))
                    });
                    if exported {
                        continue;
                    }
                    decl.node
                }
                DeclKind::Param => {
                    if !matches!(*ast.node(decl.node), Node::Param { pattern, .. } if pattern == decl.name)
                    {
                        continue;
                    }
                    decl.node
                }
                _ => continue,
            };
            let uses: Vec<NodeId> = refs
                .iter()
                .filter(|r| !r.flags.contains(RefFlags::INIT))
                .map(|r| r.node)
                .collect();
            check_object(cx, node, &uses, 0);
        }
    }
}
