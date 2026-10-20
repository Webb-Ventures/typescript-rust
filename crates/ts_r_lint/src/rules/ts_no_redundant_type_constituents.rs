//! `@typescript-eslint/no-redundant-type-constituents`: union and intersection members
//! that do nothing (`any`, `unknown`, `never`, literals next to their primitive).

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-redundant-type-constituents",
        "Disallow members of unions and intersections that do nothing or override type information",
        TS_REC_TC,
        Category::Suspicious,
    )
    .typed(),
    create: create_default::<NoRedundantTypeConstituents>,
};

#[derive(Debug, Default)]
struct NoRedundantTypeConstituents;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Prim {
    Str,
    Num,
    Big,
    Bool,
}

impl Prim {
    fn name(self) -> &'static str {
        match self {
            Prim::Str => "string",
            Prim::Num => "number",
            Prim::Big => "bigint",
            Prim::Bool => "boolean",
        }
    }
}

/// `boolean` itself (the union `false | true`).
fn is_boolean(ti: &dyn TypeInfo, t: Ty) -> bool {
    let f = ti.flags(t);
    if f.contains(TypeFlags::BOOLEAN) {
        return true;
    }
    f.contains(TypeFlags::UNION) && {
        let ms = ti.union_members(t);
        ms.len() == 2
            && ms
                .iter()
                .all(|&m| m != Ty::UNRESOLVED && ti.flags(m).contains(TypeFlags::BOOLEAN_LITERAL))
    }
}

/// The primitive a type is exactly (`string`, `number`, `bigint`, `boolean`).
fn primitive(ti: &dyn TypeInfo, t: Ty) -> Option<Prim> {
    if t == Ty::UNRESOLVED {
        return None;
    }
    if is_boolean(ti, t) {
        return Some(Prim::Bool);
    }
    let f = ti.flags(t);
    if f == TypeFlags::STRING {
        Some(Prim::Str)
    } else if f == TypeFlags::NUMBER {
        Some(Prim::Num)
    } else if f == TypeFlags::BIGINT {
        Some(Prim::Big)
    } else {
        None
    }
}

/// The primitive a literal type belongs to.
fn literal_class(ti: &dyn TypeInfo, t: Ty) -> Option<Prim> {
    if t == Ty::UNRESOLVED {
        return None;
    }
    let f = ti.flags(t);
    if f.contains(TypeFlags::ENUM_LITERAL) {
        None
    } else if f.intersects(TypeFlags::STRING_LITERAL | TypeFlags::TEMPLATE_LITERAL) {
        Some(Prim::Str)
    } else if f.contains(TypeFlags::NUMBER_LITERAL) {
        Some(Prim::Num)
    } else if f.contains(TypeFlags::BIGINT_LITERAL) {
        Some(Prim::Big)
    } else if f.contains(TypeFlags::BOOLEAN_LITERAL) {
        Some(Prim::Bool)
    } else {
        None
    }
}

fn literal_shown(ti: &dyn TypeInfo, t: Ty) -> String {
    if ti.flags(t).contains(TypeFlags::TEMPLATE_LITERAL) {
        "template literal type".to_string()
    } else {
        ti.type_to_string(t)
    }
}

fn skip_paren_types(cx: &Ctx<'_, '_>, mut n: NodeId) -> NodeId {
    for _ in 0..256 {
        match *cx.node(n) {
            Node::ParenType { ty } => n = ty,
            _ => break,
        }
    }
    n
}

/// The literal types a union constituent is written with, in source order (nested unions
/// and aliases of unions are expanded; duplicates are kept).
fn literal_atoms(cx: &Ctx<'_, '_>, node: NodeId, out: &mut Vec<Ty>, depth: u32) {
    if depth > 64 {
        return;
    }
    let node = skip_paren_types(cx, node);
    if let Node::UnionType { types } = *cx.node(node) {
        for &m in cx.ast().list(types) {
            literal_atoms(cx, m, out, depth + 1);
        }
        return;
    }
    let ti = cx.types();
    let Some(t) = ti.type_at(node) else { return };
    if t == Ty::UNRESOLVED || is_boolean(ti, t) {
        return;
    }
    out.extend(parts(ti, t));
}

/// A union in the return type of a function or signature (where `never` is meaningful).
fn in_return_type(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let mut child = node;
    let mut p = cx.parent(child);
    while let Some(pp) = p
        && matches!(cx.node(pp), Node::ParenType { .. })
    {
        child = pp;
        p = cx.parent(pp);
    }
    let Some(p) = p else { return false };
    match *cx.node(p) {
        Node::FunctionType { return_type, .. } | Node::ConstructorType { return_type, .. } => {
            return_type == child
        }
        Node::FunctionDecl { return_type, .. }
        | Node::FunctionExpr { return_type, .. }
        | Node::Arrow { return_type, .. }
        | Node::MethodSignature { return_type, .. }
        | Node::CallSignature { return_type, .. }
        | Node::ConstructSignature { return_type, .. } => return_type == Some(child),
        _ => false,
    }
}

