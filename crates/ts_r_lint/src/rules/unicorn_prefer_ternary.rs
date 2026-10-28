//! `unicorn/prefer-ternary`: an `if`/`else` whose branches (each a single statement, empty
//! statements ignored) are `return a`/`return b` or `x = a`/`x = b` becomes
//! `return test ? a : b` / `x = test ? a : b` (nested merges like `return x = …` too). An
//! `if (test) return a;` followed by `return b;` counts as an `if`/`else`, unless the
//! statement before is another `if` returning without `else`. A `let x = a;` followed by
//! `if (test) x = b;` gets a suggestion to declare `const x = test ? b : a` (`let` when `x`
//! is written elsewhere).
//!
//! Not for `else if` branches, tests or branches holding a ternary (functions and classes
//! excluded), blocks, classes or multi-line object/array/JSX/template literals in the
//! parts, bare `return;`, two boolean returns, compound assignments, arrow values assigned
//! to a plain name, and (with `"only-single-line"`) multi-line parts. A comment in the
//! replaced range removes the fix. Branches that only differ in one inner value (the
//! plugin's "minimal ternary" forms such as `foo(test ? a : b)`) are not handled here.

use super::prelude::*;
use super::unicorn_text::paren_span;
use super::unicorn_text::{is_parenthesized, needs_semicolon, paren_text};
use super::unicorn_util::is_same_reference;
use super::unicorn_util::skip_wrappers;
use super::unicorn_util::{has_side_effect_with, trailing_comment_same_line};
use crate::semantic::RefFlags;
use ts_r_ast::{AssignOp, Ast, VarKind};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-ternary",
        "Prefer ternary expressions over simple `if` statements.",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<PreferTernary>,
};

const MESSAGE: &str = "This `if` statement can be replaced by a ternary expression.";

#[derive(Debug)]
struct PreferTernary {
    only_single_line: bool,
}

impl FromOptions for PreferTernary {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let only_single_line = match opts.first() {
            None => false,
            Some(v) => match v.as_str() {
                Some("always") => false,
                Some("only-single-line") => true,
                _ => return Err("expected \"always\" or \"only-single-line\"".into()),
            },
        };
        Ok(PreferTernary { only_single_line })
    }
}

fn is_function_or_class(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(id),
        Node::FunctionDecl { .. }
            | Node::FunctionExpr { .. }
            | Node::Arrow { .. }
            | Node::ClassDecl { .. }
            | Node::ClassExpr { .. }
    )
}

/// A conditional expression in `id`, not looking into functions or classes.
fn has_ternary(ast: &Ast, id: NodeId) -> bool {
    let mut stack = vec![id];
    while let Some(n) = stack.pop() {
        if matches!(ast.node(n), Node::Conditional { .. }) {
            return true;
        }
        if is_function_or_class(ast, n) {
            continue;
        }
        ast.for_each_child(n, |c| stack.push(c));
    }
    false
}

/// A block or class anywhere in `id`, or a multi-line object, array, JSX or template
/// literal.
fn has_complex_structure(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let lines = cx.file.lines;
    let mut stack = vec![id];
    while let Some(n) = stack.pop() {
        match ast.node(n) {
            Node::Block { .. } | Node::ClassDecl { .. } | Node::ClassExpr { .. } => return true,
            Node::ObjectLit { .. }
            | Node::ArrayLit { .. }
            | Node::ObjectPattern { .. }
            | Node::ArrayPattern { .. }
            | Node::JsxElement { .. }
            | Node::JsxFragment { .. }
            | Node::TemplateLit { .. } => {
                let s = cx.span(n);
                if lines.line_of(s.start) != lines.line_of(s.end) {
                    return true;
                }
            }
            _ => {}
        }
        ast.for_each_child(n, |c| stack.push(c));
    }
    false
}

/// `getNodeBody`: an expression statement's expression, or the only non-empty statement
/// of a block (recursively).
fn node_body(ast: &Ast, id: NodeId, depth: u32) -> NodeId {
    if depth > 64 {
        return id;
    }
    match *ast.node(id) {
        Node::ExprStmt { expr } => skip_parens(ast, expr),
        Node::Block { stmts } => {
            let body: Vec<NodeId> = ast
                .list(stmts)
                .iter()
                .copied()
                .filter(|&s| !matches!(ast.node(s), Node::Empty {}))
                .collect();
            match *body {
                [only] => node_body(ast, only, depth + 1),
                _ => id,
            }
        }
        _ => id,
    }
}

