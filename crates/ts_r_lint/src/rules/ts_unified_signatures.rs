//! `@typescript-eslint/unified-signatures`: disallow overloads that could be one signature
//! with a union or an optional/rest parameter.

use super::prelude::*;
use crate::ast_util::compact_text;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/unified-signatures",
        "Disallow two overloads that could be unified into one with a union or an optional/rest parameter",
        TS_STRICT,
        Category::Style,
    ),
    create: create::<UnifiedSignatures>,
};

#[derive(Debug)]
struct UnifiedSignatures {
    same_names: bool,
    same_jsdoc: bool,
}

impl FromOptions for UnifiedSignatures {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(UnifiedSignatures {
            same_names: bool_field(o, "ignoreDifferentlyNamedParameters", false),
            same_jsdoc: bool_field(o, "ignoreOverloadsWithDifferentJSDoc", false),
        })
    }
}

/// One overload signature.
struct Sig<'a> {
    /// Reported for identical overloads.
    report: Span,
    /// Its start, for "the one on line N".
    start: u32,
    type_params: Option<NodeId>,
    params: &'a [NodeId],
    ret: Option<NodeId>,
    doc: Option<&'a str>,
}

/// A parameter: name text, rest, optional, type.
struct P {
    node: NodeId,
    name: String,
    rest: bool,
    optional: bool,
    ty: Option<NodeId>,
    this: bool,
}

fn param(cx: &Ctx<'_, '_>, id: NodeId) -> P {
    match *cx.node(id) {
        Node::Param {
            rest,
            pattern,
            optional,
            type_ann,
            ..
        } => {
            let name = cx.text(pattern).to_owned();
            P {
                node: id,
                this: name == "this",
                name,
                rest,
                optional,
                ty: type_ann,
            }
        }
        _ => P {
            node: id,
            name: String::new(),
            rest: false,
            optional: false,
            ty: None,
            this: false,
        },
    }
}

fn skip_paren_type(cx: &Ctx<'_, '_>, mut id: NodeId) -> NodeId {
    while let Node::ParenType { ty } = *cx.node(id) {
        id = ty;
    }
    id
}

fn same_opt(cx: &Ctx<'_, '_>, a: Option<NodeId>, b: Option<NodeId>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => compact_text(cx.text(a)) == compact_text(cx.text(b)),
        _ => false,
    }
}

/// The block comment directly before `pos` (only whitespace between).
fn leading_block_comment<'a>(cx: &Ctx<'a, '_>, pos: u32) -> Option<&'a str> {
    let ast = cx.ast();
    let src = cx.src();
    let i = ast.comments.partition_point(|c| c.span.end <= pos);
    let c = ast.comments.get(i.checked_sub(1)?)?;
    if !src[c.span.end as usize..pos as usize].trim().is_empty() {
        return None;
    }
    match c.kind {
        ts_r_ast::CommentKind::Block => Some(&src[c.span.start as usize..c.span.end as usize]),
        _ => None,
    }
}

/// Type parameter names declared by `owner` (a class or interface).
fn type_param_names(cx: &Ctx<'_, '_>, owner: NodeId) -> Vec<String> {
    let ast = cx.ast();
    let tp = match *cx.node(owner) {
        Node::ClassDecl { type_params, .. }
        | Node::ClassExpr { type_params, .. }
        | Node::Interface { type_params, .. } => type_params,
        _ => None,
    };
    let Some(tp) = tp else {
        return Vec::new();
    };
    let Node::TypeParams { params } = *ast.node(tp) else {
        return Vec::new();
    };
    ast.list(params)
        .iter()
        .filter_map(|&p| match *ast.node(p) {
            Node::TypeParam { name, .. } => Some(cx.text(name).to_owned()),
            _ => None,
        })
        .collect()
}

fn mentions(text: &str, names: &[String]) -> bool {
    text.split(|c: char| !is_ident_char(c))
        .any(|w| names.iter().any(|n| n == w))
}

enum Unify {
    Same,
    Single(NodeId, String),
    Omit(NodeId, bool),
}

impl UnifiedSignatures {
    fn params_equal(&self, cx: &Ctx<'_, '_>, a: &P, b: &P) -> bool {
        a.rest == b.rest && same_opt(cx, a.ty, b.ty) && (!self.same_names || a.name == b.name)
    }

