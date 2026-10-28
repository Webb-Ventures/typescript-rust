//! `unicorn/no-thenable`: a `then` property makes a value look like a promise. Reported:
//! object members and class fields/methods/accessors named `then` (also a computed key or
//! a `const` that evaluates to `"then"`), `x.then = ...` assignments,
//! `Object.defineProperty(x, "then", ...)` / `Reflect.defineProperty`, `["then", ...]` pairs
//! in `Object.fromEntries([...])`, and named exports called `then`. Abstract class members
//! are left alone (typescript-eslint gives them other node types).
//!
//! Under espree a shorthand `export {then}` is one identifier node playing both roles, so
//! it is reported twice; typescript-eslint makes two nodes and reports once.

use super::prelude::*;
use super::unicorn_static::{Static, static_value_control_flow};
use ts_r_ast::Modifiers;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-thenable",
        "Disallow `then` property.",
        NO_PRESET,
        Category::Correctness,
    ),
    create: create_default::<NoThenable>,
};

#[derive(Debug, Default)]
struct NoThenable;

const OBJECT: &str = "Do not add `then` to an object.";
const CLASS: &str = "Do not add `then` to a class.";
const EXPORT: &str = "Do not export `then`.";

fn is_string_then(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    matches!(static_value_control_flow(cx, id), Some(Static::Str(s)) if s == "then")
}

/// The node to report when the property key `key` names `then`.
fn then_key(cx: &Ctx<'_, '_>, key: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    match *ast.node(key) {
        Node::Ident { .. } => (ident_name(ast, key) == Some("then")).then_some(key),
        Node::StringLit { value } => (ast.atom(value) == "then").then_some(key),
        Node::ComputedKey { expr } => {
            let expr = skip_parens(ast, expr);
            match *ast.node(expr) {
                Node::StringLit { value } => (ast.atom(value) == "then").then_some(expr),
                _ => is_string_then(cx, expr).then_some(expr),
            }
        }
        _ => None,
    }
}

fn report(cx: &mut Ctx<'_, '_>, node: NodeId, message: &'static str) {
    let id = match message {
        OBJECT => "no-thenable-object",
        CLASS => "no-thenable-class",
        _ => "no-thenable-export",
    };
    cx.report_node(node, message).id(id);
}

/// `Object.method(...)` / `Reflect.method(...)` with a plain member and call.
fn static_method_call(cx: &Ctx<'_, '_>, call: NodeId, objects: &[&str], method: &str) -> bool {
    let ast = cx.ast();
    let Node::Call {
        callee,
        optional: false,
        ..
    } = *ast.node(call)
    else {
        return false;
    };
    matches!(*ast.node(callee), Node::Member { obj, optional: false, prop }
        if ident_name(ast, obj).is_some_and(|o| objects.contains(&o))
            && ident_name(ast, prop) == Some(method))
}

fn is_exported(m: Modifiers) -> bool {
    m.contains(Modifiers::EXPORT) && !m.contains(Modifiers::DEFAULT)
}

impl Rule for NoThenable {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::ObjectLit,
            NodeKind::PropertyDef,
            NodeKind::MethodDef,
            NodeKind::Member,
            NodeKind::Index,
            NodeKind::Call,
            NodeKind::ExportSpecifier,
            NodeKind::FunctionDecl,
            NodeKind::ClassDecl,
            NodeKind::VarDecl,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        match *cx.node(node) {
            Node::ObjectLit { props } => {
                for &p in ast.list(props) {
                    let key = match *ast.node(p) {
                        Node::Property { key, .. } | Node::MethodDef { key, .. } => key,
                        Node::ShorthandProp { name, .. } => name,
                        _ => continue,
                    };
                    if let Some(r) = then_key(cx, key) {
                        report(cx, r, OBJECT);
                    }
                }
            }
            Node::PropertyDef { modifiers, key, .. } | Node::MethodDef { modifiers, key, .. } => {
                let in_class = cx
                    .parent(node)
                    .is_some_and(|p| !matches!(cx.node(p), Node::ObjectLit { .. }));
                if in_class
                    && !modifiers.contains(Modifiers::ABSTRACT)
                    && let Some(r) = then_key(cx, key)
                {
                    report(cx, r, CLASS);
                }
            }
            Node::Member { prop, .. } | Node::Index { index: prop, .. } => {
                let Some(assign) = parent_skip_parens(ast, cx.file.parents, node) else {
                    return;
                };
                let Node::Assign { left, .. } = *ast.node(assign) else {
                    return;
                };
                if skip_parens(ast, left) != node {
                    return;
                }
                let prop = skip_parens(ast, prop);
                let hit = match *ast.node(prop) {
                    Node::StringLit { value } => ast.atom(value) == "then",
                    _ if matches!(cx.node(node), Node::Member { .. }) => {
                        ident_name(ast, prop) == Some("then")
                    }
                    _ => is_string_then(cx, prop),
                };
                if hit {
                    report(cx, prop, OBJECT);
                }
            }
            Node::Call { args, .. } => {
                let args = ast.list(args);
                if static_method_call(cx, node, &["Object", "Reflect"], "defineProperty")
                    && args.len() >= 3
                    && !matches!(ast.node(args[0]), Node::Spread { .. })
                    && is_string_then(cx, args[1])
                {
                    report(cx, args[1], OBJECT);
                }
                if !static_method_call(cx, node, &["Object"], "fromEntries") || args.len() != 1 {
                    return;
                }
                let Node::ArrayLit { elements } = *ast.node(args[0]) else {
                    return;
                };
                for &pair in ast.list(elements) {
                    let Node::ArrayLit { elements } = *ast.node(pair) else {
                        continue;
                    };
                    let Some(&key) = ast.list(elements).first() else {
                        continue;
                    };
                    if matches!(ast.node(key), Node::Hole {} | Node::Spread { .. }) {
                        continue;
                    }
                    if is_string_then(cx, key) {
                        report(cx, key, OBJECT);
                    }
                }
            }
            Node::ExportSpecifier {
                local, exported, ..
            } => match exported {
                Some(e) => {
                    if ident_name(ast, e) == Some("then") {
                        report(cx, e, EXPORT);
                    }
                }
                None => {
                    if ident_name(ast, local) == Some("then") {
                        report(cx, local, EXPORT);
                        if !cx.env().typescript {
                            report(cx, local, EXPORT);
                        }
                    }
                }
            },
            Node::FunctionDecl {
                modifiers,
                name: Some(name),
                ..
            }
            | Node::ClassDecl {
                modifiers,
                name: Some(name),
                ..
            } => {
                if is_exported(modifiers) && ident_name(ast, name) == Some("then") {
                    report(cx, name, EXPORT);
                }
            }
            Node::VarDecl {
                modifiers, decls, ..
            } => {
                if !is_exported(modifiers) {
                    return;
                }
                let mut names = Vec::new();
                for &d in ast.list(decls) {
                    if let Node::VarDeclarator { name, .. } = *ast.node(d) {
                        binding_idents(ast, name, &mut names);
                    }
                }
                for n in names {
                    if ident_name(ast, n) == Some("then") {
                        report(cx, n, EXPORT);
                    }
                }
            }
            _ => {}
        }
    }
}