fn is_single_line(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let s = cx.span(id);
    let lines = cx.file.lines;
    lines.line_of(s.start) == lines.line_of(s.end)
}

fn is_arrow(ast: &Ast, id: NodeId) -> bool {
    matches!(ast.node(skip_wrappers(ast, id)), Node::Arrow { .. })
}

fn is_bool_literal(ast: &Ast, id: NodeId) -> bool {
    matches!(ast.node(id), Node::BoolLit { .. })
}

fn sibling(cx: &Ctx<'_, '_>, node: NodeId, offset: isize) -> Option<NodeId> {
    let list = statement_list(cx.ast(), cx.parent(node)?)?;
    let i = list.iter().position(|&s| s == node)?;
    list.get(i.checked_add_signed(offset)?).copied()
}

/// Whitespace-insensitive token comparison of two patterns (comments ignored).
fn same_tokens(cx: &Ctx<'_, '_>, a: NodeId, b: NodeId) -> bool {
    let squash = |id: NodeId| -> String {
        let span = cx.span(id);
        let src = cx.src();
        let mut out = String::new();
        let mut pos = span.start as usize;
        for c in cx.ast().comments_in(span) {
            out.push_str(&src[pos..c.span.start as usize]);
            out.push(' ');
            pos = c.span.end as usize;
        }
        out.push_str(&src[pos..span.end as usize]);
        out.split_whitespace().collect::<Vec<_>>().join(" ")
    };
    squash(a) == squash(b)
}

struct Merged {
    before: String,
    consequent: NodeId,
    alternate: NodeId,
}

impl PreferTernary {
    /// `getText`: the branch text, parenthesized when it binds looser than `?:` allows.
    fn text(cx: &Ctx<'_, '_>, id: NodeId) -> String {
        let ast = cx.ast();
        let text = paren_text(cx, id).to_owned();
        let parenthesized = is_parenthesized(cx, id);
        let needs = matches!(
            ast.node(id),
            Node::Await { .. }
                | Node::Assign { .. }
                | Node::Yield { .. }
                | Node::Seq { .. }
                | Node::As { .. }
                | Node::TypeAssertion { .. }
        );
        let text = if !parenthesized && needs {
            format!("({text})")
        } else {
            text
        };
        if matches!(ast.node(id), Node::Arrow { .. })
            && !parenthesized
            && cx
                .parent(id)
                .is_some_and(|p| matches!(cx.node(p), Node::If { .. }))
        {
            return format!("({text})");
        }
        text
    }

    fn merge(cx: &Ctx<'_, '_>, m: Merged, strict: bool, depth: u32) -> Option<Merged> {
        let ast = cx.ast();
        let (c, a) = (m.consequent, m.alternate);
        if depth > 64 || std::mem::discriminant(ast.node(c)) != std::mem::discriminant(ast.node(a))
        {
            return (!strict).then_some(m);
        }
        if let (Node::Return { arg: Some(ca) }, Node::Return { arg: Some(aa) }) =
            (*ast.node(c), *ast.node(a))
        {
            let (ca, aa) = (skip_parens(ast, ca), skip_parens(ast, aa));
            let both_bool = is_bool_literal(ast, ca) && is_bool_literal(ast, aa);
            if !has_ternary(ast, ca) && !has_ternary(ast, aa) && !both_bool {
                return Self::merge(
                    cx,
                    Merged {
                        before: format!("{}return ", m.before),
                        consequent: ca,
                        alternate: aa,
                    },
                    false,
                    depth + 1,
                );
            }
        }
        if let (
            Node::Assign {
                left: cl,
                op: AssignOp::Assign,
                right: cr,
            },
            Node::Assign {
                left: al,
                op: AssignOp::Assign,
                right: ar,
            },
        ) = (*ast.node(c), *ast.node(a))
        {
            let (cr, ar) = (skip_parens(ast, cr), skip_parens(ast, ar));
            let (cls, als) = (skip_parens(ast, cl), skip_parens(ast, al));
            let is_pattern = |n: NodeId| {
                matches!(
                    ast.node(n),
                    Node::ArrayPattern { .. }
                        | Node::ObjectPattern { .. }
                        | Node::ArrayLit { .. }
                        | Node::ObjectLit { .. }
                )
            };
            let mergeable = !has_ternary(ast, cr)
                && !has_ternary(ast, ar)
                && ((matches!(ast.node(skip_wrappers(ast, cls)), Node::Ident { .. })
                    && !is_arrow(ast, cr)
                    && !is_arrow(ast, ar)
                    && is_same_reference(cx, cls, als))
                    || (is_pattern(cls)
                        && std::mem::discriminant(ast.node(cls))
                            == std::mem::discriminant(ast.node(als))
                        && !has_complex_structure(cx, cls)
                        && !has_complex_structure(cx, als)
                        && same_tokens(cx, cls, als)));
            if mergeable {
                let result = Merged {
                    before: format!("{}{} = ", m.before, paren_text(cx, cls)),
                    consequent: cr,
                    alternate: ar,
                };
                if is_pattern(cls) {
                    return Some(result);
                }
                return Self::merge(cx, result, false, depth + 1);
            }
        }
        (!strict).then_some(m)
    }

