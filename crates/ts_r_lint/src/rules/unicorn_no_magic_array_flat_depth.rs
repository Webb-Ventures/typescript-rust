//! `unicorn/no-magic-array-flat-depth`: the depth of `foo.flat(depth)` (a plain method call
//! with one argument) may not be a number other than `1` written inline (a number literal,
//! possibly under `+`/`-` and TypeScript wrappers). A comment inside the argument list
//! explains it and silences the report; receivers known not to be arrays are skipped.

use super::prelude::*;
use super::unicorn_ast::{call_tokens, method_call};
use super::unicorn_static::skip_ts_only;
use super::unicorn_types::should_skip_known_non_array_receiver;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-magic-array-flat-depth",
        "Disallow a magic number as the `depth` argument in `Array#flat(…).`",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<NoMagicArrayFlatDepth>,
};

#[derive(Debug, Default)]
struct NoMagicArrayFlatDepth;

fn static_number(ast: &Ast, id: NodeId, depth: u32) -> Option<f64> {
    if depth > 200 {
        return None;
    }
    match *ast.node(skip_ts_only(ast, id)) {
        Node::NumberLit { value } => Some(value.get()),
        Node::Unary {
            op: UnaryOp::Minus,
            arg,
        } => static_number(ast, arg, depth + 1).map(|v| -v),
        Node::Unary {
            op: UnaryOp::Plus,
            arg,
        } => static_number(ast, arg, depth + 1),
        _ => None,
    }
}

impl Rule for NoMagicArrayFlatDepth {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some(call) = method_call(ast, node) else {
            return;
        };
        if call.name != "flat" || call.optional_call {
            return;
        }
        let [depth] = *call.args else {
            return;
        };
        match static_number(ast, depth, 0) {
            Some(v) if v != 1.0 => {}
            _ => return,
        }
        let Some((open, close, _)) = call_tokens(cx, node) else {
            return;
        };
        let commented = ast
            .comments_in(Span::new(open + 1, close))
            .iter()
            .any(|c| c.span.end <= close && !is_directive_comment(c, cx.src()));
        if commented || should_skip_known_non_array_receiver(cx, call.obj) {
            return;
        }
        cx.report(cx.span(depth), "Magic number as depth is not allowed.")
            .id("no-magic-array-flat-depth");
    }
}

/// An ESLint disable or enable directive, which ESLint itself acts on rather than a
/// comment explaining the depth.
fn is_directive_comment(c: &ts_r_ast::Comment, src: &str) -> bool {
    let text = c.span.text(src);
    let (body, line) = match c.kind {
        ts_r_ast::CommentKind::Line => (text.get(2..).unwrap_or(""), true),
        ts_r_ast::CommentKind::Block => (
            text.get(2..text.len().saturating_sub(2)).unwrap_or(""),
            false,
        ),
        ts_r_ast::CommentKind::Hashbang => return false,
    };
    let word = body.split_whitespace().next().unwrap_or("");
    match word {
        "eslint-disable-line" | "eslint-disable-next-line" => true,
        "eslint-disable" | "eslint-enable" => !line,
        _ => false,
    }
}
