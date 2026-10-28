//! `prefer-object-spread`: `{ ...a }` instead of `Object.assign({}, a)`.

use super::prefer_exponentiation_operator::{global_member_calls, statement_at_start};
use super::prelude::*;
use ts_r_ast::{Ast, CommentKind};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "prefer-object-spread",
        "Disallow using `Object.assign` with an object literal as the first argument and prefer \
         the use of object spread instead",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<PreferObjectSpread>,
};

#[derive(Debug, Default)]
struct PreferObjectSpread;

fn object_props(ast: &Ast, id: NodeId) -> Option<&[NodeId]> {
    match *ast.node(skip_parens(ast, id)) {
        Node::ObjectLit { props } => Some(ast.list(props)),
        _ => None,
    }
}

fn has_accessor(ast: &Ast, props: &[NodeId]) -> bool {
    props.iter().any(|&p| {
        matches!(
            ast.node(p),
            Node::MethodDef {
                kind: MethodKind::Get | MethodKind::Set,
                ..
            }
        )
    })
}

fn has_proto(ast: &Ast, src: &str, props: &[NodeId]) -> bool {
    props.iter().any(|&p| {
        let key = match *ast.node(p) {
            Node::Property { key, .. } | Node::MethodDef { key, .. } => key,
            Node::ShorthandProp { name, .. } => name,
            _ => return false,
        };
        key_name(ast, src, key).as_deref() == Some("__proto__")
    })
}

/// The previous non-whitespace position, and whether a line comment ends there.
fn start_with_spaces(ast: &Ast, src: &str, at: u32) -> u32 {
    let b = src.as_bytes();
    let mut s = at as usize;
    while s > 0 && b[s - 1].is_ascii_whitespace() {
        s -= 1;
    }
    let after_line_comment = ast
        .comments
        .iter()
        .any(|c| c.kind == CommentKind::Line && c.span.end as usize == s);
    if after_line_comment { at } else { s as u32 }
}

fn end_with_spaces(src: &str, at: u32) -> u32 {
    let b = src.as_bytes();
    let mut e = at as usize;
    while e < b.len() && b[e].is_ascii_whitespace() {
        e += 1;
    }
    e as u32
}

/// The first token at or after `at`, skipping whitespace and comments: its start.
fn next_token(ast: &Ast, src: &str, mut at: usize) -> usize {
    let b = src.as_bytes();
    loop {
        while at < b.len() && b[at].is_ascii_whitespace() {
            at += 1;
        }
        match ast.comments.iter().find(|c| c.span.start as usize == at) {
            Some(c) => at = c.span.end as usize,
            None => return at,
        }
    }
}

/// The last token ending at or before `at`: the position just after it.
fn prev_token_end(ast: &Ast, src: &str, mut at: usize) -> usize {
    let b = src.as_bytes();
    loop {
        while at > 0 && b[at - 1].is_ascii_whitespace() {
            at -= 1;
        }
        match ast.comments.iter().find(|c| c.span.end as usize == at) {
            Some(c) => at = c.span.start as usize,
            None => return at,
        }
    }
}

/// The first `op` token in `src[from..to]`, outside comments.
pub(crate) fn op_token(ast: &Ast, src: &str, from: u32, to: u32, op: &str) -> Option<Span> {
    let mut ci = ast.comments.partition_point(|c| c.span.start < from);
    let mut i = from as usize;
    while i < to as usize {
        if let Some(c) = ast.comments.get(ci)
            && c.span.start as usize <= i
        {
            i = i.max(c.span.end as usize);
            ci += 1;
            continue;
        }
        if src[i..].starts_with(op) {
            return Some(Span::new(i as u32, (i + op.len()) as u32));
        }
        i += src[i..].chars().next().map_or(1, char::len_utf8);
    }
    None
}

fn needs_parens(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let parenthesized = cx
        .parent(node)
        .is_some_and(|p| matches!(ast.node(p), Node::Paren { .. }));
    match parent_skip_parens(ast, cx.file.parents, node).map(|p| ast.node(p)) {
        Some(
            Node::VarDeclarator { .. }
            | Node::ArrayLit { .. }
            | Node::Return { .. }
            | Node::Call { .. }
            | Node::Property { .. }
            | Node::Assign { .. },
        ) => false,
        _ => !parenthesized,
    }
}

