//! `arrow-body-style`: braces around arrow function bodies (`as-needed`, `always`, `never`).

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "arrow-body-style",
        "Require braces around arrow function bodies",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<ArrowBodyStyle>,
};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Mode {
    #[default]
    AsNeeded,
    Always,
    Never,
}

#[derive(Debug, Default)]
struct ArrowBodyStyle {
    mode: Mode,
    require_return_for_object: bool,
}

impl FromOptions for ArrowBodyStyle {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let mode = match opts.first().and_then(Value::as_str) {
            Some("always") => Mode::Always,
            Some("never") => Mode::Never,
            _ => Mode::AsNeeded,
        };
        Ok(ArrowBodyStyle {
            mode,
            require_return_for_object: bool_field(
                obj_opt(opts, 1),
                "requireReturnForObjectLiteral",
                false,
            ),
        })
    }
}

fn is_paren(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    matches!(cx.node(id), Node::Paren { .. })
}

/// An `in` expression anywhere in the arrow.
fn has_in(cx: &Ctx<'_, '_>, arrow: NodeId) -> bool {
    let ast = cx.ast();
    let mut stack = vec![arrow];
    while let Some(n) = stack.pop() {
        if let Node::Binary {
            op: BinaryOp::In, ..
        } = *ast.node(n)
        {
            return true;
        }
        ast.for_each_child(n, |c| stack.push(c));
    }
    false
}

fn in_for_init(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let mut cur = node;
    while let Some(p) = cx.parent(cur) {
        if let Node::For { init: Some(i), .. } = *ast.node(p)
            && i == cur
        {
            return true;
        }
        cur = p;
    }
    false
}

impl ArrowBodyStyle {
    fn block_fix(&self, cx: &Ctx<'_, '_>, arrow: NodeId, body: NodeId, ret: NodeId) -> Option<Fix> {
        let ast = cx.ast();
        let src = cx.src();
        let Node::Return { arg: Some(arg) } = *ast.node(ret) else {
            return None;
        };
        let bspan = cx.span(body);
        let after = skip_trivia_forward(src, bspan.end as usize);
        if src[after..].starts_with(['(', '[', '/', '`', '+', '-']) {
            return None;
        }
        let open = bspan.start;
        let close = bspan.end - 1;
        let rspan = cx.span(ret);
        let first = skip_trivia_forward(src, rspan.start as usize + "return".len()) as u32;
        let last_end = rspan.end;
        let semi = src[..last_end as usize].ends_with(';');
        let comments = !ast.comments_in(Span::new(open + 1, first)).is_empty()
            || !ast.comments_in(Span::new(last_end, close)).is_empty();
        let mut fix = if comments {
            Fix::delete(Span::new(open, open + 1))
                .and(Span::new(close, close + 1), "")
                .and(
                    Span::new(rspan.start, rspan.start + "return".len() as u32),
                    "",
                )
        } else {
            Fix::delete(Span::new(open, first)).and(Span::new(last_end, close + 1), "")
        };
        let needs_parens = src[first as usize..].starts_with('{')
            || matches!(ast.node(arg), Node::Seq { .. })
            || (has_in(cx, arrow) && in_for_init(cx, arrow));
        if needs_parens && !is_paren(cx, arg) {
            fix = fix.and(Span::at(first), "(").and(Span::at(last_end), ")");
        }
        if semi {
            fix = fix.and(Span::new(last_end - 1, last_end), "");
        }
        Some(fix)
    }

    fn expr_fix(&self, cx: &Ctx<'_, '_>, arrow: NodeId, body: NodeId) -> Fix {
        let ast = cx.ast();
        let src = cx.src();
        let end = cx.span(arrow).end;
        let first = cx.span(body).start;
        let second = skip_trivia_forward(src, first as usize + 1) as u32;
        // The object literal whose `{` follows the `(`.
        let mut inner = None;
        let mut n = body;
        for _ in 0..10_000 {
            if matches!(ast.node(n), Node::ObjectLit { .. }) && cx.span(n).start == second {
                inner = Some(n);
                break;
            }
            n = match *ast.node(n) {
                Node::Paren { expr } => expr,
                _ => match leading_child(ast, n) {
                    Some(c) => c,
                    None => break,
                },
            };
        }
        if let Some(inner) = inner
            && src[first as usize..].starts_with('(')
            && src[second as usize..].starts_with('{')
        {
            let mut fix = if has_newline(src, first, second) {
                Fix::replace(Span::new(first, first + 1), "{").and(Span::at(second), "return ")
            } else {
                Fix::replace(Span::new(first, first + 1), "{return ")
            };
            // The `)` closing the innermost parenthesised node around the object.
            let mut cur = inner;
            while let Some(p) = cx.parent(cur) {
                if is_paren(cx, p) {
                    let c = cx.span(p).end;
                    fix = fix.and(Span::new(c - 1, c), "");
                    break;
                }
                cur = p;
            }
            return fix.and(Span::at(end), "}");
        }
        Fix::insert(first, "{return ").and(Span::at(end), "}")
    }
}

impl Rule for ArrowBodyStyle {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Arrow]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Arrow { flags, body, .. } = *cx.node(node) else {
            return;
        };
        let never = self.mode == Mode::Never;
        let as_needed = self.mode == Mode::AsNeeded;
        if !flags.contains(FnFlags::EXPRESSION_BODY) {
            let Node::Block { stmts } = *ast.node(body) else {
                return;
            };
            let stmts = ast.list(stmts);
            if stmts.len() != 1 && !never {
                return;
            }
            let ret = stmts.first().copied();
            let ret_arg = ret.and_then(|r| match *ast.node(r) {
                Node::Return { arg } => Some(arg),
                _ => None,
            });
            if as_needed
                && self.require_return_for_object
                && ret_arg
                    .flatten()
                    .is_some_and(|a| matches!(ast.node(a), Node::ObjectLit { .. }))
            {
                return;
            }
            if !(never || (as_needed && ret_arg.is_some())) {
                return;
            }
            let src = cx.src();
            let (id, msg) = if stmts.is_empty() {
                (
                    "unexpectedEmptyBlock",
                    "Unexpected block statement surrounding arrow body; put a value of `undefined` immediately after the `=>`.",
                )
            } else if stmts.len() > 1 || ret_arg.is_none() {
                (
                    "unexpectedOtherBlock",
                    "Unexpected block statement surrounding arrow body.",
                )
            } else if ret_arg.flatten().is_some()
                && src
                    [skip_trivia_forward(src, cx.span(stmts[0]).start as usize + "return".len())..]
                    .starts_with('{')
            {
                (
                    "unexpectedObjectBlock",
                    "Unexpected block statement surrounding arrow body; parenthesize the returned value and move it immediately after the `=>`.",
                )
            } else {
                (
                    "unexpectedSingleBlock",
                    "Unexpected block statement surrounding arrow body; move the returned value immediately after the `=>`.",
                )
            };
            let fix = match (stmts, ret_arg) {
                ([r], Some(Some(_))) => self.block_fix(cx, node, body, *r),
                _ => None,
            };
            cx.report(cx.span(body), msg).id(id).fix_opt(fix);
            return;
        }
        let inner = skip_parens(ast, body);
        if self.mode == Mode::Always
            || (as_needed
                && self.require_return_for_object
                && matches!(ast.node(inner), Node::ObjectLit { .. }))
        {
            let fix = self.expr_fix(cx, node, body);
            cx.report(
                cx.span(inner),
                "Expected block statement surrounding arrow body.",
            )
            .id("expectedBlock")
            .fix(fix);
        }
    }
}