    fn branches_problem(&self, cx: &mut Ctx<'_, '_>, node: NodeId, alternate_node: NodeId) -> bool {
        let ast = cx.ast();
        let Node::If { test, cons, alt } = *ast.node(node) else {
            return false;
        };
        let is_else_if = cx
            .parent(node)
            .is_some_and(|p| matches!(*cx.node(p), Node::If { alt: Some(a), .. } if a == node));
        let test_inner = skip_parens(ast, test);
        if is_else_if || has_ternary(ast, test_inner) {
            return false;
        }
        let consequent = node_body(ast, cons, 0);
        let alternate = node_body(ast, alternate_node, 0);
        if self.only_single_line
            && [consequent, alternate, test_inner]
                .iter()
                .any(|&n| !is_single_line(cx, n))
        {
            return false;
        }
        let Some(result) = Self::merge(
            cx,
            Merged {
                before: String::new(),
                consequent,
                alternate,
            },
            true,
            0,
        ) else {
            return false;
        };
        if [test_inner, result.consequent, result.alternate]
            .iter()
            .any(|&n| has_complex_structure(cx, n))
        {
            return false;
        }
        let flat = alt.is_none();
        let node_span = cx.span(node);
        let range = if flat {
            Span::new(node_span.start, cx.span(alternate_node).end)
        } else {
            node_span
        };
        let comment_range = match flat
            .then(|| trailing_comment_same_line(cx, cx.span(alternate_node).end))
            .flatten()
        {
            Some(c) => Span::new(range.start, c.end),
            None => range,
        };
        let has_comment = cx
            .ast()
            .comments_in(comment_range)
            .iter()
            .any(|c| c.span.start >= comment_range.start && c.span.end <= comment_range.end);
        let mut fixed = format!(
            "{}{} ? {} : {}",
            result.before,
            Self::text(cx, test_inner),
            Self::text(cx, result.consequent),
            Self::text(cx, result.alternate)
        );
        if let Node::Assign { left, .. } = *ast.node(consequent)
            && matches!(
                ast.node(skip_parens(ast, left)),
                Node::ObjectPattern { .. } | Node::ObjectLit { .. }
            )
        {
            fixed = format!("({fixed})");
        }
        fixed.push(';');
        if needs_semicolon(cx, node_span.start, &fixed) {
            fixed.insert(0, ';');
        }
        let fix = (!has_comment).then(|| Fix::replace(range, fixed));
        cx.report(node_span, MESSAGE)
            .id("prefer-ternary")
            .fix_opt(fix);
        true
    }

