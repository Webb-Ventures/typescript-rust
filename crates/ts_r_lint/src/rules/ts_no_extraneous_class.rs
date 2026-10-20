//! `@typescript-eslint/no-extraneous-class`: disallow classes used as namespaces.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-extraneous-class",
        "Disallow classes used as namespaces",
        TS_STRICT,
        Category::Style,
    ),
    create: create::<NoExtraneousClass>,
};

#[derive(Debug)]
struct NoExtraneousClass {
    allow_constructor_only: bool,
    allow_empty: bool,
    allow_static_only: bool,
    allow_with_decorator: bool,
}

impl FromOptions for NoExtraneousClass {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(NoExtraneousClass {
            allow_constructor_only: bool_field(o, "allowConstructorOnly", false),
            allow_empty: bool_field(o, "allowEmpty", false),
            allow_static_only: bool_field(o, "allowStaticOnly", false),
            allow_with_decorator: bool_field(o, "allowWithDecorator", false),
        })
    }
}

/// The `class` keyword's span start through the end of the class (anonymous classes).
fn class_keyword_span(cx: &Ctx<'_, '_>, node: NodeId) -> Span {
    let span = cx.span(node);
    let text = cx.text(node);
    let mut from = 0;
    while let Some(off) = text[from..].find("class") {
        let at = from + off;
        let before = text[..at].chars().next_back();
        let after = text[at + 5..].chars().next();
        if !before.is_some_and(is_ident_char) && !after.is_some_and(is_ident_char) {
            return Span::new(span.start + at as u32, span.end);
        }
        from = at + 5;
    }
    span
}

impl Rule for NoExtraneousClass {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::ClassDecl, NodeKind::ClassExpr]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let (decorators, name, heritage, members) = match *cx.node(node) {
            Node::ClassDecl {
                decorators,
                name,
                heritage,
                members,
                ..
            }
            | Node::ClassExpr {
                decorators,
                name,
                heritage,
                members,
                ..
            } => (decorators, name, heritage, members),
            _ => return,
        };
        if self.allow_with_decorator && !ast.list(decorators).is_empty() {
            return;
        }
        let extends = ast.list(heritage).iter().any(|&h| {
            matches!(
                ast.node(h),
                Node::HeritageClause {
                    kind: ts_r_ast::HeritageKind::Extends,
                    ..
                }
            )
        });
        if extends {
            return;
        }
        let members = ast.list(members);
        let span = name.map_or_else(|| class_keyword_span(cx, node), |n| cx.span(n));
        if members.is_empty() {
            if !self.allow_empty {
                cx.report(span, "Unexpected empty class.").id("empty");
            }
            return;
        }
        let mut only_ctor = true;
        let mut only_static = true;
        for &m in members {
            match *cx.node(m) {
                Node::MethodDef {
                    kind: MethodKind::Constructor,
                    value,
                    ..
                } => {
                    let props = matches!(*cx.node(value), Node::FunctionExpr { params, .. }
                        if ast.list(params).iter().any(|&p| matches!(*ast.node(p), Node::Param { modifiers, .. } if !modifiers.is_empty())));
                    if props {
                        return;
                    }
                }
                Node::MethodDef { modifiers, .. } | Node::PropertyDef { modifiers, .. } => {
                    only_ctor = false;
                    if !modifiers.contains(Modifiers::STATIC)
                        || modifiers.contains(Modifiers::ABSTRACT)
                    {
                        only_static = false;
                    }
                }
                _ => {
                    only_ctor = false;
                    only_static = false;
                }
            }
        }
        if only_ctor {
            if !self.allow_constructor_only {
                cx.report(span, "Unexpected class with only a constructor.")
                    .id("onlyConstructor");
            }
        } else if only_static && !self.allow_static_only {
            cx.report(span, "Unexpected class with only static properties.")
                .id("onlyStatic");
        }
    }
}
