//! `@typescript-eslint/no-misused-new`: enforce valid definition of `new` and
//! `constructor`.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-misused-new",
        "Enforce valid definition of `new` and `constructor`",
        TS_REC,
        Category::Correctness,
    ),
    create: create_default::<NoMisusedNew>,
};

#[derive(Debug, Default)]
struct NoMisusedNew;

/// The return type names `name` (`I` or `I<T>`).
fn returns(cx: &Ctx<'_, '_>, ret: Option<NodeId>, name: &str) -> bool {
    ret.is_some_and(|r| matches!(*cx.node(r), Node::TypeRef { name: n, .. } if cx.text(n) == name))
}

/// A member span including its trailing `;` (as ESTree does).
fn member_span(cx: &Ctx<'_, '_>, m: NodeId) -> Span {
    let s = cx.span(m);
    if cx.src().as_bytes().get(s.end as usize) == Some(&b';') {
        Span::new(s.start, s.end + 1)
    } else {
        s
    }
}

const IFACE: &str = "Interfaces cannot be constructed, only classes.";

impl Rule for NoMisusedNew {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::Interface,
            NodeKind::TypeLiteral,
            NodeKind::ClassDecl,
            NodeKind::ClassExpr,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        match *cx.node(node) {
            Node::Interface { name, body, .. } => {
                let iname = cx.text(name);
                for &m in ast.list(body) {
                    let bad = match *cx.node(m) {
                        Node::ConstructSignature { return_type, .. } => {
                            returns(cx, return_type, iname)
                        }
                        Node::MethodSignature { key, .. } => cx.text(key) == "constructor",
                        _ => false,
                    };
                    if bad {
                        let span = member_span(cx, m);
                        cx.report(span, IFACE).id("errorMessageInterface");
                    }
                }
            }
            Node::TypeLiteral { members } => {
                for &m in ast.list(members) {
                    if let Node::MethodSignature { key, .. } = *cx.node(m)
                        && cx.text(key) == "constructor"
                    {
                        let span = member_span(cx, m);
                        cx.report(span, IFACE).id("errorMessageInterface");
                    }
                }
            }
            Node::ClassDecl {
                name: Some(name),
                members,
                ..
            }
            | Node::ClassExpr {
                name: Some(name),
                members,
                ..
            } => {
                let cname = cx.text(name);
                for &m in ast.list(members) {
                    let Node::MethodDef {
                        kind: MethodKind::Method,
                        key,
                        value,
                        ..
                    } = *cx.node(m)
                    else {
                        continue;
                    };
                    if cx.text(key) != "new" {
                        continue;
                    }
                    let Node::FunctionExpr {
                        body: None,
                        return_type,
                        ..
                    } = *cx.node(value)
                    else {
                        continue;
                    };
                    if returns(cx, return_type, cname) {
                        let span = member_span(cx, m);
                        cx.report(span, "Class cannot have method named `new`.")
                            .id("errorMessageClass");
                    }
                }
            }
            _ => {}
        }
    }
}
