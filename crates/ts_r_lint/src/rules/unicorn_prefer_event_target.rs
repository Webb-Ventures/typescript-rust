//! `unicorn/prefer-event-target`: `EventEmitter` used as a superclass or as a `new` callee
//! is reported, unless the name is declared by an import from (or a `const` destructuring /
//! `.EventEmitter` read of `require(...)` / `await import(...)` of) a package whose
//! `EventEmitter` is not Node's: `@angular/core`, `eventemitter3`.

use super::prelude::*;
use super::unicorn_ast::{declared_name, superclass};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-event-target",
        "Prefer `EventTarget` over `EventEmitter`.",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<PreferEventTarget>,
};

#[derive(Debug, Default)]
struct PreferEventTarget;

const IGNORED: &[&str] = &["@angular/core", "eventemitter3"];

impl Rule for PreferEventTarget {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::ClassDecl, NodeKind::ClassExpr, NodeKind::New]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let target = match *ast.node(node) {
            Node::New { callee, .. } => callee,
            _ => match superclass(ast, node) {
                Some(s) => s,
                None => return,
            },
        };
        let target = skip_parens(ast, target);
        if ident_name(ast, target) != Some("EventEmitter") {
            return;
        }
        if declared_name(cx, target).is_some_and(|d| from_ignored_package(cx, d)) {
            return;
        }
        cx.report_node(target, "Prefer `EventTarget` over `EventEmitter`.")
            .id("prefer-event-target");
    }
}

fn is_ignored_source(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    static_value_string(cx.ast(), cx.src(), id).is_some_and(|s| IGNORED.contains(&s.as_ref()))
}

/// `require("pkg")` or `await import("pkg")` of an ignored package.
fn is_ignored_load(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    match *ast.node(id) {
        Node::Call {
            callee,
            optional: false,
            args,
            ..
        } => {
            let args = ast.list(args);
            ident_name(ast, skip_parens(ast, callee)) == Some("require")
                && args.len() == 1
                && is_ignored_source(cx, args[0])
        }
        Node::Await { arg } => match *ast.node(arg) {
            Node::ImportCall { arg, .. } => is_ignored_source(cx, arg),
            _ => false,
        },
        _ => false,
    }
}

/// `decl` is the name of a `const` declarator; its initialiser.
fn const_declarator_init(cx: &Ctx<'_, '_>, decl: NodeId) -> Option<Option<NodeId>> {
    let ast = cx.ast();
    let d = cx.parent(decl)?;
    let Node::VarDeclarator { name, init, .. } = *ast.node(d) else {
        return None;
    };
    let list = cx.parent(d)?;
    (name == decl
        && matches!(
            ast.node(list),
            Node::VarDecl {
                kind: VarKind::Const,
                ..
            }
        ))
    .then_some(init)
}

fn from_ignored_package(cx: &Ctx<'_, '_>, decl: NodeId) -> bool {
    let ast = cx.ast();
    for a in ancestors_inclusive(cx.file.parents, decl) {
        if let Node::ImportDecl { source, .. } = *ast.node(a) {
            return is_ignored_source(cx, source);
        }
    }
    // `const {EventEmitter} = require("pkg")` (no default value).
    if let Some(p) = cx.parent(decl) {
        let destructured = match *ast.node(p) {
            Node::BindingProperty {
                key,
                value,
                init: None,
            } => value.is_none_or(|v| v == decl) && ident_name(ast, key) == Some("EventEmitter"),
            _ => false,
        };
        if destructured
            && let Some(pattern) = cx.parent(p)
            && matches!(ast.node(pattern), Node::ObjectPattern { .. })
            && let Some(Some(init)) = const_declarator_init(cx, pattern)
        {
            return is_ignored_load(cx, skip_parens(ast, init));
        }
    }
    // `const EventEmitter = require("pkg").EventEmitter`
    if let Some(Some(init)) = const_declarator_init(cx, decl)
        && let Node::Member {
            obj,
            optional: false,
            prop,
        } = *ast.node(skip_parens(ast, init))
        && ident_name(ast, prop) == Some("EventEmitter")
    {
        return is_ignored_load(cx, skip_parens(ast, obj));
    }
    false
}
