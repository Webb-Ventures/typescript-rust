//! `unicorn/no-incomplete-accessor-override`: a getter (setter) in a subclass whose class
//! defines no matching setter (getter) for the name, while the nearest ancestor class that
//! defines the name has the other accessor (or both, or an `accessor` field): the
//! inherited half is hidden.
//!
//! Ancestors are followed through `extends` to class declarations, class expressions and
//! `const` aliases of them (each binding declared once and never reassigned); a
//! `declare` or decorated class ends the walk. A member's name is its key, or the static
//! value of a computed key (`unknown` when the value is not known or is an object or
//! function, which makes the answer unknown; symbols never match). A class's kind for a
//! name folds its own runtime members in order: a field makes it `data`, a getter and a
//! setter together `both`, a decorated member `unknown`. The default own properties
//! (`constructor`; static `name` and `length`) are not checked. One report per name.

use super::prelude::*;
use super::unicorn_static::{Static, static_value};
use rustc_hash::FxHashSet;
use ts_r_ast::{Ast, HeritageKind, NodeList};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-incomplete-accessor-override",
        "Disallow class accessors that hide an inherited getter or setter.",
        NO_PRESET,
        Category::Correctness,
    ),
    create: create_default::<NoIncompleteAccessorOverride>,
};

#[derive(Debug, Default)]
struct NoIncompleteAccessorOverride;

/// A member's name: known, unknown, or one that never matches (private names, symbols).
#[derive(Debug, PartialEq)]
enum Name {
    Known(String),
    Unknown,
    Never,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Get,
    Set,
    Both,
    Data,
    Unknown,
}

fn super_class(ast: &Ast, class: NodeId) -> Option<NodeId> {
    let (Node::ClassDecl { heritage, .. } | Node::ClassExpr { heritage, .. }) = *ast.node(class)
    else {
        return None;
    };
    for &h in ast.list(heritage) {
        if let Node::HeritageClause {
            kind: HeritageKind::Extends,
            types,
        } = *ast.node(h)
            && let Some(&t) = ast.list(types).first()
        {
            return Some(match *ast.node(t) {
                Node::ExprWithTypeArgs { expr, .. } => expr,
                _ => t,
            });
        }
    }
    None
}

/// The class `expr` names, through `const` aliases; `None` for `declare` or decorated
/// classes and anything else.
fn class_of(cx: &Ctx<'_, '_>, expr: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let sem = cx.semantic();
    // Each step visits a new symbol, so the walk ends.
    let mut visited: FxHashSet<SymbolId> = FxHashSet::default();
    let mut cur = expr;
    loop {
        cur = skip_parens(ast, cur);
        match *ast.node(cur) {
            Node::ClassDecl {
                decorators,
                modifiers,
                ..
            }
            | Node::ClassExpr {
                decorators,
                modifiers,
                ..
            } => {
                return (!modifiers.contains(Modifiers::DECLARE)
                    && ast.list(decorators).is_empty())
                .then_some(cur);
            }
            Node::Ident { .. } => {}
            _ => return None,
        }
        let s = sem.symbol_of_ident(cur)?;
        if !visited.insert(s) {
            return None;
        }
        let sym = sem.symbol(s);
        let [decl] = sym.decls.as_slice() else {
            return None;
        };
        if sym.refs.iter().any(|&r| {
            let r = sem.reference(r);
            r.is_write() && !r.flags.contains(RefFlags::INIT)
        }) {
            return None;
        }
        match decl.kind {
            DeclKind::Class | DeclKind::ClassExprName => {
                cur = decl.node;
                if !matches!(ast.kind(cur), NodeKind::ClassDecl | NodeKind::ClassExpr) {
                    return None;
                }
            }
            DeclKind::Const => match *ast.node(decl.node) {
                Node::VarDeclarator {
                    name,
                    init: Some(init),
                    ..
                } if name == decl.name => cur = init,
                _ => return None,
            },
            _ => return None,
        }
    }
}

fn member_name(cx: &Ctx<'_, '_>, key: NodeId) -> Name {
    let ast = cx.ast();
    match *ast.node(key) {
        Node::PrivateName { .. } => Name::Never,
        Node::Ident { name } => Name::Known(ast.atom(name).to_owned()),
        Node::ComputedKey { expr } => match static_value(cx, expr) {
            None => Name::Unknown,
            Some(Static::Builtin(p)) if p.starts_with("Symbol.") => Name::Never,
            Some(
                Static::Builtin(_) | Static::Object(_) | Static::Array(_) | Static::RegExp { .. },
            ) => Name::Unknown,
            Some(v) => v.to_js_string().map_or(Name::Unknown, Name::Known),
        },
        _ => static_value(cx, key)
            .and_then(|v| v.to_js_string())
            .map_or(Name::Unknown, Name::Known),
    }
}

/// A runtime member (method, field, `accessor` field; not `declare`) with its static flag.
fn runtime_member(ast: &Ast, m: NodeId) -> Option<(Modifiers, NodeId, NodeList)> {
    match *ast.node(m) {
        Node::MethodDef {
            modifiers,
            key,
            decorators,
            ..
        }
        | Node::PropertyDef {
            modifiers,
            key,
            decorators,
            ..
        } if !modifiers.contains(Modifiers::DECLARE) => Some((modifiers, key, decorators)),
        _ => None,
    }
}

