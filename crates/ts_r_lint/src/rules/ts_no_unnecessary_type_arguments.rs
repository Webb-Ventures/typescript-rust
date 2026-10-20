//! `@typescript-eslint/no-unnecessary-type-arguments`: a last type argument equal to its
//! parameter's default can be left out.

use super::prelude::*;
use ts_r_ast::{HeritageKind, NodeList};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-unnecessary-type-arguments",
        "Disallow type arguments that are equal to the default",
        TS_STRICT_TC,
        Category::Style,
    )
    .typed()
    .fixable(),
    create: create_default::<NoUnnecessaryTypeArguments>,
};

#[derive(Debug, Default)]
struct NoUnnecessaryTypeArguments;

const MESSAGE: &str = "This is the default value for this type parameter, so it can be omitted.";

impl Rule for NoUnnecessaryTypeArguments {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::TypeArgs]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Node::TypeArgs { args } = *cx.node(node) else {
            return;
        };
        let ast = cx.ast();
        let args = ast.list(args).to_vec();
        let Some(&last) = args.last() else { return };
        let Some(parent) = cx.parent(node) else {
            return;
        };
        let decl = match *cx.node(parent) {
            Node::Call { type_args, .. } | Node::New { type_args, .. }
                if type_args == Some(node) =>
            {
                call_decl(cx, parent)
            }
            Node::TaggedTemplate { type_args, .. } if type_args == Some(node) => {
                call_decl(cx, parent)
            }
            Node::JsxOpening { name, .. } => type_side_decl(cx, name, false),
            Node::TypeRef { name, .. } => type_side_decl(cx, name, true),
            Node::ExprWithTypeArgs { expr, .. } => {
                let class_extends = cx.parent(parent).is_some_and(|h| {
                    matches!(
                        cx.node(h),
                        Node::HeritageClause {
                            kind: HeritageKind::Extends,
                            ..
                        }
                    ) && cx.parent(h).is_some_and(|c| {
                        matches!(cx.node(c), Node::ClassDecl { .. } | Node::ClassExpr { .. })
                    })
                });
                if class_extends {
                    construct_decl(cx, expr)
                } else {
                    type_side_decl(cx, expr, true)
                }
            }
            _ => None,
        };
        let Some(params) = decl.and_then(|d| decl_type_params(cx, d)) else {
            return;
        };
        let params = ast.list(params);
        let i = args.len() - 1;
        let Some(&param) = params.get(i) else { return };
        let Node::TypeParam {
            default: Some(default),
            ..
        } = *cx.node(param)
        else {
            return;
        };
        if !same_type(cx, last, default) {
            return;
        }
        let fix = if i == 0 {
            Fix::delete(cx.span(node))
        } else {
            Fix::delete(Span::new(cx.span(args[i - 1]).end, cx.span(last).end))
        };
        cx.report_node(last, MESSAGE)
            .id("unnecessaryTypeParameter")
            .fix(fix);
    }
}

/// The argument and the default are the same type (unresolved names: the same text).
fn same_type(cx: &Ctx<'_, '_>, arg: NodeId, default: NodeId) -> bool {
    let ti = cx.types();
    let (Some(a), Some(d)) = (ti.type_of_type_node(arg), ti.type_of_type_node(default)) else {
        return false;
    };
    if a == Ty::UNRESOLVED || d == Ty::UNRESOLVED {
        return false;
    }
    if any_kind(ti, a) == Some(AnyKind::Error) || any_kind(ti, d) == Some(AnyKind::Error) {
        let src = cx.src();
        return cx.span(arg).text(src).trim() == cx.span(default).text(src).trim();
    }
    same_reference(ti, a, d, 0)
}