    fn let_plus_if_problem(&self, cx: &mut Ctx<'_, '_>, node: NodeId) {
        let ast = cx.ast();
        let sem = cx.semantic();
        let src = cx.src();
        let Node::If { test, cons, .. } = *ast.node(node) else {
            return;
        };
        let Node::Assign {
            left,
            op: AssignOp::Assign,
            right,
        } = *ast.node(node_body(ast, cons, 0))
        else {
            return;
        };
        let (left, right) = (skip_parens(ast, left), skip_parens(ast, right));
        let Some(name) = ident_name(ast, left) else {
            return;
        };
        let Some(prev) = sibling(cx, node, -1) else {
            return;
        };
        let Node::VarDecl {
            kind: VarKind::Let,
            decls,
            ..
        } = *ast.node(prev)
        else {
            return;
        };
        let &[declarator] = ast.list(decls) else {
            return;
        };
        let Node::VarDeclarator {
            name: id,
            init: Some(init),
            ..
        } = *ast.node(declarator)
        else {
            return;
        };
        if ident_name(ast, id) != Some(name) {
            return;
        }
        let init_inner = skip_parens(ast, init);
        let test = skip_parens(ast, test);
        if is_arrow(ast, init_inner) || is_arrow(ast, right) {
            return;
        }
        if [test, right, init_inner].iter().any(|&e| {
            has_ternary(ast, e)
                || has_complex_structure(cx, e)
                || (self.only_single_line && !is_single_line(cx, e))
        }) {
            return;
        }
        if has_side_effect_with(ast, init_inner, false, false)
            || has_side_effect_with(ast, test, false, false)
        {
            return;
        }
        let Some(sym) = sem
            .reference_at(left)
            .and_then(|r| sem.reference(r).symbol())
        else {
            return;
        };
        let inside = |r: NodeId, outer: NodeId| {
            let (a, b) = (cx.span(r), cx.span(outer));
            a.start >= b.start && a.end <= b.end
        };
        let refs: Vec<_> = sem
            .symbol(sym)
            .refs
            .iter()
            .map(|&r| *sem.reference(r))
            .collect();
        if refs
            .iter()
            .any(|r| inside(r.node, test) || inside(r.node, right))
        {
            return;
        }
        let prev_span = cx.span(prev);
        let node_span = cx.span(node);
        let comment_range = Span::new(prev_span.start, node_span.end);
        let has_comment = ast
            .comments_in(comment_range)
            .iter()
            .any(|c| c.span.start >= comment_range.start && c.span.end <= comment_range.end);
        let other_writes = refs
            .iter()
            .any(|r| r.is_write() && !r.flags.contains(RefFlags::INIT) && !inside(r.node, node));
        let ternary = format!(
            "{} ? {} : {}",
            Self::text(cx, test),
            Self::text(cx, right),
            Self::text(cx, init_inner)
        );
        let next = skip_trivia_forward(src, node_span.end as usize);
        let semicolon = next < src.len() && needs_semicolon(cx, prev_span.end, &src[next..]);
        let fix = Fix {
            edits: vec![
                crate::diagnostic::Edit {
                    span: Span::new(prev_span.start, prev_span.start + 3),
                    text: if other_writes { "let" } else { "const" }.into(),
                },
                crate::diagnostic::Edit {
                    span: paren_span(cx, init_inner),
                    text: ternary,
                },
                crate::diagnostic::Edit {
                    span: Span::new(prev_span.end, node_span.end),
                    text: if semicolon { ";" } else { "" }.into(),
                },
            ],
        };
        let report = cx.report(node_span, MESSAGE).id("prefer-ternary");
        if !has_comment {
            report.suggest(
                "prefer-ternary/suggestion",
                "Use a ternary expression.",
                fix,
            );
        }
    }
}

impl Rule for PreferTernary {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::If]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::If { alt, .. } = *ast.node(node) else {
            return;
        };
        if let Some(alt) = alt {
            self.branches_problem(cx, node, alt);
            return;
        }
        if let Some(prev) = sibling(cx, node, -1)
            && let Node::If {
                cons: prev_cons,
                alt: None,
                ..
            } = *ast.node(prev)
            && matches!(ast.node(node_body(ast, prev_cons, 0)), Node::Return { .. })
        {
            return;
        }
        if let Some(next) = sibling(cx, node, 1)
            && matches!(ast.node(next), Node::Return { .. })
            && self.branches_problem(cx, node, next)
        {
            return;
        }
        self.let_plus_if_problem(cx, node);
    }
}
