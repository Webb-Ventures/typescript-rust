//! `@typescript-eslint/prefer-function-type`: an interface (with no supertype other than
//! `Function`) or type literal whose only member is a call or construct signature should be
//! a function type.
//!
//! An interface whose own body uses `this` (outside nested type literals) reports that
//! `this` instead, without a fix. The fix rewrites `(...): R` as `(...) => R` (in
//! parentheses inside a union, intersection or array type), turns an interface into a type
//! alias, and keeps the comments next to the member: above an exported interface, else in
//! front of the new type. An interface under `export default` gets no fix.

use super::prelude::*;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/prefer-function-type",
        "Enforce using function types instead of interfaces with call signatures",
        TS_STYLISTIC,
        Category::Style,
    )
    .fixable(),
    create: create_default::<PreferFunctionType>,
};

#[derive(Debug, Default)]
struct PreferFunctionType;

/// `this` types in `id`'s subtree, outside nested type literals, in source order.
fn this_types(ast: &Ast, id: NodeId, out: &mut Vec<NodeId>, depth: u32) {
    if depth > 400 {
        return;
    }
    match ast.node(id) {
        Node::ThisType {} => out.push(id),
        Node::TypeLiteral { .. } => {}
        _ => {
            let mut kids = Vec::new();
            ast.for_each_child(id, |c| kids.push(c));
            for k in kids {
                this_types(ast, k, out, depth + 1);
            }
        }
    }
}

/// The ESTree start of an interface: after `export` / `default`.
fn decl_start(src: &str, span: Span) -> u32 {
    let mut pos = span.start as usize;
    for _ in 0..4 {
        let at = skip_trivia_forward(src, pos);
        let rest = src.get(at..span.end as usize).unwrap_or("");
        let word = rest.split(|c: char| !is_ident_char(c)).next().unwrap_or("");
        if matches!(word, "export" | "default") {
            pos = at + word.len();
        } else {
            return at as u32;
        }
    }
    pos as u32
}

impl PreferFunctionType {
    #[allow(clippy::too_many_arguments)]
    fn check(
        &self,
        cx: &mut Ctx<'_, '_>,
        member: NodeId,
        node: NodeId,
        node_span: Span,
        interface: Option<(NodeId, Option<NodeId>, Modifiers)>,
        this: &[NodeId],
    ) {
        let ast = cx.ast();
        let return_type = match *ast.node(member) {
            Node::CallSignature { return_type, .. }
            | Node::ConstructSignature { return_type, .. } => return_type,
            _ => None,
        };
        let Some(ret) = return_type else { return };
        if let (Some((name, ..)), Some(&t)) = (interface, this.first()) {
            let n = cx.text(name).to_owned();
            cx.report(
                cx.span(t),
                format!(
                    "`this` refers to the function type '{n}', did you intend to use a generic `this` parameter like `<Self>(this: Self, ...) => Self` instead?"
                ),
            )
            .id("unexpectedThisOnFunctionOnlyInterface");
            return;
        }
        let phrase = if interface.is_some() {
            "Interface"
        } else {
            "Type literal"
        };
        let msg =
            format!("{phrase} only has a call signature, you should use a function type instead.");
        let member_span = cx.span(member);
        let fix = match interface {
            Some((_, _, m)) if m.contains(Modifiers::DEFAULT) => None,
            _ => self.fix(cx, member_span, ret, node, node_span, interface),
        };
        cx.report(member_span, msg)
            .id("functionTypeOverCallableType")
            .fix_opt(fix);
    }

