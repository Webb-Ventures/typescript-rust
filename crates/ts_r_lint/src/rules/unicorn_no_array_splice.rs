//! `unicorn/no-array-splice`: a statement `array.splice(...)` on a local `let`/`var` that
//! starts out as a fresh array (`[]`, `Array(...)`, `Array.from/of`, or a method that
//! always returns a new array) and never escapes can be `array = array.toSpliced(...)`.
//! Calls `no-unnecessary-splice` handles, receivers known not to be arrays, global or
//! exported bindings and non-array annotations (tuples, `readonly`, aliases) are skipped.
//! The rewrite is a suggestion.

use super::prelude::*;
use super::unicorn_ast::{is_value_not_usable, method_call};
use super::unicorn_splice::unnecessary_splice_replacement;
use super::unicorn_types::should_skip_known_non_array_receiver;
use super::unicorn_util::skip_wrappers;
use crate::semantic::{DeclKind, ScopeKind, Symbol};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-array-splice",
        "Prefer `Array#toSpliced()` over `Array#splice()`.",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<NoArraySplice>,
};

#[derive(Debug, Default)]
struct NoArraySplice;

/// Methods that always return a new array (`slice`/`concat` may be `String`'s).
const NEW_ARRAY_METHODS: &[&str] = &[
    "filter",
    "flat",
    "flatMap",
    "map",
    "toReversed",
    "toSorted",
    "toSpliced",
    "with",
];

/// The declarator of a single `let`/`var` plain-name declaration outside the global scope
/// and not exported, with a fresh-array initializer.
fn fresh_local_array_declarator(cx: &Ctx<'_, '_>, sym: &Symbol) -> Option<NodeId> {
    let ast = cx.ast();
    let sem = cx.semantic();
    let [decl] = sym.decls.as_slice() else {
        return None;
    };
    if !matches!(decl.kind, DeclKind::Let | DeclKind::Var)
        || sem.scope(sym.scope).kind == ScopeKind::Global
    {
        return None;
    }
    let Node::VarDeclarator {
        name,
        init: Some(init),
        ..
    } = *ast.node(decl.node)
    else {
        return None;
    };
    if name != decl.name || !matches!(ast.node(name), Node::Ident { .. }) {
        return None;
    }
    let exported = cx.parent(decl.node).is_some_and(|d| {
        matches!(*ast.node(d), Node::VarDecl { modifiers, .. } if modifiers.contains(Modifiers::EXPORT))
    });
    if exported {
        return None;
    }
    let init = skip_wrappers(ast, init);
    let fresh = match *ast.node(init) {
        Node::ArrayLit { .. } => true,
        Node::Call {
            callee,
            optional: false,
            ..
        }
        | Node::New { callee, .. }
            if ident_name(ast, callee) == Some("Array") =>
        {
            true
        }
        _ => method_call(ast, init).is_some_and(|m| {
            m.plain()
                && (NEW_ARRAY_METHODS.contains(&m.name)
                    || (matches!(m.name, "from" | "of")
                        && ident_name(ast, m.obj) == Some("Array")
                        && matches!(ast.node(m.obj), Node::Ident { .. })))
        }),
    };
    fresh.then_some(decl.node)
}

/// A reference other than the initialization and `receiver` writes the variable or lets
/// the array escape (anything but a member access, a spread or a `for-of` source).
fn escapes(cx: &Ctx<'_, '_>, sym: &Symbol, receiver: NodeId) -> bool {
    let ast = cx.ast();
    let sem = cx.semantic();
    sym.refs.iter().any(|&r| {
        let r = sem.reference(r);
        if r.flags.contains(RefFlags::INIT) || r.node == receiver {
            return false;
        }
        if r.flags.contains(RefFlags::WRITE) {
            return true;
        }
        let mut cur = r.node;
        let parent = loop {
            match cx.parent(cur) {
                Some(p) if matches!(ast.node(p), Node::Paren { .. }) => cur = p,
                Some(p) => break p,
                None => return true,
            }
        };
        !match *ast.node(parent) {
            Node::Member { obj, .. } | Node::Index { obj, .. } => obj == cur,
            Node::Spread { arg } => arg == cur,
            Node::ForOf { right, .. } => right == cur,
            _ => false,
        }
    })
}

/// `T[]` or `Array<...>` (through parentheses).
fn is_plain_array_annotation(ast: &ts_r_ast::Ast, mut ty: NodeId) -> bool {
    for _ in 0..1000 {
        match *ast.node(ty) {
            Node::ParenType { ty: inner } => ty = inner,
            Node::ArrayType { .. } => return true,
            Node::TypeRef { name, .. } => {
                return matches!(ast.node(name), Node::Ident { .. })
                    && ident_name(ast, name) == Some("Array");
            }
            _ => return false,
        }
    }
    false
}

impl Rule for NoArraySplice {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some(m) = method_call(ast, node) else {
            return;
        };
        if m.name != "splice" || !m.plain() || !is_value_not_usable(cx, node) {
            return;
        }
        let receiver = skip_wrappers(ast, m.obj);
        if !matches!(ast.node(receiver), Node::Ident { .. })
            || unnecessary_splice_replacement(cx, m.obj, m.args).is_some()
            || should_skip_known_non_array_receiver(cx, m.obj)
        {
            return;
        }
        let sem = cx.semantic();
        let Some(s) = sem.symbol_of_ident(receiver) else {
            return;
        };
        let sym = sem.symbol(s);
        let Some(declarator) = fresh_local_array_declarator(cx, sym) else {
            return;
        };
        if let Node::VarDeclarator {
            type_ann: Some(t), ..
        } = *ast.node(declarator)
            && !is_plain_array_annotation(ast, t)
        {
            return;
        }
        if escapes(cx, sym, receiver) {
            return;
        }
        let receiver_text = cx.text(receiver).to_owned();
        let fix = Fix::insert(cx.span(node).start, format!("{receiver_text} = "))
            .and(cx.span(m.prop), "toSpliced");
        cx.report_node(
            m.prop,
            "Use `Array#toSpliced()` instead of `Array#splice()`.",
        )
        .id("error")
        .suggest(
            "suggestion",
            "Assign the `.toSpliced()` result back to the array.",
            fix,
        );
    }
}
