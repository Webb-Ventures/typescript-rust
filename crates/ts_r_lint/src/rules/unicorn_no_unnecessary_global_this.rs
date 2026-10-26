//! `unicorn/no-unnecessary-global-this`: `globalThis.X` (the global `globalThis`, through
//! TS wrappers) where `X` is an available global that is not shadowed can be written `X`.
//! Writes, optional chains and existence checks (`if (globalThis.X)`, `globalThis.X ?? y`,
//! `globalThis.X === undefined`) are left alone: a bare missing global throws. The fix is
//! withheld for call and tag callees (the `this` would change) and when comments would go.

use super::prelude::*;
use super::unicorn_explicit_length_check::is_boolean_context;
use super::unicorn_text::{has_comment_in, is_written};
use super::unicorn_util::{has_optional_chain, skip_wrappers};
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-unnecessary-global-this",
        "Disallow unnecessary `globalThis` references.",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<NoUnnecessaryGlobalThis>,
};

#[derive(Debug, Default)]
struct NoUnnecessaryGlobalThis;

/// Reserved words a bare identifier can't be (`eval` and `arguments` are fine as names).
const RESERVED: &[&str] = &[
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "function",
    "if",
    "import",
    "in",
    "instanceof",
    "new",
    "null",
    "return",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "var",
    "void",
    "while",
    "with",
    "yield",
    "implements",
    "interface",
    "let",
    "package",
    "private",
    "protected",
    "public",
    "static",
];

fn is_identifier_name(s: &str) -> bool {
    let mut it = s.chars();
    it.next()
        .is_some_and(|c| c == '$' || c == '_' || ts_r_lexer::is_id_start(c))
        && it.all(|c| c == '$' || c == '_' || ts_r_lexer::is_id_part(c))
}

/// The outermost of the parentheses and TS wrappers around `id`.
fn outermost_wrapper(cx: &Ctx<'_, '_>, mut id: NodeId) -> NodeId {
    while let Some(p) = cx.parent(id) {
        match *cx.ast().node(p) {
            Node::Paren { .. }
            | Node::As { .. }
            | Node::Satisfies { .. }
            | Node::NonNull { .. }
            | Node::TypeAssertion { .. }
            | Node::Instantiation { .. } => id = p,
            _ => break,
        }
    }
    id
}

/// A computed key's string value, evaluated without looking up any name (literals,
/// substitution-free templates and `+` of those).
fn constant_key(ast: &Ast, id: NodeId, depth: u32) -> Option<String> {
    if depth > 100 {
        return None;
    }
    match *ast.node(id) {
        Node::Paren { expr } => constant_key(ast, expr, depth + 1),
        Node::StringLit { value } => Some(ast.atom(value).to_owned()),
        Node::NumberLit { value } => Some(number_to_string(value.get())),
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
        Node::Binary {
            left,
            op: BinaryOp::Add,
            right,
        } => {
            let l = constant_key(ast, left, depth + 1)?;
            let r = constant_key(ast, right, depth + 1)?;
            Some(l + &r)
        }
        _ => None,
    }
}

/// Compared with `null` or `undefined` by `==`, `!=`, `===` or `!==`.
fn is_nullish_comparison(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let Some(p) = cx.parent(node) else {
        return false;
    };
    let Node::Binary { left, op, right } = *ast.node(p) else {
        return false;
    };
    if !matches!(
        op,
        BinaryOp::Eq | BinaryOp::NotEq | BinaryOp::StrictEq | BinaryOp::StrictNotEq
    ) {
        return false;
    }
    let other = skip_wrappers(ast, if left == node { right } else { left });
    matches!(ast.node(other), Node::NullLit {}) || ident_name(ast, other) == Some("undefined")
}

impl Rule for NoUnnecessaryGlobalThis {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Member, NodeKind::Index]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let (obj, name) = match *ast.node(node) {
            Node::Member { obj, prop, .. } => {
                let Some(name) =
                    ident_name(ast, prop).filter(|_| matches!(ast.node(prop), Node::Ident { .. }))
                else {
                    return;
                };
                (obj, name.to_owned())
            }
            Node::Index { obj, index, .. } => {
                let Some(name) = constant_key(ast, index, 0) else {
                    return;
                };
                (obj, name)
            }
            _ => return,
        };
        let object = skip_wrappers(ast, obj);
        if !cx.is_global_ref(object, "globalThis") || is_written(cx, node) {
            return;
        }
        let outer = outermost_wrapper(cx, node);
        let parent = cx.parent(outer).map(|p| *ast.node(p));
        let optional_use = has_optional_chain(ast, node)
            || matches!(parent, Some(Node::Member { obj, optional: true, .. } | Node::Index { obj, optional: true, .. }) if obj == outer)
            || matches!(parent, Some(Node::Call { callee, optional: true, .. }) if callee == outer);
        let existence_check = matches!(
            parent,
            Some(Node::Binary {
                op: BinaryOp::LogicalAnd | BinaryOp::LogicalOr | BinaryOp::Coalesce,
                ..
            })
        ) || is_nullish_comparison(cx, outer)
            || is_boolean_context(cx, outer);
        if optional_use || existence_check {
            return;
        }
        let callee = matches!(parent, Some(Node::Call { callee, .. }) if callee == outer);
        let tag = matches!(parent, Some(Node::TaggedTemplate { tag, .. }) if tag == outer);
        if !is_identifier_name(&name)
            || RESERVED.contains(&name.as_str())
            || (name == "eval" && callee)
            || !cx.global_available(node, &name)
        {
            return;
        }
        let span = cx.span(node);
        let fix = (!callee && !tag && !has_comment_in(cx, span)).then(|| Fix::replace(span, &name));
        cx.report_node(
            node,
            format!("Use `{name}` directly instead of `globalThis.{name}`."),
        )
        .id("no-unnecessary-global-this")
        .fix_opt(fix);
    }
}