    #[allow(clippy::too_many_arguments)]
    fn fix(
        &self,
        cx: &Ctx<'_, '_>,
        member_span: Span,
        ret: NodeId,
        node: NodeId,
        node_span: Span,
        interface: Option<(NodeId, Option<NodeId>, Modifiers)>,
    ) -> Option<Fix> {
        let ast = cx.ast();
        let src = cx.src();
        // The return type annotation's colon.
        let colon = skip_trivia_backward(ast, src, ast.span(ret).start as usize);
        let colon = colon.checked_sub(1)? as u32;
        if src.as_bytes().get(colon as usize) != Some(&b':') || colon < member_span.start {
            return None;
        }
        let text = member_span.text(src);
        let at = (colon - member_span.start) as usize;
        let mut s = format!("{} =>{}", &text[..at], &text[at + 1..]);
        let semi = s.ends_with(';');
        if semi {
            s.pop();
        }
        // The literal's parent, through parentheses.
        let mut p = cx.parent(node);
        for _ in 0..10_000 {
            match p {
                Some(q) if matches!(cx.node(q), Node::ParenType { .. }) => p = cx.parent(q),
                _ => break,
            }
        }
        if interface.is_none()
            && p.is_some_and(|q| {
                matches!(
                    cx.node(q),
                    Node::UnionType { .. } | Node::IntersectionType { .. } | Node::ArrayType { .. }
                )
            })
        {
            s = format!("({s})");
        }
        let last = if semi { ";" } else { "" };
        if let Some((name, tp, _)) = interface {
            s = match tp {
                Some(tp) => format!(
                    "type {} = {s}{last}",
                    &src[cx.span(name).start as usize..cx.span(tp).end as usize]
                ),
                None => format!("type {} = {s}{last}", cx.text(name)),
            };
        }
        // Comments right before and after the member.
        let before_end = skip_trivia_backward(ast, src, member_span.start as usize) as u32;
        let after_end = skip_trivia_forward(src, member_span.end as usize) as u32;
        let comments: Vec<Span> = ast
            .comments
            .iter()
            .filter(|c| c.span.start >= before_end && c.span.end <= member_span.start)
            .chain(
                ast.comments
                    .iter()
                    .filter(|c| c.span.start >= member_span.end && c.span.end <= after_end),
            )
            .map(|c| c.span)
            .collect();
        let exported = interface.is_some_and(|(_, _, m)| m.contains(Modifiers::EXPORT));
        let mut fix = Fix::replace(node_span, String::new());
        if exported {
            let text: String = comments
                .iter()
                .map(|c| format!("{}\n", c.text(src)))
                .collect();
            fix = Fix::insert(cx.span(node).start, text);
        } else {
            let line = cx.file.lines.line_of(member_span.start);
            for c in &comments {
                let sep = if cx.file.lines.line_of(c.start) == line {
                    " "
                } else {
                    "\n"
                };
                s = format!("{}{sep}{s}", c.text(src));
            }
        }
        if exported {
            Some(fix.and(node_span, s))
        } else {
            Some(Fix::replace(node_span, s))
        }
    }
}

impl Rule for PreferFunctionType {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Interface, NodeKind::TypeLiteral]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        match *ast.node(node) {
            Node::Interface {
                modifiers,
                name,
                type_params,
                extends,
                body,
            } => {
                let ext = ast.list(extends);
                let one_super = match ext {
                    [] => false,
                    [e] => {
                        let expr = match *ast.node(*e) {
                            Node::ExprWithTypeArgs { expr, .. } => expr,
                            _ => *e,
                        };
                        ident_name(ast, expr) != Some("Function")
                    }
                    _ => true,
                };
                let &[member] = ast.list(body) else { return };
                if one_super {
                    return;
                }
                let mut this = Vec::new();
                this_types(ast, node, &mut this, 0);
                let span = cx.span(node);
                let span = Span::new(decl_start(cx.src(), span), span.end);
                self.check(
                    cx,
                    member,
                    node,
                    span,
                    Some((name, type_params, modifiers)),
                    &this,
                );
            }
            Node::TypeLiteral { members } => {
                let &[member] = ast.list(members) else { return };
                let span = cx.span(node);
                self.check(cx, member, node, span, None, &[]);
            }
            _ => {}
        }
    }
}