    fn compare(
        &self,
        cx: &Ctx<'_, '_>,
        a: &Sig<'_>,
        b: &Sig<'_>,
        outer: &[String],
    ) -> Option<Unify> {
        if !same_opt(cx, a.type_params, b.type_params) || !same_opt(cx, a.ret, b.ret) {
            return None;
        }
        if self.same_jsdoc && a.doc != b.doc {
            return None;
        }
        let pa: Vec<P> = a.params.iter().map(|&p| param(cx, p)).collect();
        let pb: Vec<P> = b.params.iter().map(|&p| param(cx, p)).collect();
        if pa.len() == pb.len() {
            let diffs: Vec<usize> = (0..pa.len())
                .filter(|&i| !self.params_equal(cx, &pa[i], &pb[i]))
                .collect();
            match diffs.as_slice() {
                [] => return Some(Unify::Same),
                &[i] => {
                    let (x, y) = (&pa[i], &pb[i]);
                    if x.rest || y.rest || x.optional != y.optional {
                        return None;
                    }
                    if self.same_names && x.name != y.name {
                        return None;
                    }
                    if x.this && (is_void_ty(cx, x.ty) || is_void_ty(cx, y.ty)) {
                        return None;
                    }
                    return Some(Unify::Single(y.node, union_text(cx, x.ty, y.ty)));
                }
                _ => return None,
            }
        }
        let (short, long) = if pa.len() < pb.len() {
            (&pa, &pb)
        } else {
            (&pb, &pa)
        };
        if !short
            .iter()
            .zip(long.iter())
            .all(|(x, y)| self.params_equal(cx, x, y))
        {
            return None;
        }
        if short.last().is_some_and(|p| p.rest) {
            return None;
        }
        let extra = &long[short.len()..];
        if extra[0].this || extra[1..].iter().any(|p| !p.optional && !p.rest) {
            return None;
        }
        if extra
            .iter()
            .any(|p| p.ty.is_some_and(|t| mentions(cx.text(t), outer)))
        {
            return None;
        }
        let last = extra.last()?;
        Some(Unify::Omit(last.node, last.rest))
    }
}

fn is_void_ty(cx: &Ctx<'_, '_>, ty: Option<NodeId>) -> bool {
    ty.is_some_and(|t| {
        matches!(
            cx.node(skip_paren_type(cx, t)),
            Node::KeywordType {
                kind: KeywordTypeKind::Void
            }
        )
    })
}

/// `a | b` with union members flattened and duplicates dropped.
fn union_text(cx: &Ctx<'_, '_>, a: Option<NodeId>, b: Option<NodeId>) -> String {
    let ast = cx.ast();
    let mut parts: Vec<(&str, bool)> = Vec::new();
    for t in [a, b].into_iter().flatten() {
        let t = skip_paren_type(cx, t);
        let members: Vec<NodeId> = match *cx.node(t) {
            Node::UnionType { types } => ast.list(types).to_vec(),
            _ => vec![t],
        };
        for m in members {
            let m = skip_paren_type(cx, m);
            let text = cx.text(m);
            let wrap = matches!(
                cx.node(m),
                Node::FunctionType { .. }
                    | Node::ConstructorType { .. }
                    | Node::ConditionalType { .. }
            );
            if !parts.iter().any(|&(t, _)| t == text) {
                parts.push((text, wrap));
            }
        }
    }
    if let [(only, _)] = parts.as_slice() {
        return (*only).to_owned();
    }
    parts
        .iter()
        .map(|&(t, wrap)| if wrap { format!("({t})") } else { t.to_owned() })
        .collect::<Vec<_>>()
        .join(" | ")
}

/// The grouping key of an overload in a statement list, if it is a bodyless function.
fn decl_key(cx: &Ctx<'_, '_>, id: NodeId) -> Option<String> {
    match *cx.node(id) {
        Node::FunctionDecl {
            name, body: None, ..
        } => Some(name.map_or_else(|| "default".to_owned(), |n| cx.text(n).to_owned())),
        _ => None,
    }
}

fn key_text(cx: &Ctx<'_, '_>, key: NodeId) -> String {
    match key_name(cx.ast(), cx.src(), key) {
        Some(n)
            if !matches!(
                cx.node(key),
                Node::ComputedKey { .. } | Node::PrivateName { .. }
            ) =>
        {
            n.into_owned()
        }
        _ => compact_text(cx.text(key)),
    }
}

/// `report` span of a method's value, through a trailing `;`.
fn with_semicolon(cx: &Ctx<'_, '_>, span: Span) -> Span {
    let src = cx.src();
    if src[span.end as usize..].starts_with(';') {
        Span::new(span.start, span.end + 1)
    } else {
        span
    }
}

