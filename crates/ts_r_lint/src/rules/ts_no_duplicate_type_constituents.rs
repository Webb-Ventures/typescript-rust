//! `@typescript-eslint/no-duplicate-type-constituents`: a union or intersection that
//! repeats a type, and `undefined` written out on an optional parameter.

use super::prelude::*;
use ts_r_span::Span;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-duplicate-type-constituents",
        "Disallow duplicate constituents of union or intersection types",
        TS_REC_TC,
        Category::Style,
    )
    .typed()
    .fixable(),
    create: create::<NoDuplicateTypeConstituents>,
};

#[derive(Debug)]
struct NoDuplicateTypeConstituents {
    ignore_unions: bool,
    ignore_intersections: bool,
}

impl FromOptions for NoDuplicateTypeConstituents {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(NoDuplicateTypeConstituents {
            ignore_unions: bool_field(o, "ignoreUnions", false),
            ignore_intersections: bool_field(o, "ignoreIntersections", false),
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Op {
    Union,
    Intersection,
}

fn op_of(n: &Node) -> Option<(Op, ts_r_ast::NodeList)> {
    match *n {
        Node::UnionType { types } => Some((Op::Union, types)),
        Node::IntersectionType { types } => Some((Op::Intersection, types)),
        _ => None,
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

/// Deletes constituent `list[i]` together with the operator next to it.
fn removal(cx: &Ctx<'_, '_>, list: &[NodeId], i: usize, op: Op) -> Option<Fix> {
    let src = cx.src();
    let ch = if op == Op::Union { b'|' } else { b'&' };
    let node = cx.span(list[i]);
    let at = if i > 0 {
        skip_trivia_forward(src, cx.span(list[i - 1]).end as usize)
    } else {
        skip_trivia_forward(src, node.end as usize)
    };
    if src.as_bytes().get(at) != Some(&ch) {
        return None;
    }
    let at = at as u32;
    Some(Fix::delete(node).and(Span::new(at, at + 1), ""))
}

struct Seen {
    ty: Ty,
    shown: String,
    text: String,
}

impl NoDuplicateTypeConstituents {
    fn walk(
        &self,
        cx: &mut Ctx<'_, '_>,
        list: &[NodeId],
        op: Op,
        seen: &mut Vec<Seen>,
        depth: u32,
    ) {
        if depth > 64 {
            return;
        }
        for (i, &child) in list.iter().enumerate() {
            let inner = skip_paren_types(cx, child);
            let ti = cx.types();
            let group = op_of(cx.node(inner)).filter(|(o, _)| *o == op);
            let t = ti
                .type_at(inner)
                .filter(|&t| !is_unresolved(ti, t) && any_kind(ti, t) != Some(AnyKind::Error));
            if let Some(t) = t {
                let shown = ti.type_to_string(t);
                if let Some(prev) = seen.iter().find(|s| s.ty == t || s.shown == shown) {
                    let what = if op == Op::Union {
                        "Union"
                    } else {
                        "Intersection"
                    };
                    let msg = format!("{what} type constituent is duplicated with {}.", prev.text);
                    let fix = removal(cx, list, i, op);
                    // A parenthesized constituent's range runs to the closing parenthesis.
                    let span = Span::new(cx.span(inner).start, cx.span(child).end);
                    cx.report(span, msg).id("duplicate").fix_opt(fix);
                    continue;
                }
                seen.push(Seen {
                    ty: t,
                    shown,
                    text: cx.text(inner).to_string(),
                });
            }
            if let Some((_, types)) = group {
                let members = cx.ast().list(types).to_vec();
                self.walk(cx, &members, op, seen, depth + 1);
            }
        }
    }

    /// `undefined` constituents of an optional parameter's union annotation.
    fn unnecessary_undefined(cx: &mut Ctx<'_, '_>, list: &[NodeId], depth: u32) {
        if depth > 64 {
            return;
        }
        for (i, &child) in list.iter().enumerate() {
            let inner = skip_paren_types(cx, child);
            if let Some((Op::Union, types)) = op_of(cx.node(inner)) {
                let members = cx.ast().list(types).to_vec();
                Self::unnecessary_undefined(cx, &members, depth + 1);
                continue;
            }
            let ti = cx.types();
            let Some(t) = ti.type_at(inner) else { continue };
            if t == Ty::UNRESOLVED || ti.flags(t) != TypeFlags::UNDEFINED {
                continue;
            }
            let fix = removal(cx, list, i, Op::Union);
            cx.report_node(
                inner,
                "Explicit undefined is unnecessary on an optional parameter.",
            )
            .id("unnecessary")
            .fix_opt(fix);
        }
    }
}

impl Rule for NoDuplicateTypeConstituents {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::UnionType,
            NodeKind::IntersectionType,
            NodeKind::Param,
        ]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        if let Node::Param {
            optional: true,
            type_ann: Some(ann),
            ..
        } = *cx.node(node)
        {
            let ann = skip_paren_types(cx, ann);
            if let Some((Op::Union, types)) = op_of(cx.node(ann)) {
                let members = cx.ast().list(types).to_vec();
                Self::unnecessary_undefined(cx, &members, 0);
            }
            return;
        }
        let Some((op, types)) = op_of(cx.node(node)) else {
            return;
        };
        if (op == Op::Union && self.ignore_unions)
            || (op == Op::Intersection && self.ignore_intersections)
        {
            return;
        }
        // Nested groups of the same operator are handled with their outermost group.
        let mut p = cx.parent(node);
        while let Some(pp) = p
            && matches!(cx.node(pp), Node::ParenType { .. })
        {
            p = cx.parent(pp);
        }
        if p.and_then(|p| op_of(cx.node(p)))
            .is_some_and(|(o, _)| o == op)
        {
            return;
        }
        let members = cx.ast().list(types).to_vec();
        let mut seen = Vec::new();
        self.walk(cx, &members, op, &mut seen, 0);
    }
}
