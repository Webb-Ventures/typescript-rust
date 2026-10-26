//! `unicorn/prefer-date-now`: `Date.now()` over turning a fresh `new Date()` into a number.
//!
//! `new Date()` here means `new Date` with no arguments and the callee written as `Date`.
//! Reported:
//! - `new Date().getTime()` / `.valueOf()` (no arguments, no optional chaining, not computed),
//!   at the method name; the fix replaces the whole call;
//! - `Number(new Date())` (reported and replaced whole) and `BigInt(new Date())` (only the
//!   argument);
//! - `+new Date()` (the whole unary; the fix adds a space next to an adjacent keyword, as in
//!   `return+new Date`) and `-new Date()` (only the operand);
//! - an operand of `-`, `*`, `/`, `%`, `**`, and the right side of `-=`, `*=`, `/=`, `%=`,
//!   `**=`.
//!
//! The fix writes `Date.now()` and is withheld when a comment sits inside the replaced node.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-date-now",
        "Prefer `Date.now()` to get the number of milliseconds since the Unix Epoch.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<PreferDateNow>,
};

#[derive(Debug, Default)]
struct PreferDateNow;

const DEFAULT_MESSAGE: &str = "Prefer `Date.now()` over `new Date()`.";

impl Rule for PreferDateNow {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::Call,
            NodeKind::Unary,
            NodeKind::Assign,
            NodeKind::Binary,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        match *cx.node(node) {
            Node::Call {
                callee,
                optional: false,
                args,
                ..
            } => {
                let args = ast.list(args);
                let callee = skip_parens(ast, callee);
                match *ast.node(callee) {
                    Node::Member {
                        obj,
                        optional: false,
                        prop,
                    } if args.is_empty() => {
                        let Some(method @ ("getTime" | "valueOf")) = ident_name(ast, prop) else {
                            return;
                        };
                        if !is_new_date(ast, obj) {
                            return;
                        }
                        let fix = date_now_fix(cx, node, false);
                        cx.report(
                            cx.span(prop),
                            format!("Prefer `Date.now()` over `Date#{method}()`."),
                        )
                        .id("prefer-date-now-over-methods")
                        .fix_opt(fix);
                    }
                    Node::Ident { .. } => {
                        let [arg] = args else {
                            return;
                        };
                        if !is_new_date(ast, *arg) {
                            return;
                        }
                        match ident_name(ast, callee) {
                            Some("Number") => {
                                let fix = date_now_fix(cx, node, false);
                                cx.report_node(
                                    node,
                                    "Prefer `Date.now()` over `Number(new Date())`.",
                                )
                                .id("prefer-date-now-over-number-data-object")
                                .fix_opt(fix);
                            }
                            Some("BigInt") => report_default(cx, skip_parens(ast, *arg), false),
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
            Node::Unary { op, arg } => {
                if !is_new_date(ast, arg) {
                    return;
                }
                match op {
                    UnaryOp::Plus => report_default(cx, node, true),
                    UnaryOp::Minus => report_default(cx, skip_parens(ast, arg), false),
                    _ => {}
                }
            }
            Node::Assign { op, right, .. } => {
                if matches!(
                    op,
                    AssignOp::SubAssign
                        | AssignOp::MulAssign
                        | AssignOp::DivAssign
                        | AssignOp::ModAssign
                        | AssignOp::ExpAssign
                ) && is_new_date(ast, right)
                {
                    report_default(cx, skip_parens(ast, right), false);
                }
            }
            Node::Binary { left, op, right } => {
                if !matches!(
                    op,
                    BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod | BinaryOp::Exp
                ) {
                    return;
                }
                for side in [left, right] {
                    if is_new_date(ast, side) {
                        report_default(cx, skip_parens(ast, side), false);
                    }
                }
            }
            _ => {}
        }
    }
}

fn report_default(cx: &mut Ctx<'_, '_>, node: NodeId, unary: bool) {
    let fix = date_now_fix(cx, node, unary);
    cx.report_node(node, DEFAULT_MESSAGE)
        .id("prefer-date")
        .fix_opt(fix);
}

/// `new Date` / `new Date()`, through parentheses.
fn is_new_date(ast: &ts_r_ast::Ast, id: NodeId) -> bool {
    match *ast.node(skip_parens(ast, id)) {
        Node::New { callee, args, .. } => {
            ast.list(args).is_empty() && ident_name(ast, callee) == Some("Date")
        }
        _ => false,
    }
}

/// Replaces `node` with `Date.now()`; for a unary `+`, also separates the result from a
/// keyword written right next to it (`return+new Date` becomes `return Date.now()`).
fn date_now_fix(cx: &Ctx<'_, '_>, node: NodeId, unary: bool) -> Option<Fix> {
    let ast = cx.ast();
    let span = cx.span(node);
    if !ast.comments_in(span).is_empty() {
        return None;
    }
    let mut fix = Fix::replace(span, "Date.now()");
    if unary {
        for (at, text) in keyword_spacing(cx, node) {
            fix = fix.and(Span::at(at), text);
        }
    }
    Some(fix)
}

/// Keywords and the contextual words (`of`, `await`, `as`, `satisfies`) that would run
/// into an adjacent replacement.
fn is_spacing_keyword(word: &str) -> bool {
    matches!(
        word,
        "of" | "await"
            | "as"
            | "satisfies"
            | "break"
            | "case"
            | "catch"
            | "class"
            | "const"
            | "continue"
            | "debugger"
            | "default"
            | "delete"
            | "do"
            | "else"
            | "export"
            | "extends"
            | "finally"
            | "for"
            | "function"
            | "if"
            | "import"
            | "in"
            | "instanceof"
            | "new"
            | "return"
            | "super"
            | "switch"
            | "this"
            | "throw"
            | "try"
            | "typeof"
            | "var"
            | "void"
            | "while"
            | "with"
            | "yield"
            | "let"
            | "static"
            | "null"
            | "true"
            | "false"
    )
}

pub(crate) fn keyword_ending_at(src: &str, end: usize) -> bool {
    let b = src.as_bytes();
    let start = b[..end]
        .iter()
        .rposition(|c| !(c.is_ascii_alphanumeric() || *c == b'_' || *c == b'$'))
        .map_or(0, |p| p + 1);
    start < end && is_spacing_keyword(&src[start..end])
}

pub(crate) fn keyword_starting_at(src: &str, start: usize) -> bool {
    let b = src.as_bytes();
    let end = b[start..]
        .iter()
        .position(|c| !(c.is_ascii_alphanumeric() || *c == b'_' || *c == b'$'))
        .map_or(b.len(), |p| start + p);
    start < end && is_spacing_keyword(&src[start..end])
}

/// Spaces to insert so that replacing `node` (with its parentheses) keeps it apart from a
/// keyword written right next to it: `(position, " ")` before and/or after.
pub(crate) fn keyword_spacing(cx: &Ctx<'_, '_>, node: NodeId) -> Vec<(u32, &'static str)> {
    let mut outer = node;
    while let Some(p) = cx.parent(outer)
        && matches!(cx.node(p), Node::Paren { .. })
    {
        outer = p;
    }
    let range = cx.span(outer);
    let src = cx.src();
    let mut out = Vec::new();
    if keyword_ending_at(src, range.start as usize) {
        out.push((range.start, " "));
    }
    if keyword_starting_at(src, range.end as usize) {
        out.push((range.end, " "));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{keyword_ending_at, keyword_starting_at};

    #[test]
    fn adjacent_keywords() {
        assert!(keyword_ending_at("return+x", 6));
        assert!(!keyword_ending_at("foo+x", 3));
        assert!(!keyword_ending_at("(+x", 1));
        assert!(!keyword_ending_at("", 0));
        assert!(keyword_starting_at("x)in y", 2));
        assert!(!keyword_starting_at("x)inside", 2));
        assert!(!keyword_starting_at("x", 1));
    }
}