impl NoRedundantTypeConstituents {
    /// `any`, the error type, `unknown` and `never` members. Returns true when reported.
    fn keyword_member(
        cx: &mut Ctx<'_, '_>,
        m: NodeId,
        t: Ty,
        union: bool,
        return_type: bool,
    ) -> bool {
        let ti = cx.types();
        let what = if union { "union" } else { "intersection" };
        if any_kind(ti, t) == Some(AnyKind::Error) {
            let name = cx.text(m).to_string();
            cx.report_node(
                m,
                format!("'{name}' is an 'error' type that acts as 'any' and overrides all other types in this {what} type."),
            )
            .id("errorTypeOverrides");
            return true;
        }
        let f = ti.flags(t);
        let (kw, overrides) = if f.contains(TypeFlags::ANY) {
            ("any", true)
        } else if f.contains(TypeFlags::UNKNOWN) {
            ("unknown", union)
        } else if f.contains(TypeFlags::NEVER) {
            if union && return_type {
                return false;
            }
            ("never", !union)
        } else {
            return false;
        };
        if overrides {
            cx.report_node(
                m,
                format!("'{kw}' overrides all other types in this {what} type."),
            )
            .id("overrides");
        } else {
            cx.report_node(
                m,
                format!("'{kw}' is overridden by other types in this {what} type."),
            )
            .id("overridden");
        }
        true
    }

    fn check_union(cx: &mut Ctx<'_, '_>, node: NodeId, members: &[NodeId]) {
        let return_type = in_return_type(cx, node);
        let ti = cx.types();
        let types: Vec<Option<Ty>> = members
            .iter()
            .map(|&m| {
                ti.type_at(skip_paren_types(cx, m))
                    .filter(|&t| t != Ty::UNRESOLVED)
            })
            .collect();
        let prims: Vec<Prim> = types
            .iter()
            .flatten()
            .filter_map(|&t| primitive(ti, t))
            .collect();
        for (&m, t) in members.iter().zip(&types) {
            let inner = skip_paren_types(cx, m);
            let Some(t) = *t else { continue };
            if Self::keyword_member(cx, inner, t, true, return_type) {
                continue;
            }
            let ti = cx.types();
            if primitive(ti, t).is_some() {
                continue;
            }
            let mut atoms = Vec::new();
            literal_atoms(cx, inner, &mut atoms, 0);
            for p in [Prim::Str, Prim::Num, Prim::Big, Prim::Bool] {
                if !prims.contains(&p) {
                    continue;
                }
                let shown: Vec<String> = atoms
                    .iter()
                    .filter(|&&a| literal_class(ti, a) == Some(p))
                    .map(|&a| literal_shown(ti, a))
                    .collect();
                if shown.is_empty() {
                    continue;
                }
                cx.report_node(
                    inner,
                    format!(
                        "{} is overridden by {} in this union type.",
                        shown.join(" | "),
                        p.name()
                    ),
                )
                .id("literalOverridden");
            }
        }
    }

    fn check_intersection(cx: &mut Ctx<'_, '_>, members: &[NodeId]) {
        let ti = cx.types();
        let types: Vec<Option<Ty>> = members
            .iter()
            .map(|&m| {
                ti.type_at(skip_paren_types(cx, m))
                    .filter(|&t| t != Ty::UNRESOLVED)
            })
            .collect();
        let mut prim_nodes: Vec<(NodeId, Prim)> = Vec::new();
        let mut literals: Vec<(NodeId, Ty, Prim)> = Vec::new();
        for (&m, t) in members.iter().zip(&types) {
            let inner = skip_paren_types(cx, m);
            let Some(t) = *t else { continue };
            if Self::keyword_member(cx, inner, t, false, false) {
                continue;
            }
            let ti = cx.types();
            if let Some(p) = primitive(ti, t) {
                prim_nodes.push((inner, p));
                continue;
            }
            // A literal, or a union of literals of one primitive.
            let ps = parts(ti, t);
            let classes: Vec<Option<Prim>> = ps.iter().map(|&p| literal_class(ti, p)).collect();
            if let Some(Some(c)) = classes.first()
                && classes.iter().all(|k| *k == Some(*c))
            {
                literals.push((inner, t, *c));
            }
        }
        let ti = cx.types();
        for (lit_node, lt, c) in literals {
            for &(pn, p) in &prim_nodes {
                if p != c {
                    continue;
                }
                let is_union = ti.flags(lt).contains(TypeFlags::UNION);
                let shown = parts(ti, lt)
                    .into_iter()
                    .map(|a| literal_shown(ti, a))
                    .collect::<Vec<_>>()
                    .join(" | ");
                let at = if is_union { lit_node } else { pn };
                cx.report_node(
                    at,
                    format!(
                        "{} is overridden by the {shown} in this intersection type.",
                        p.name()
                    ),
                )
                .id("primitiveOverridden");
            }
        }
    }
}

impl Rule for NoRedundantTypeConstituents {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::UnionType, NodeKind::IntersectionType]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        match *cx.node(node) {
            Node::UnionType { types } => {
                let members = cx.ast().list(types).to_vec();
                Self::check_union(cx, node, &members);
            }
            Node::IntersectionType { types } => {
                let members = cx.ast().list(types).to_vec();
                Self::check_intersection(cx, &members);
            }
            _ => {}
        }
    }
}
