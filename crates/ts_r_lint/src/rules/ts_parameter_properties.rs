//! `@typescript-eslint/parameter-properties`: no parameter properties (`class-property`, the
//! default), or (`parameter-property`) class properties only assigned from a constructor
//! parameter of the same name become parameter properties.

use rustc_hash::FxHashMap;

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/parameter-properties",
        "Require or disallow parameter properties in class constructors",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<ParameterProperties>,
};

#[derive(Debug)]
struct ParameterProperties {
    allow: Vec<String>,
    prefer_parameter: bool,
}

impl FromOptions for ParameterProperties {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(ParameterProperties {
            allow: str_list(o, "allow"),
            prefer_parameter: str_field(o, "prefer") == Some("parameter-property"),
        })
    }
}

/// `private readonly`, `public`, `readonly` or `` as the option spells them.
fn modifier_text(m: Modifiers) -> String {
    let mut parts = Vec::new();
    for (flag, word) in [
        (Modifiers::PRIVATE, "private"),
        (Modifiers::PROTECTED, "protected"),
        (Modifiers::PUBLIC, "public"),
    ] {
        if m.contains(flag) {
            parts.push(word);
        }
    }
    if m.contains(Modifiers::READONLY) {
        parts.push("readonly");
    }
    parts.join(" ")
}

const PARAM_PROPERTY: Modifiers = Modifiers::PUBLIC
    .union(Modifiers::PRIVATE)
    .union(Modifiers::PROTECTED)
    .union(Modifiers::READONLY)
    .union(Modifiers::OVERRIDE);

/// A type annotation as ESTree's `TSTypeAnnotation` text: from the `:` to the type's end.
fn annotation_text<'a>(cx: &Ctx<'_, 'a>, ty: NodeId) -> &'a str {
    let src: &'a str = cx.src();
    let span = cx.span(ty);
    let before = skip_trivia_backward(cx.ast(), src, span.start as usize);
    let start = if src.as_bytes().get(before.wrapping_sub(1)) == Some(&b':') {
        before - 1
    } else {
        span.start as usize
    };
    src.get(start..span.end as usize).unwrap_or("")
}

#[derive(Default)]
struct Nodes {
    property: Option<NodeId>,
    assigned: bool,
    param: Option<NodeId>,
}

impl ParameterProperties {
    fn check_class(&self, cx: &mut Ctx<'_, '_>, members: ts_r_ast::NodeList) {
        let ast = cx.ast();
        let mut by_name: FxHashMap<&str, Nodes> = FxHashMap::default();
        let mut order: Vec<&str> = Vec::new();
        for &m in ast.list(members) {
            match *ast.node(m) {
                Node::PropertyDef {
                    modifiers,
                    key,
                    value: None,
                    ..
                } if !modifiers.intersects(Modifiers::ABSTRACT.union(Modifiers::ACCESSOR))
                    && matches!(ast.node(key), Node::Ident { .. })
                    && !self.allow.contains(&modifier_text(modifiers)) =>
                {
                    let name = ident_name(ast, key).unwrap_or_default();
                    if !by_name.contains_key(name) {
                        order.push(name);
                    }
                    by_name.entry(name).or_default().property = Some(m);
                }
                _ => {}
            }
        }
        for &m in ast.list(members) {
            let Node::MethodDef {
                kind: MethodKind::Constructor,
                value,
                ..
            } = *ast.node(m)
            else {
                continue;
            };
            let Node::FunctionExpr { params, body, .. } = *ast.node(value) else {
                continue;
            };
            for &p in ast.list(params) {
                if let Node::Param {
                    modifiers,
                    rest: false,
                    pattern,
                    init: None,
                    ..
                } = *ast.node(p)
                    && !modifiers.intersects(PARAM_PROPERTY)
                    && let Some(name) = ident_name(ast, pattern)
                {
                    if !by_name.contains_key(name) {
                        order.push(name);
                    }
                    by_name.entry(name).or_default().param = Some(p);
                }
            }
            let Some(Node::Block { stmts }) = body.map(|b| ast.node(b)) else {
                continue;
            };
            for &s in ast.list(*stmts) {
                let Node::ExprStmt { expr } = *ast.node(s) else {
                    break;
                };
                let Node::Assign { left, right, .. } = *ast.node(skip_parens(ast, expr)) else {
                    break;
                };
                let Node::Member { obj, prop, .. } = *ast.node(skip_parens(ast, left)) else {
                    break;
                };
                if !matches!(ast.node(skip_parens(ast, obj)), Node::This {})
                    || !matches!(ast.node(prop), Node::Ident { .. })
                {
                    break;
                }
                let right = skip_parens(ast, right);
                let Some(name) = ident_name(ast, right)
                    .filter(|_| matches!(ast.node(right), Node::Ident { .. }))
                else {
                    break;
                };
                if !by_name.contains_key(name) {
                    order.push(name);
                }
                by_name.entry(name).or_default().assigned = true;
            }
        }
        for name in order {
            let n = &by_name[name];
            let (Some(prop), Some(param), true) = (n.property, n.param, n.assigned) else {
                continue;
            };
            let prop_ty = match *ast.node(prop) {
                Node::PropertyDef { type_ann, .. } => type_ann,
                _ => None,
            };
            let param_ty = match *ast.node(param) {
                Node::Param { type_ann, .. } => type_ann,
                _ => None,
            };
            let same = match (prop_ty, param_ty) {
                (None, None) => true,
                (Some(a), Some(b)) => annotation_text(cx, a) == annotation_text(cx, b),
                _ => false,
            };
            if same {
                cx.report_node(
                    prop,
                    format!("Property {name} should be declared as a parameter property."),
                )
                .id("preferParameterProperty");
            }
        }
    }
}

impl Rule for ParameterProperties {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Param, NodeKind::ClassDecl, NodeKind::ClassExpr]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        match *cx.node(node) {
            Node::Param {
                modifiers, pattern, ..
            } if !self.prefer_parameter && modifiers.intersects(PARAM_PROPERTY) => {
                if self.allow.contains(&modifier_text(modifiers)) {
                    return;
                }
                let Some(name) = ident_name(cx.ast(), pattern) else {
                    return;
                };
                let msg = format!("Property {name} should be declared as a class property.");
                cx.report_node(node, msg).id("preferClassProperty");
            }
            Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. }
                if self.prefer_parameter =>
            {
                self.check_class(cx, members)
            }
            _ => {}
        }
    }
}
