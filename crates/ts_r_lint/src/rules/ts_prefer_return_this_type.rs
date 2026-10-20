//! `@typescript-eslint/prefer-return-this-type`: methods that return `this` declare the
//! `this` type rather than the class name.

use super::prelude::*;
use ts_r_ast::visit::{Visitor, Walk, walk};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/prefer-return-this-type",
        "Enforce that `this` is used when only `this` type is returned",
        TS_STRICT_TC,
        Category::Style,
    )
    .typed()
    .fixable(),
    create: create_default::<PreferReturnThisType>,
};

#[derive(Debug, Default)]
struct PreferReturnThisType;

impl Rule for PreferReturnThisType {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::ClassDecl, NodeKind::ClassExpr]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let (Node::ClassDecl {
            name: Some(name),
            members,
            ..
        }
        | Node::ClassExpr {
            name: Some(name),
            members,
            ..
        }) = *cx.node(node)
        else {
            return;
        };
        let ast = cx.ast();
        let Some(class_name) = ident_name(ast, name).map(str::to_string) else {
            return;
        };
        let ti = cx.types();
        let Some(instance) = ti
            .symbol_at(name)
            .and_then(|s| ti.declared_type_of_symbol(s))
        else {
            return;
        };
        if instance == Ty::UNRESOLVED {
            return;
        }
        for &m in ast.list(members) {
            let f = match *cx.node(m) {
                Node::MethodDef { value, .. } => value,
                Node::PropertyDef { value: Some(v), .. } => skip_parens(ast, v),
                _ => continue,
            };
            let (params, return_type, body) = match *cx.node(f) {
                Node::FunctionExpr {
                    params,
                    return_type: Some(rt),
                    body: Some(body),
                    ..
                }
                | Node::Arrow {
                    params,
                    return_type: Some(rt),
                    body,
                    ..
                } => (params, rt, body),
                _ => continue,
            };
            let has_this_param = ast.list(params).iter().any(|&p| {
                matches!(*cx.node(p), Node::Param { pattern, .. } if matches!(cx.node(pattern), Node::This {}) || ident_name(ast, pattern) == Some("this"))
            });
            if has_this_param {
                continue;
            }
            let Some(target) = class_ref(cx, return_type, &class_name) else {
                continue;
            };
            if !returns_only_this(cx, f, body, instance) {
                continue;
            }
            let fix = Fix::replace(cx.span(target), "this");
            cx.report_node(target, "Use `this` type instead.")
                .id("useThisType")
                .fix(fix);
        }
    }
}

/// The reference to the class in a return type: the type itself or a union member.
fn class_ref(cx: &Ctx<'_, '_>, ty: NodeId, class_name: &str) -> Option<NodeId> {
    let ast = cx.ast();
    let is_ref = |t: NodeId| matches!(*cx.node(t), Node::TypeRef { name, .. } if ident_name(ast, name) == Some(class_name));
    if is_ref(ty) {
        return Some(ty);
    }
    if let Node::UnionType { types } = *cx.node(ty) {
        return ast.list(types).iter().copied().find(|&t| is_ref(t));
    }
    None
}

fn is_this_type(ti: &dyn TypeInfo, t: Ty) -> bool {
    t != Ty::UNRESOLVED
        && ti.flags(t).contains(TypeFlags::TYPE_PARAMETER)
        && ti.type_to_string(t) == "this"
}

fn returns_only_this(cx: &Ctx<'_, '_>, f: NodeId, body: NodeId, instance: Ty) -> bool {
    let ti = cx.types();
    let mut values = Vec::new();
    if matches!(*cx.node(f), Node::Arrow { flags, .. } if flags.contains(FnFlags::EXPRESSION_BODY))
    {
        values.push(body);
    } else {
        let mut finder = Returns {
            root: body,
            values: Vec::new(),
        };
        walk(cx.ast(), body, &mut finder);
        values = finder.values;
    }
    let mut any_this = false;
    for v in values {
        // `return this` (even where the checker types a function expression's `this` loosely).
        if matches!(cx.node(skip_parens(cx.ast(), v)), Node::This {}) {
            any_this = true;
            continue;
        }
        let Some(t) = ti.type_at(v) else { return false };
        if has_unresolved_part(ti, t) {
            return false;
        }
        let ps = parts(ti, t);
        if ps.iter().all(|&p| is_this_type(ti, p)) {
            any_this = true;
            continue;
        }
        if ps
            .iter()
            .any(|&p| !is_this_type(ti, p) && (is_any(ti, p) || ti.is_assignable(p, instance)))
        {
            return false;
        }
    }
    any_this
}

struct Returns {
    root: NodeId,
    values: Vec<NodeId>,
}

impl Visitor for Returns {
    fn enter(&mut self, ast: &ts_r_ast::Ast, id: NodeId) -> Walk {
        match *ast.node(id) {
            Node::FunctionDecl { .. }
            | Node::FunctionExpr { .. }
            | Node::Arrow { .. }
            | Node::ClassDecl { .. }
            | Node::ClassExpr { .. }
                if id != self.root =>
            {
                Walk::SkipChildren
            }
            Node::Return { arg: Some(a) } => {
                self.values.push(a);
                Walk::SkipChildren
            }
            _ => Walk::Continue,
        }
    }
}
