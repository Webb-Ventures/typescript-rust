//! `@typescript-eslint/adjacent-overload-signatures`: overloads of one function or member
//! are declared next to each other.
//!
//! In every statement list (program, block, module block), class body, interface body and
//! type literal, each function declaration, method (definition or signature), call signature
//! (`call`) and construct signature (`new`) has a name; any other member breaks adjacency.
//! A member reports when an earlier one has the same name (and static-ness, call-signature
//! flag and name kind) but the member right before it doesn't.

use std::borrow::Cow;

use super::prelude::*;
use ts_r_ast::NodeList;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/adjacent-overload-signatures",
        "Require that function overload signatures be consecutive",
        TS_STYLISTIC,
        Category::Style,
    ),
    create: create_default::<AdjacentOverloadSignatures>,
};

#[derive(Debug, Default)]
struct AdjacentOverloadSignatures;

/// How a member's name is written (typescript-eslint's `MemberNameType`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum NameKind {
    Private,
    Quoted,
    Normal,
    Expression,
}

#[derive(PartialEq, Eq, Debug)]
struct Method<'a> {
    name: Cow<'a, str>,
    kind: NameKind,
    is_static: bool,
    call: bool,
}

/// Whether `name` is not a valid identifier and so is quoted.
pub(crate) fn requires_quoting(name: &str) -> bool {
    let mut it = name.chars();
    match it.next() {
        Some(c) if ts_r_lexer::is_id_start(c) => !it.all(ts_r_lexer::is_id_part),
        _ => true,
    }
}

/// A member key's name and kind (`getNameFromMember`).
pub(crate) fn member_key_name<'a>(cx: &Ctx<'_, 'a>, key: NodeId) -> (Cow<'a, str>, NameKind) {
    let ast = cx.ast();
    let src: &'a str = cx.src();
    let (key, computed) = match *ast.node(key) {
        Node::ComputedKey { expr } => (skip_parens(ast, expr), true),
        _ => (key, false),
    };
    match *ast.node(key) {
        Node::Ident { name } if !computed => (Cow::Borrowed(ast.atom(name)), NameKind::Normal),
        Node::PrivateName { name } => (
            Cow::Owned(format!("#{}", ast.atom(name))),
            NameKind::Private,
        ),
        Node::StringLit { .. } | Node::NumberLit { .. } | Node::BigIntLit { .. } => {
            let v = key_name(ast, src, key).unwrap_or_default();
            if requires_quoting(&v) {
                (Cow::Owned(format!("\"{v}\"")), NameKind::Quoted)
            } else {
                (v, NameKind::Normal)
            }
        }
        _ => (Cow::Borrowed(ast.span(key).text(src)), NameKind::Expression),
    }
}

impl AdjacentOverloadSignatures {
    fn method<'a>(cx: &Ctx<'_, 'a>, member: NodeId) -> Option<Method<'a>> {
        let ast = cx.ast();
        match *ast.node(member) {
            Node::FunctionDecl { name, .. } => {
                let name = ident_name(ast, name?)?;
                Some(Method {
                    name: Cow::Borrowed(name),
                    kind: NameKind::Normal,
                    is_static: false,
                    call: false,
                })
            }
            Node::MethodDef { modifiers, key, .. } if !modifiers.contains(Modifiers::ABSTRACT) => {
                let (name, kind) = member_key_name(cx, key);
                Some(Method {
                    name,
                    kind,
                    is_static: modifiers.contains(Modifiers::STATIC),
                    call: false,
                })
            }
            Node::MethodSignature { modifiers, key, .. } => {
                let (name, kind) = member_key_name(cx, key);
                Some(Method {
                    name,
                    kind,
                    is_static: modifiers.contains(Modifiers::STATIC),
                    call: false,
                })
            }
            Node::CallSignature { .. } => Some(Method {
                name: Cow::Borrowed("call"),
                kind: NameKind::Normal,
                is_static: false,
                call: true,
            }),
            Node::ConstructSignature { .. } => Some(Method {
                name: Cow::Borrowed("new"),
                kind: NameKind::Normal,
                is_static: false,
                call: false,
            }),
            _ => None,
        }
    }

    fn check(cx: &mut Ctx<'_, '_>, members: NodeList) {
        let ast = cx.ast();
        let mut seen: Vec<Method<'_>> = Vec::new();
        let mut last: Option<Method<'_>> = None;
        let mut reports = Vec::new();
        for &m in ast.list(members) {
            let Some(method) = Self::method(cx, m) else {
                last = None;
                continue;
            };
            let known = seen.contains(&method);
            if known && last.as_ref() != Some(&method) {
                let name = format!(
                    "{}{}",
                    if method.is_static { "static " } else { "" },
                    method.name
                );
                reports.push((cx.span(m), name));
            } else if !known {
                seen.push(Method {
                    name: method.name.clone(),
                    ..method
                });
            }
            last = Some(method);
        }
        for (span, name) in reports {
            cx.report(span, format!("All {name} signatures should be adjacent."))
                .id("adjacentSignature");
        }
    }
}

impl Rule for AdjacentOverloadSignatures {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::Program,
            NodeKind::Block,
            NodeKind::ModuleBlock,
            NodeKind::ClassDecl,
            NodeKind::ClassExpr,
            NodeKind::Interface,
            NodeKind::TypeLiteral,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let list = match *cx.node(node) {
            Node::Program { stmts } | Node::Block { stmts } | Node::ModuleBlock { stmts } => stmts,
            Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. } => members,
            Node::Interface { body, .. } => body,
            Node::TypeLiteral { members } => members,
            _ => return,
        };
        Self::check(cx, list);
    }
}