fn has_body(ast: &Ast, method: NodeId) -> bool {
    matches!(*ast.node(method), Node::MethodDef { value, .. }
        if matches!(*ast.node(value), Node::FunctionExpr { body: Some(_), .. }))
}

fn member_kind(cx: &Ctx<'_, '_>, m: NodeId, name: &str) -> Option<Kind> {
    let ast = cx.ast();
    let (modifiers, key, decorators) = runtime_member(ast, m)?;
    let method_kind = match *ast.node(m) {
        Node::MethodDef { kind, .. } => {
            if !has_body(ast, m) {
                return None;
            }
            Some(kind)
        }
        _ => None,
    };
    match member_name(cx, key) {
        Name::Unknown => return Some(Kind::Unknown),
        Name::Known(n) if n == name => {}
        _ => return None,
    }
    if !ast.list(decorators).is_empty() {
        return Some(Kind::Unknown);
    }
    Some(match method_kind {
        None if modifiers.contains(Modifiers::ACCESSOR) => Kind::Both,
        Some(MethodKind::Get) => Kind::Get,
        Some(MethodKind::Set) => Kind::Set,
        _ => Kind::Data,
    })
}

fn class_members(ast: &Ast, class: NodeId) -> &[NodeId] {
    match *ast.node(class) {
        Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. } => ast.list(members),
        _ => &[],
    }
}

/// The kind of the property `name` a class defines (static or on instances).
fn descriptor_kind(cx: &Ctx<'_, '_>, class: NodeId, name: &str, is_static: bool) -> Option<Kind> {
    let ast = cx.ast();
    let mut out: Option<Kind> = None;
    for &m in class_members(ast, class) {
        let Some((modifiers, ..)) = runtime_member(ast, m) else {
            continue;
        };
        if modifiers.contains(Modifiers::STATIC) != is_static {
            continue;
        }
        let kind = member_kind(cx, m, name);
        match kind {
            Some(Kind::Unknown) => return kind,
            Some(Kind::Data) if ast.kind(m) == NodeKind::PropertyDef => return kind,
            Some(k @ (Kind::Get | Kind::Set)) => {
                let opposite = if k == Kind::Get { Kind::Set } else { Kind::Get };
                out = if out == Some(Kind::Both) || out == Some(opposite) {
                    Some(Kind::Both)
                } else {
                    Some(k)
                };
            }
            Some(k) => out = Some(k),
            None => {}
        }
    }
    out
}

fn inherited_kind(cx: &Ctx<'_, '_>, class: NodeId, name: &str, is_static: bool) -> Option<Kind> {
    let ast = cx.ast();
    // Each step visits a new class, so the walk ends.
    let mut visited: FxHashSet<NodeId> = FxHashSet::default();
    let mut cur = class;
    loop {
        let sup = super_class(ast, cur)?;
        cur = class_of(cx, sup)?;
        if !visited.insert(cur) {
            return None;
        }
        if let Some(k) = descriptor_kind(cx, cur, name, is_static) {
            return Some(k);
        }
    }
}

impl Rule for NoIncompleteAccessorOverride {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::ClassDecl, NodeKind::ClassExpr]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let (Node::ClassDecl {
            decorators,
            modifiers,
            ..
        }
        | Node::ClassExpr {
            decorators,
            modifiers,
            ..
        }) = *cx.node(node)
        else {
            return;
        };
        if super_class(ast, node).is_none()
            || modifiers.contains(Modifiers::DECLARE)
            || !ast.list(decorators).is_empty()
        {
            return;
        }
        let mut reported: [Vec<String>; 2] = [Vec::new(), Vec::new()];
        for &m in class_members(ast, node) {
            let Node::MethodDef {
                kind,
                key,
                modifiers,
                ..
            } = *ast.node(m)
            else {
                continue;
            };
            let this_kind = match kind {
                MethodKind::Get => Kind::Get,
                MethodKind::Set => Kind::Set,
                _ => continue,
            };
            if !has_body(ast, m) {
                continue;
            }
            let Name::Known(name) = member_name(cx, key) else {
                continue;
            };
            let is_static = modifiers.contains(Modifiers::STATIC);
            let default_prop = if is_static {
                name == "name" || name == "length"
            } else {
                name == "constructor"
            };
            if default_prop {
                continue;
            }
            let seen = &reported[usize::from(is_static)];
            if seen.contains(&name)
                || descriptor_kind(cx, node, &name, is_static) != Some(this_kind)
            {
                continue;
            }
            let opposite = if this_kind == Kind::Get {
                Kind::Set
            } else {
                Kind::Get
            };
            let inherited = inherited_kind(cx, node, &name, is_static);
            if inherited != Some(opposite) && inherited != Some(Kind::Both) {
                continue;
            }
            let (kind_word, opposite_word) = if this_kind == Kind::Get {
                ("get", "set")
            } else {
                ("set", "get")
            };
            let msg = format!(
                "This {kind_word}ter hides the inherited {opposite_word}ter for `{name}`. Define both accessors in this class."
            );
            reported[usize::from(is_static)].push(name);
            let span = cx.span(key);
            // ESTree's key of a computed member is the expression inside the brackets.
            let span = match *ast.node(key) {
                Node::ComputedKey { expr } => cx.span(expr),
                _ => span,
            };
            cx.report(span, msg).id("no-incomplete-accessor-override");
        }
    }
}