impl Rule for UnifiedSignatures {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::Program,
            NodeKind::ModuleBlock,
            NodeKind::ClassDecl,
            NodeKind::ClassExpr,
            NodeKind::Interface,
            NodeKind::TypeLiteral,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let (items, outer): (&[NodeId], Vec<String>) = match *cx.node(node) {
            Node::Program { stmts } | Node::ModuleBlock { stmts } => (ast.list(stmts), Vec::new()),
            Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. } => {
                (ast.list(members), type_param_names(cx, node))
            }
            Node::Interface { body, .. } => (ast.list(body), type_param_names(cx, node)),
            Node::TypeLiteral { members } => (ast.list(members), Vec::new()),
            _ => return,
        };
        let mut groups: Vec<(String, Vec<Sig<'_>>)> = Vec::new();
        for &m in items {
            let doc = leading_block_comment(cx, cx.span(m).start);
            let entry = match *cx.node(m) {
                Node::FunctionDecl {
                    type_params,
                    params,
                    return_type,
                    ..
                } => decl_key(cx, m).map(|k| {
                    (
                        k,
                        Sig {
                            report: cx.span(m),
                            start: cx.span(m).start,
                            type_params,
                            params: ast.list(params),
                            ret: return_type,
                            doc,
                        },
                    )
                }),
                Node::MethodDef {
                    kind: kind @ (MethodKind::Method | MethodKind::Constructor),
                    modifiers,
                    key,
                    value,
                    ..
                } => match *cx.node(value) {
                    Node::FunctionExpr {
                        type_params,
                        params,
                        return_type,
                        body: None,
                        ..
                    } => {
                        let k = if kind == MethodKind::Constructor {
                            "constructor".to_owned()
                        } else {
                            key_text(cx, key)
                        };
                        let k = if modifiers.contains(Modifiers::STATIC) {
                            format!("static {k}")
                        } else {
                            k
                        };
                        Some((
                            k,
                            Sig {
                                report: with_semicolon(cx, cx.span(value)),
                                start: cx.span(m).start,
                                type_params,
                                params: ast.list(params),
                                ret: return_type,
                                doc,
                            },
                        ))
                    }
                    _ => None,
                },
                Node::MethodSignature {
                    kind: MethodKind::Method,
                    key,
                    type_params,
                    params,
                    return_type,
                    ..
                } => Some((
                    key_text(cx, key),
                    Sig {
                        report: cx.span(m),
                        start: cx.span(m).start,
                        type_params,
                        params: ast.list(params),
                        ret: return_type,
                        doc,
                    },
                )),
                Node::CallSignature {
                    type_params,
                    params,
                    return_type,
                } => Some((
                    "\u{0}call".to_owned(),
                    Sig {
                        report: cx.span(m),
                        start: cx.span(m).start,
                        type_params,
                        params: ast.list(params),
                        ret: return_type,
                        doc,
                    },
                )),
                Node::ConstructSignature {
                    type_params,
                    params,
                    return_type,
                } => Some((
                    "\u{0}new".to_owned(),
                    Sig {
                        report: cx.span(m),
                        start: cx.span(m).start,
                        type_params,
                        params: ast.list(params),
                        ret: return_type,
                        doc,
                    },
                )),
                _ => None,
            };
            if let Some((k, sig)) = entry {
                match groups.iter_mut().find(|(gk, _)| *gk == k) {
                    Some((_, v)) => v.push(sig),
                    None => groups.push((k, vec![sig])),
                }
            }
        }
        let src = cx.src();
        let line = |pos: u32| src[..pos as usize].matches('\n').count() + 1;
        let mut hits: Vec<(Span, String, &'static str)> = Vec::new();
        for (_, sigs) in &groups {
            let only2 = sigs.len() == 2;
            for (i, a) in sigs.iter().enumerate() {
                for b in &sigs[i + 1..] {
                    let Some(u) = self.compare(cx, a, b, &outer) else {
                        continue;
                    };
                    let lead = if only2 {
                        "These overloads".to_owned()
                    } else {
                        format!("This overload and the one on line {}", line(a.start))
                    };
                    let (span, tail, id) = match u {
                        Unify::Same => (
                            b.report,
                            "with identical parameters".to_owned(),
                            "allParametersAreSame",
                        ),
                        Unify::Single(p, ty) => (
                            cx.span(p),
                            format!("taking `{ty}`"),
                            "singleParameterDifference",
                        ),
                        Unify::Omit(p, rest) => (
                            cx.span(p),
                            if rest {
                                "with a rest parameter".to_owned()
                            } else {
                                "with an optional parameter".to_owned()
                            },
                            if rest {
                                "omittingRestParameter"
                            } else {
                                "omittingSingleParameter"
                            },
                        ),
                    };
                    hits.push((
                        span,
                        format!("{lead} can be combined into one signature {tail}."),
                        id,
                    ));
                }
            }
        }
        for (span, msg, id) in hits {
            cx.report(span, msg).id(id);
        }
    }
}
