//! `unicorn/prefer-hoisting-branch-code`: statements every branch of a complete
//! `if`/`else if`/`else` chain starts (or ends) with belong before (or after) the `if`.
//!
//! Statements compare by tokens and tree shape (comments, layout and a final `;` aside).
//! Each branch is a block and keeps at least one statement of its own. A moved statement
//! must not be a block-scoped declaration (or a block holding one) or contain a multi-line
//! token, the `if` must sit in a statement list, no comment may be dropped or detached, and
//! the shared code must not use a branch's own `let`/`const`/class/function (nor cross a
//! `using` when it trails). Trailing code moves as a fix; leading code is a fix only for
//! side-effect-free expression statements after side-effect-free tests, else a suggestion.

use super::prelude::*;
use super::unicorn_ast::linebreak;
use super::unicorn_conditions::{has_multiline_token, is_block_scoped_declaration, reindent_text};
use super::unicorn_static::has_side_effect;
use super::unicorn_text::{needs_semicolon, would_remove_comments};
use crate::ast_util::same_tokens;
use ts_r_ast::Ast;
use ts_r_ast::visit::{Visitor, Walk, walk};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-hoisting-branch-code",
        "Prefer moving code shared by all branches of an `if` statement out of the branches.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create_default::<PreferHoistingBranchCode>,
};

#[derive(Debug, Default)]
struct PreferHoistingBranchCode;

/// `isSameStatement`: same tokens and shape; a bare `;` never matches.
fn same_statement(cx: &Ctx<'_, '_>, a: NodeId, b: NodeId) -> bool {
    !matches!(cx.node(a), Node::Empty {}) && same_tokens(cx.ast(), cx.src(), a, b)
}

fn has_tagged_template(ast: &Ast, root: NodeId) -> bool {
    struct Find {
        found: bool,
    }
    impl Visitor for Find {
        fn enter(&mut self, ast: &Ast, id: NodeId) -> Walk {
            if matches!(ast.node(id), Node::TaggedTemplate { .. }) {
                self.found = true;
                return Walk::Stop;
            }
            Walk::Continue
        }
    }
    let mut f = Find { found: false };
    walk(ast, root, &mut f);
    f.found
}

fn has_effect(ast: &Ast, id: NodeId) -> bool {
    has_side_effect(ast, id) || has_tagged_template(ast, id)
}

/// The whitespace right before `pos` on its line (`getIndentString`).
fn indent_before(src: &str, pos: u32) -> &str {
    let before = &src[..pos as usize];
    let trimmed = before.trim_end_matches(|c: char| c.is_whitespace() && c != '\n' && c != '\r');
    &before[trimmed.len()..]
}

fn block_statements(ast: &Ast, block: NodeId) -> &[NodeId] {
    match *ast.node(block) {
        Node::Block { stmts } => ast.list(stmts),
        _ => &[],
    }
}

/// Which side of the branches is moved, and how many statements each side shares.
#[derive(Clone, Copy)]
struct Shared {
    leading: usize,
    trailing: usize,
    start: bool,
}

impl Shared {
    /// The shared statements of `body` on this side.
    fn of<'a>(&self, body: &'a [NodeId]) -> &'a [NodeId] {
        if self.start {
            &body[..self.leading]
        } else {
            &body[body.len() - self.trailing..]
        }
    }

    /// The range removed from `body`, through the gap up to the code that stays.
    fn removal(&self, cx: &Ctx<'_, '_>, body: &[NodeId]) -> Span {
        if self.start {
            Span::new(cx.span(body[0]).start, cx.span(body[self.leading]).start)
        } else {
            Span::new(
                cx.span(body[body.len() - self.trailing - 1]).end,
                cx.span(body[body.len() - 1]).end,
            )
        }
    }
}

