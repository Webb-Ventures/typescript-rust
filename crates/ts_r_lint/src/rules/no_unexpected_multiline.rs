//! `no-unexpected-multiline`: disallow confusing multiline expressions.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-unexpected-multiline",
        "Disallow confusing multiline expressions",
        ESLINT_REC,
        Category::Correctness,
    ),
    create: create_default::<NoUnexpectedMultiline>,
};

#[derive(Debug, Default)]
struct NoUnexpectedMultiline;

fn is_flags(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b"gimsuyvd".contains(&b))
}

impl NoUnexpectedMultiline {
    /// Reports `ch` when it starts a new line after `end`.
    fn check(cx: &mut Ctx<'_, '_>, end: u32, ch: u8, msg: &str, id: &'static str) {
        let src = cx.src();
        let Some(pos) = find_char(src, end, ch) else {
            return;
        };
        if skip_trivia_forward(src, end as usize) != pos as usize {
            return;
        }
        if has_newline(src, end, pos) {
            cx.report(Span::new(pos, pos + 1), msg).id(id);
        }
    }
}

impl Rule for NoUnexpectedMultiline {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::Call,
            NodeKind::Index,
            NodeKind::TaggedTemplate,
            NodeKind::Binary,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let src = cx.src();
        match *cx.node(node) {
            Node::Call {
                callee,
                optional: false,
                type_args,
                args,
            } => {
                if ast.list(args).is_empty() {
                    return;
                }
                let end = type_args.map_or(cx.span(callee).end, |t| cx.span(t).end);
                Self::check(
                    cx,
                    end,
                    b'(',
                    "Unexpected newline between function and ( of function call.",
                    "function",
                );
            }
            Node::Index {
                obj,
                optional: false,
                ..
            } => {
                Self::check(
                    cx,
                    cx.span(obj).end,
                    b'[',
                    "Unexpected newline between object and [ of property access.",
                    "property",
                );
            }
            Node::TaggedTemplate {
                tag,
                type_args,
                quasi,
            } => {
                let end = type_args.map_or(cx.span(tag).end, |t| cx.span(t).end);
                let q = cx.span(quasi).start;
                if has_newline(src, end, q) {
                    cx.report(
                        Span::new(q, q + 1),
                        "Unexpected newline between template tag and template literal.",
                    )
                    .id("taggedTemplate");
                }
            }
            Node::Binary {
                left,
                op: BinaryOp::Div,
                right,
            } => {
                let Node::Binary {
                    left: num,
                    op: BinaryOp::Div,
                    ..
                } = *ast.node(left)
                else {
                    return;
                };
                let num_end = cx.span(num).end;
                let Some(first) = find_char(src, num_end, b'/') else {
                    return;
                };
                if !has_newline(src, num_end, first) {
                    return;
                }
                let Some(second) = find_char(src, cx.span(left).end, b'/') else {
                    return;
                };
                let r = cx.span(right).start;
                if r != second + 1 {
                    return;
                }
                let word: &str = src[r as usize..]
                    .split(|c: char| !is_ident_char(c))
                    .next()
                    .unwrap_or("");
                if is_flags(word) {
                    cx.report(
                        Span::new(first, first + 1),
                        "Unexpected newline between numerator and division operator.",
                    )
                    .id("division");
                }
            }
            _ => {}
        }
    }
}