/// `a` and `b` are one type, or references to the same target with the same arguments
/// (the checker keeps the alias a reference was written through, `A` for
/// `type A = Map<string, string>`, as a separate type).
fn same_reference(ti: &dyn TypeInfo, a: Ty, b: Ty, depth: u32) -> bool {
    if a == b {
        return true;
    }
    if depth > 16 {
        return false;
    }
    match (ti.reference_target(a), ti.reference_target(b)) {
        (Some(x), Some(y)) if x == y => {
            let (xs, ys) = (ti.type_args(a), ti.type_args(b));
            xs.len() == ys.len()
                && xs
                    .iter()
                    .zip(&ys)
                    .all(|(&p, &q)| same_reference(ti, p, q, depth + 1))
        }
        _ => false,
    }
}

/// The declaration a call, `new` or tagged template resolves to.
fn call_decl(cx: &Ctx<'_, '_>, call: NodeId) -> Option<NodeId> {
    let ti = cx.types();
    if let Some(sig) = ti.resolved_signature(call)
        && let Some(d) = ti.signature_decl(sig)
        && d.in_this_file
    {
        return Some(d.node);
    }
    let callee = match *cx.node(call) {
        Node::Call { callee, .. } | Node::New { callee, .. } => callee,
        Node::TaggedTemplate { tag, .. } => tag,
        _ => return None,
    };
    construct_decl(cx, callee)
}

/// The class (or construct signature) an expression names.
fn construct_decl(cx: &Ctx<'_, '_>, expr: NodeId) -> Option<NodeId> {
    let ti = cx.types();
    let expr = skip_parens(cx.ast(), expr);
    if let Some(t) = ti.type_at(expr)
        && t != Ty::UNRESOLVED
        && let Some(sig) = ti.signatures(t, true).into_iter().next()
        && let Some(d) = ti.signature_decl(sig)
        && d.in_this_file
    {
        return Some(d.node);
    }
    let s = ti.symbol_at(expr)?;
    ti.symbol_decls(s)
        .into_iter()
        .filter(|d| d.in_this_file)
        .map(|d| d.node)
        .find(|&n| {
            matches!(
                cx.node(n),
                Node::ClassDecl { .. } | Node::ClassExpr { .. } | Node::FunctionDecl { .. }
            )
        })
}

/// The last type declaration (or, for JSX, any declaration) of the named symbol.
fn type_side_decl(cx: &Ctx<'_, '_>, name: NodeId, types_only: bool) -> Option<NodeId> {
    let ti = cx.types();
    let name = match *cx.node(name) {
        Node::QualifiedName { right, .. } => right,
        Node::Member { prop, .. } => prop,
        _ => name,
    };
    let s = ti.symbol_at(name)?;
    let s = if ti.symbol_facts(s).alias {
        ti.resolve_alias(s).unwrap_or(s)
    } else {
        s
    };
    ti.symbol_decls(s)
        .into_iter()
        .filter(|d| d.in_this_file)
        .map(|d| d.node)
        .rfind(|&n| {
            matches!(
                cx.node(n),
                Node::ClassDecl { .. }
                    | Node::ClassExpr { .. }
                    | Node::Interface { .. }
                    | Node::TypeAlias { .. }
            ) || (!types_only && matches!(cx.node(n), Node::FunctionDecl { .. }))
        })
}

fn decl_type_params(cx: &Ctx<'_, '_>, d: NodeId) -> Option<NodeList> {
    let tp = match *cx.node(d) {
        Node::FunctionDecl { type_params, .. }
        | Node::FunctionExpr { type_params, .. }
        | Node::Arrow { type_params, .. }
        | Node::ClassDecl { type_params, .. }
        | Node::ClassExpr { type_params, .. }
        | Node::Interface { type_params, .. }
        | Node::TypeAlias { type_params, .. }
        | Node::MethodSignature { type_params, .. }
        | Node::CallSignature { type_params, .. }
        | Node::ConstructSignature { type_params, .. }
        | Node::FunctionType { type_params, .. }
        | Node::ConstructorType { type_params, .. } => type_params,
        Node::MethodDef { kind, value, .. } => {
            if kind == MethodKind::Constructor {
                let class = cx.parent(d)?;
                return decl_type_params(cx, class);
            }
            return decl_type_params(cx, value);
        }
        _ => None,
    }?;
    match *cx.node(tp) {
        Node::TypeParams { params } => Some(params),
        _ => None,
    }
}