impl Rule for PreferObjectSpread {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        for node in global_member_calls(cx, "Object", "assign") {
            check(cx, node);
        }
    }
}

fn check(cx: &mut Ctx<'_, '_>, node: NodeId) {
    let ast = cx.ast();
    let src = cx.src();
    let Node::Call { callee, args, .. } = *cx.node(node) else {
        return;
    };
    let callee = skip_parens(ast, callee);
    let args = ast.list(args);
    let Some(&first) = args.first() else {
        return;
    };
    if object_props(ast, first).is_none()
        || args
            .iter()
            .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
    {
        return;
    }
    if args.len() > 1
        && (args
            .iter()
            .filter_map(|&a| object_props(ast, a))
            .any(|p| has_accessor(ast, p))
            || args[1..]
                .iter()
                .filter_map(|&a| object_props(ast, a))
                .any(|p| has_proto(ast, src, p)))
    {
        return;
    }
    let span = cx.span(node);
    let fix = op_token(ast, src, cx.span(callee).end, span.end, "(").map(|lp| {
        let rp = Span::new(span.end - 1, span.end);
        let mut fix = Fix::delete(Span::new(span.start, lp.start));
        if needs_parens(cx, node) {
            let semi = statement_at_start(cx, node).is_some_and(|stmt| {
                prev_statement(ast, cx.file.parents, stmt)
                    .is_some_and(|p| stmt_continuable(ast, src, p))
            });
            fix = fix.and(lp, if semi { ";({" } else { "({" }).and(rp, "})");
        } else {
            fix = fix.and(lp, "{").and(rp, "}");
        }
        for &arg in args {
            // The argument's own first and last tokens, inside its parentheses.
            let mut inner = arg;
            let mut parens = Vec::new();
            while let Node::Paren { expr } = *ast.node(inner) {
                parens.push(cx.span(inner));
                inner = expr;
            }
            let outer = cx.span(arg);
            let left = Span::new(outer.start, outer.start + 1);
            let right = Span::new(outer.end - 1, outer.end);
            if let Some(props) = object_props(ast, inner) {
                let own = cx.span(inner);
                let mut middle: Vec<Span> = parens
                    .iter()
                    .skip(1)
                    .flat_map(|p| [Span::new(p.start, p.start + 1), Span::new(p.end - 1, p.end)])
                    .collect();
                if !parens.is_empty() {
                    middle.push(Span::new(own.start, own.start + 1));
                    middle.push(Span::new(own.end - 1, own.end));
                }
                for m in middle {
                    fix = fix.and(m, "");
                }
                let left_end = end_with_spaces(src, left.end);
                fix = fix.and(Span::new(left.start, left_end), "");
                let right_start = start_with_spaces(ast, src, right.start).max(left_end);
                fix = fix.and(Span::new(right_start, right.end), "");
                let before_close = prev_token_end(ast, src, own.end as usize - 1);
                let trailing = props.is_empty()
                    || (before_close > 0 && src.as_bytes()[before_close - 1] == b',');
                let after = next_token(ast, src, right.end as usize);
                if trailing && src.as_bytes().get(after) == Some(&b',') {
                    let at = after as u32;
                    fix = fix.and(Span::new(at, at + 1), "");
                }
            } else if parens.is_empty()
                && matches!(
                    ast.node(inner),
                    Node::Assign { .. } | Node::Arrow { .. } | Node::Conditional { .. }
                )
            {
                fix = fix
                    .and(Span::at(left.start), "...(")
                    .and(Span::at(right.end), ")");
            } else {
                fix = fix.and(Span::at(left.start), "...");
            }
        }
        fix
    });
    let (msg, id) = if args.len() == 1 {
        (
            "Use an object literal instead of `Object.assign`. eg: `{ foo: bar }`.",
            "useLiteralMessage",
        )
    } else {
        (
            "Use an object spread instead of `Object.assign` eg: `{ ...foo }`.",
            "useSpreadMessage",
        )
    };
    cx.report_node(node, msg).id(id).fix_opt(fix);
}