/// A comment the fix would drop or detach.
fn has_comment_hazard(cx: &Ctx<'_, '_>, if_stmt: NodeId, blocks: &[NodeId], s: Shared) -> bool {
    let ast = cx.ast();
    let src = cx.src();
    let span = cx.span(if_stmt);
    let outside = if s.start {
        let gap = skip_trivia_backward(ast, src, span.start as usize) as u32;
        would_remove_comments(cx, Span::new(gap, span.start), &[])
    } else {
        let next = skip_trivia_forward(src, span.end as usize) as u32;
        ast.comments_in(Span::new(span.end, next))
            .first()
            .is_some_and(|c| !src[span.end as usize..c.span.start as usize].contains(['\n', '\r']))
    };
    outside
        || blocks.iter().any(|&block| {
            let body = block_statements(ast, block);
            let block_span = cx.span(block);
            let edge = if s.start {
                Span::new(block_span.start + 1, cx.span(body[0]).start)
            } else {
                Span::new(cx.span(body[body.len() - 1]).end, block_span.end - 1)
            };
            would_remove_comments(cx, edge, &[])
                || would_remove_comments(cx, s.removal(cx, body), &[])
        })
}

/// The shared code uses a declaration of the branch made outside it, or trailing code
/// would move past a `using` declaration's disposal.
fn has_branch_local_hazard(cx: &Ctx<'_, '_>, block: NodeId, s: Shared) -> bool {
    let sem = cx.semantic();
    let ast = cx.ast();
    let shared = s.of(block_statements(ast, block));
    let (Some(&first), Some(&last)) = (shared.first(), shared.last()) else {
        return false;
    };
    let (lo, hi) = (cx.span(first).start, cx.span(last).end);
    let in_shared = |id: NodeId| (lo..=hi).contains(&cx.span(id).start);
    let Some(scope) = sem.scope_of_node(block) else {
        return false;
    };
    sem.scope(scope).symbols.iter().any(|&sym| {
        let symbol = sem.symbol(sym);
        let Some(decl) = symbol.decls.first() else {
            return false;
        };
        if in_shared(decl.name) {
            return false;
        }
        let trailing_resource = !s.start
            && matches!(
                ast.node(decl.parent),
                Node::VarDecl {
                    kind: VarKind::Using | VarKind::AwaitUsing,
                    ..
                }
            );
        trailing_resource
            || symbol
                .refs
                .iter()
                .any(|&r| in_shared(sem.reference(r).node))
    })
}

fn has_direct_block_scoped_declaration(ast: &Ast, stmt: NodeId) -> bool {
    is_block_scoped_declaration(ast.node(stmt))
        || block_statements(ast, stmt)
            .iter()
            .any(|&s| is_block_scoped_declaration(ast.node(s)))
}

fn hoist_fix(cx: &Ctx<'_, '_>, if_stmt: NodeId, blocks: &[NodeId], s: Shared) -> Fix {
    let ast = cx.ast();
    let src = cx.src();
    let span = cx.span(if_stmt);
    let if_indent = indent_before(src, span.start);
    let moved = s.of(block_statements(ast, blocks[0]));
    let (first, last) = (moved[0], moved[moved.len() - 1]);
    let first_start = cx.span(first).start;
    let text = reindent_text(
        &src[first_start as usize..cx.span(last).end as usize],
        indent_before(src, first_start),
        if_indent,
    );
    let brk = linebreak(src);
    let mut fix = if s.start {
        let semi = if needs_semicolon(cx, span.start, &text) {
            ";"
        } else {
            ""
        };
        Fix::replace(
            Span::new(span.start, span.start),
            format!("{semi}{text}{brk}{if_indent}"),
        )
    } else {
        let next = skip_trivia_forward(src, span.end as usize);
        let semi = if next < src.len() && needs_semicolon(cx, cx.span(last).end, &src[next..]) {
            ";"
        } else {
            ""
        };
        Fix::replace(
            Span::new(span.end, span.end),
            format!("{brk}{if_indent}{text}{semi}"),
        )
    };
    for &block in blocks {
        fix = fix.and(s.removal(cx, block_statements(ast, block)), "");
    }
    fix
}

