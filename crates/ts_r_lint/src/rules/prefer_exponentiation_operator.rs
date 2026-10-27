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

/// The calls `object.prop(..)` of a global `object` (also through `globalThis.object`,
/// parentheses and `?.`), as ESLint's `ReferenceTracker` finds them: none when the
/// global is assigned anywhere in the file. In source order.
pub(crate) fn global_member_calls(cx: &Ctx<'_, '_>, object: &str, prop: &str) -> Vec<NodeId> {
    let ast = cx.ast();
    let sem = cx.semantic();
    let up = |mut e: NodeId| {
        while let Some(p) = cx.parent(e)
            && matches!(ast.node(p), Node::Paren { .. })
        {
            e = p;
        }
        (e, cx.parent(e))
    };
    // `e`, as the object of a member access named `name`: that member.
    let member = |e: NodeId, name: &str| {
        let (e, p) = up(e);
        let p = p?;
        match *ast.node(p) {
            Node::Member { obj, .. } | Node::Index { obj, .. }
                if obj == e && prop_name(cx, p).as_deref() == Some(name) =>
            {
                Some(p)
            }
            _ => None,
        }
    };
    let mut roots = Vec::new();
    let mut written = [false, false];
    for r in sem.references() {
        let name = ast.atom(r.name);
        let i = if name == object {
            0
        } else if name == "globalThis" {
            1
        } else {
            continue;
        };
        if !cx.is_global_ref(r.node, name) {
            continue;
        }
        if r.is_write() {
            written[i] = true;
        } else {
            roots.push((i, r.node));
        }
    }
    if written[0] {
        return Vec::new();
    }
    let mut calls = Vec::new();
    for (i, mut e) in roots {
        if i == 1 {
            if written[1] || !sem.is_global(object) {
                continue;
            }
            let Some(m) = member(e, object) else {
                continue;
            };
            e = m;
        }
        let Some(m) = member(e, prop) else {
            continue;
        };
        let (m, p) = up(m);
        if let Some(c) = p
            && let Node::Call { callee, .. } = *ast.node(c)
            && callee == m
        {
            calls.push(c);
        }
    }
    calls.sort_by_key(|&c| cx.span(c).start);
    calls
}

pub(crate) fn prop_name(cx: &Ctx<'_, '_>, id: NodeId) -> Option<String> {
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
pub(crate) fn statement_at_start(cx: &Ctx<'_, '_>, node: NodeId) -> Option<NodeId> {
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

/// ESLint's `canTokensBeAdjacent`, negated: the token `left` ends with and the one `right`
/// starts with would join into other tokens (`a+ ++b`, `b in`, `/ /**/`).
pub(crate) fn joins(left: &str, right: &str) -> bool {
    let (Some(l), Some(r)) = (left.chars().next_back(), right.chars().next()) else {
        return false;
    };
    if is_word(l) && is_word(r) {
        return true;
    }
    if l == '+' || l == '-' {
        return r == l;
    }
    l == '/' && (r == '/' || r == '*')
}

impl Rule for PreferExponentiationOperator {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        for node in global_member_calls(cx, "Math", "pow") {
            self.check(cx, node);
        }
    }
}

impl PreferExponentiationOperator {
    fn check(&self, cx: &mut Ctx<'_, '_>, node: NodeId) {
        let ast = cx.ast();
        let Node::Call { args, .. } = *cx.node(node) else {
            return;
        };
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
