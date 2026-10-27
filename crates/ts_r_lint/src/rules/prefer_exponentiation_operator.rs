//! `prefer-exponentiation-operator`: `a ** b` instead of `Math.pow(a, b)`.

use super::prelude::*;
use super::unicorn_static::{Static, static_value};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "prefer-exponentiation-operator",
        "Disallow the use of `Math.pow` in favor of the `**` operator",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<PreferExponentiationOperator>,
};

#[derive(Debug, Default)]
struct PreferExponentiationOperator;

/// Our [`precedence`] of `**`.
const EXP: u8 = 14;

/// `Math`, `(Math)` or `globalThis.Math` (the global, not a local binding).
fn is_math(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let id = skip_parens(ast, id);
    if cx.is_global_ref(id, "Math") {
        return true;
    }
    match *ast.node(id) {
        Node::Member { obj, .. } | Node::Index { obj, .. } => {
            cx.is_global_ref(obj, "globalThis")
                && prop_name(cx, id).as_deref() == Some("Math")
                && cx.semantic().is_global("Math")
        }
        _ => false,
    }
}

fn prop_name(cx: &Ctx<'_, '_>, id: NodeId) -> Option<String> {
    let ast = cx.ast();
    match *ast.node(id) {
        Node::Member { prop, .. } => ident_name(ast, prop).map(str::to_owned),
        Node::Index { index, .. } => match static_value(cx, index)? {
            Static::Str(s) => Some(s),
            _ => None,
        },
        _ => None,
    }
}

/// The base or exponent's text needs parentheses around the whole `a**b`, as ESLint's
/// `doesExponentiationExpressionNeedParens`.
fn whole_needs_parens(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let Some(p) = cx.parent(node) else {
        return false;
    };
    match *ast.node(p) {
        Node::Binary {
            op: BinaryOp::Exp,
            left,
            ..
        } => left == node,
        Node::Unary { .. }
        | Node::Await { .. }
        | Node::ImportCall { .. }
        | Node::As { .. }
        | Node::Satisfies { .. }
        | Node::NonNull { .. }
        | Node::Instantiation { .. } => true,
        Node::Call { callee, .. } | Node::New { callee, .. } => callee == node,
        Node::Member { obj, .. } | Node::Index { obj, .. } => obj == node,
        Node::TaggedTemplate { tag, .. } => tag == node,
        Node::ExprWithTypeArgs { .. } => cx
            .parent(p)
            .and_then(|h| cx.parent(h))
            .is_some_and(|c| matches!(cx.node(c), Node::ClassDecl { .. } | Node::ClassExpr { .. })),
        _ => false,
    }
}

/// `node` begins an expression statement.
fn statement_at_start(cx: &Ctx<'_, '_>, node: NodeId) -> Option<NodeId> {
    let start = cx.span(node).start;
    let mut cur = node;
    for _ in 0..10_000 {
        let p = cx.parent(cur)?;
        if cx.span(p).start != start {
            return None;
        }
        if matches!(cx.node(p), Node::ExprStmt { .. }) {
            return Some(p);
        }
        cur = p;
    }
    None
}

fn is_word(c: char) -> bool {
    c == '$' || c == '\\' || ts_r_lexer::is_id_part(c)
}

/// The text `left` ends with and `right` starts with would join into other tokens
/// (`a+ ++b`, `b in`).
fn joins(left: &str, right: &str) -> bool {
    let (Some(l), Some(r)) = (left.chars().next_back(), right.chars().next()) else {
        return false;
    };
    if is_word(l) && is_word(r) {
        return true;
    }
    if (l == '+' || l == '-') && r == l {
        // `++` / `--` followed by one more sign still reads as written.
        let mut it = left.chars().rev();
        it.next();
        return it.next() != Some(l);
    }
    l == '/' && (r == '/' || r == '*')
}

impl Rule for PreferExponentiationOperator {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Call { callee, args, .. } = *cx.node(node) else {
            return;
        };
        let callee = skip_parens(ast, callee);
        let (Node::Member { obj, .. } | Node::Index { obj, .. }) = *ast.node(callee) else {
            return;
        };
        if prop_name(cx, callee).as_deref() != Some("pow") || !is_math(cx, obj) {
            return;
        }
        let args = ast.list(args);
        let span = cx.span(node);
        let fix = (args.len() == 2
            && !args
                .iter()
                .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
            && ast.comments_in(span).is_empty())
        .then(|| self.fix(cx, node, args[0], args[1]));
        cx.report_node(node, "Use the '**' operator instead of 'Math.pow'.")
            .id("useExponentiation")
            .fix_opt(fix);
    }
}

impl PreferExponentiationOperator {
    fn fix(&self, cx: &Ctx<'_, '_>, node: NodeId, base: NodeId, exp: NodeId) -> Fix {
        let ast = cx.ast();
        let src = cx.src();
        let base = skip_parens(ast, base);
        let exp = skip_parens(ast, exp);
        let paren_base = precedence(ast, base) <= EXP
            || matches!(ast.node(base), Node::Await { .. } | Node::Unary { .. });
        let paren_exp = precedence(ast, exp) < EXP;
        let stmt = statement_at_start(cx, node);
        let mut paren_all = whole_needs_parens(cx, node);
        if !paren_all && !paren_base && stmt.is_some() {
            let first = leftmost_expr(ast, base);
            paren_all = matches!(
                ast.node(first),
                Node::ObjectLit { .. } | Node::FunctionExpr { .. } | Node::ClassExpr { .. }
            );
        }
        let span = cx.span(node);
        let wrap = |t: &str, w: bool| if w { format!("({t})") } else { t.to_owned() };
        let base_text = wrap(cx.text(base), paren_base);
        let exp_text = wrap(cx.text(exp), paren_exp);
        let mut prefix = "";
        let mut suffix = "";
        if !paren_all {
            let s = span.start as usize;
            let e = span.end as usize;
            let comment_at = |at: u32, end: bool| {
                ast.comments.iter().any(|c| {
                    if end {
                        c.span.end == at
                    } else {
                        c.span.start == at
                    }
                })
            };
            if !paren_base && !comment_at(span.start, true) && joins(&src[..s], &base_text) {
                prefix = " ";
            }
            if !paren_exp && !comment_at(span.end, false) && joins(&exp_text, &src[e..]) {
                suffix = " ";
            }
        }
        let text = wrap(&format!("{base_text}**{exp_text}"), paren_all);
        if prefix.is_empty()
            && let Some(stmt) = stmt
            && text.starts_with(['(', '[', '/', '`'])
            && prev_statement(ast, cx.file.parents, stmt)
                .is_some_and(|p| stmt_continuable(ast, src, p))
        {
            prefix = ";";
        }
        Fix::replace(span, format!("{prefix}{text}{suffix}"))
    }
}