fn report(cx: &mut Ctx<'_, '_>, if_stmt: NodeId, tests: &[NodeId], blocks: &[NodeId], s: Shared) {
    let ast = cx.ast();
    let reported = s.of(block_statements(ast, blocks[0]));
    let valid =
        reported.iter().all(|&st| {
            !has_direct_block_scoped_declaration(ast, st) && !has_multiline_token(cx, st)
        }) && cx.parent(if_stmt).is_some_and(|p| {
            matches!(
                cx.node(p),
                Node::Program { .. }
                    | Node::Block { .. }
                    | Node::StaticBlock { .. }
                    | Node::SwitchCase { .. }
            )
        }) && !has_comment_hazard(cx, if_stmt, blocks, s)
            && blocks.iter().all(|&b| !has_branch_local_hazard(cx, b, s));
    let span = Span::new(
        cx.span(reported[0]).start,
        cx.span(reported[reported.len() - 1]).end,
    );
    let fix = valid.then(|| hoist_fix(cx, if_stmt, blocks, s));
    let autofix = !s.start
        || (tests.iter().all(|&t| !has_effect(ast, t))
            && reported
                .iter()
                .all(|&st| matches!(ast.node(st), Node::ExprStmt { .. }) && !has_effect(ast, st)));
    let (message, id) = if s.start {
        (
            "These branches start with the same code. Move it before the `if` statement.",
            "prefer-hoisting-branch-code/start",
        )
    } else {
        (
            "These branches end with the same code. Move it after the `if` statement.",
            "prefer-hoisting-branch-code/end",
        )
    };
    let r = cx.report(span, message).id(id);
    match fix {
        Some(fix) if autofix => {
            r.fix(fix);
        }
        Some(fix) => {
            r.suggest(
                "prefer-hoisting-branch-code/suggestion",
                "Move the shared code out of the branches.",
                fix,
            );
        }
        None => {}
    }
}

impl Rule for PreferHoistingBranchCode {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::If]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        if cx
            .parent(node)
            .is_some_and(|p| matches!(*cx.node(p), Node::If { alt: Some(a), .. } if a == node))
        {
            return;
        }
        let mut tests = Vec::new();
        let mut blocks = Vec::new();
        let mut cur = node;
        loop {
            match *ast.node(cur) {
                Node::If { test, cons, alt } => {
                    tests.push(test);
                    blocks.push(cons);
                    match alt {
                        Some(a) => cur = a,
                        None => return,
                    }
                }
                _ => {
                    blocks.push(cur);
                    break;
                }
            }
        }
        if blocks
            .iter()
            .any(|&b| !matches!(ast.node(b), Node::Block { .. }))
        {
            return;
        }
        let bodies: Vec<&[NodeId]> = blocks.iter().map(|&b| block_statements(ast, b)).collect();
        let min = bodies.iter().map(|b| b.len()).min().unwrap_or(0);
        let first = bodies[0];
        let mut leading = 0;
        while leading < min
            && bodies
                .iter()
                .all(|b| same_statement(cx, b[leading], first[leading]))
        {
            leading += 1;
        }
        let mut trailing = 0;
        while leading + trailing < min
            && bodies.iter().all(|b| {
                same_statement(
                    cx,
                    b[b.len() - 1 - trailing],
                    first[first.len() - 1 - trailing],
                )
            })
        {
            trailing += 1;
        }
        if (leading == 0 && trailing == 0) || leading + trailing >= min {
            return;
        }
        if leading > 0 {
            let s = Shared {
                leading,
                trailing,
                start: true,
            };
            report(cx, node, &tests, &blocks, s);
        }
        if trailing > 0 {
            let s = Shared {
                leading,
                trailing,
                start: false,
            };
            report(cx, node, &tests, &blocks, s);
        }
    }
}
