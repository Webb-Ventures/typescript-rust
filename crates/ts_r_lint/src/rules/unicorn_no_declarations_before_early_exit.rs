//! `unicorn/no-declarations-before-early-exit`: a `let`/`const` declared before a guard
//! `if` (exactly one branch always exits) but only used after it should move below it.
//!
//! The first guard after the declaration in its statement list (a block or the program)
//! decides. Skipped: destructuring, React hook calls, initializers with `await`/`yield`,
//! non-literal initializers when other statements sit between, initializers sharing a
//! variable with the guard when either side has a side effect, no use other than in a
//! type query, a use before the end of the guard, and a use inside a function declaration
//! when the statements up to the guard make a call. Fixed (swapping the two statements)
//! only for one declarator with a literal (or plain template) initializer right before
//! the guard, with no comments around.

use super::prelude::*;
use super::unicorn_ast::contains_node;
use super::unicorn_conditions::{always_exits, comment_after, comment_before, idents_in};
use super::unicorn_static::has_side_effect;
use crate::semantic::RefFlags;
use ts_r_ast::Ast;
use ts_r_ast::VarKind;
use ts_r_ast::visit::{Visitor, Walk, walk};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-declarations-before-early-exit",
        "Disallow declarations before conditional early exits when they are only used after the exit.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<NoDeclarationsBeforeEarlyExit>,
};

#[derive(Debug, Default)]
struct NoDeclarationsBeforeEarlyExit;

impl FromOptions for NoDeclarationsBeforeEarlyExit {
    fn from_options(_opts: &[Value]) -> Result<Self, String> {
        Ok(NoDeclarationsBeforeEarlyExit)
    }
}

const MESSAGE: &str = "Move this declaration after the early exit.";

fn is_react_hook_name(name: &str) -> bool {
    name == "use"
        || name
            .strip_prefix("use")
            .and_then(|rest| rest.chars().next())
            .is_some_and(|c| c.is_ascii_digit() || c.is_ascii_uppercase())
}

fn is_react_hook_call(ast: &Ast, id: NodeId) -> bool {
    let Node::Call { callee, .. } = *ast.node(id) else {
        return false;
    };
    match *ast.node(callee) {
        Node::Ident { .. } => ident_name(ast, callee).is_some_and(is_react_hook_name),
        Node::Member { obj, prop, .. } => {
            matches!(ast.node(obj), Node::Ident { .. })
                && ident_name(ast, obj)
                    .is_some_and(|n| n.starts_with(|c: char| c.is_ascii_uppercase()))
                && ident_name(ast, prop).is_some_and(is_react_hook_name)
        }
        _ => false,
    }
}

fn contains_suspension_point(ast: &Ast, root: NodeId) -> bool {
    struct Find {
        found: bool,
    }
    impl Visitor for Find {
        fn enter(&mut self, ast: &Ast, id: NodeId) -> Walk {
            let node = ast.node(id);
            if matches!(
                node,
                Node::Await { .. } | Node::Yield { .. } | Node::ForOf { is_await: true, .. }
            ) {
                self.found = true;
                return Walk::Stop;
            }
            if is_function(node.kind()) {
                return Walk::SkipChildren;
            }
            Walk::Continue
        }
    }
    let mut f = Find { found: false };
    walk(ast, root, &mut f);
    f.found
}

fn is_simple_initializer(ast: &Ast, init: Option<NodeId>) -> bool {
    init.is_none_or(|i| match ast.node(i) {
        Node::StringLit { .. }
        | Node::NumberLit { .. }
        | Node::BigIntLit { .. }
        | Node::BoolLit { .. }
        | Node::NullLit {}
        | Node::RegexLit { .. } => true,
        Node::TemplateLit { parts } => ast.list(*parts).len() <= 1,
        _ => false,
    })
}

/// Names of the variables referenced under `root`.
fn referenced_names<'a>(cx: &Ctx<'a, '_>, root: NodeId) -> Vec<&'a str> {
    let ast = cx.ast();
    let sem = cx.semantic();
    idents_in(ast, root)
        .into_iter()
        .filter(|&i| sem.reference_at(i).is_some())
        .filter_map(|i| ident_name(ast, i))
        .collect()
}

fn share_variable(cx: &Ctx<'_, '_>, a: NodeId, b: NodeId) -> bool {
    let names = referenced_names(cx, b);
    referenced_names(cx, a).iter().any(|n| names.contains(n))
}

/// The reference at `id` sits in a function declaration below `scope_node`.
fn in_function_declaration(cx: &Ctx<'_, '_>, id: NodeId, scope_node: NodeId) -> bool {
    let mut cur = id;
    for _ in 0..100_000 {
        let Some(p) = cx.parent(cur) else {
            return false;
        };
        if p == scope_node {
            return false;
        }
        if matches!(cx.node(p), Node::FunctionDecl { .. }) {
            return true;
        }
        cur = p;
    }
    false
}

fn is_guard(cx: &Ctx<'_, '_>, stmt: NodeId) -> bool {
    let Node::If { cons, alt, .. } = *cx.node(stmt) else {
        return false;
    };
    always_exits(cx, cons) != alt.is_some_and(|a| always_exits(cx, a))
}

struct Site<'s> {
    stmts: &'s [NodeId],
    decl: NodeId,
    decl_index: usize,
    guard: NodeId,
    guard_index: usize,
    declarators: usize,
}

fn check(cx: &mut Ctx<'_, '_>, site: &Site<'_>, declarator: NodeId) {
    let ast = cx.ast();
    let Node::VarDeclarator { name, init, .. } = *ast.node(declarator) else {
        return;
    };
    if !matches!(ast.node(name), Node::Ident { .. }) {
        return;
    }
    if let Some(i) = init
        && (is_react_hook_call(ast, i) || contains_suspension_point(ast, i))
    {
        return;
    }
    let simple = is_simple_initializer(ast, init);
    if site.guard_index > site.decl_index + 1 && !simple {
        return;
    }
    let Node::If { test, .. } = *ast.node(site.guard) else {
        return;
    };
    if let Some(i) = init
        && share_variable(cx, i, site.guard)
        && (has_side_effect(ast, i) || has_side_effect(ast, test))
    {
        return;
    }
    let sem = cx.semantic();
    let Some(sym) = sem.symbol_of_decl(name) else {
        return;
    };
    let refs: Vec<NodeId> = sem
        .symbol(sym)
        .refs
        .iter()
        .map(|&r| sem.reference(r))
        .filter(|r| !r.flags.intersects(RefFlags::INIT | RefFlags::TYPEOF_TYPE))
        .map(|r| r.node)
        .collect();
    if refs.is_empty() {
        return;
    }
    let guard_end = cx.span(site.guard).end;
    if refs.iter().any(|&r| cx.span(r).start < guard_end) {
        return;
    }
    let scope_node = sem.scope(sem.symbol(sym).scope).node;
    if refs
        .iter()
        .any(|&r| in_function_declaration(cx, r, scope_node))
        && site.stmts[site.decl_index + 1..=site.guard_index]
            .iter()
            .any(|&s| {
                contains_node(ast, s, |n| {
                    matches!(
                        n,
                        Node::Call { .. } | Node::New { .. } | Node::TaggedTemplate { .. }
                    )
                })
            })
    {
        return;
    }
    let decl_span = cx.span(site.decl);
    let guard_span = cx.span(site.guard);
    let fixable = site.guard_index == site.decl_index + 1
        && site.declarators == 1
        && simple
        && ast.comments_in(decl_span).is_empty()
        && !comment_before(cx, decl_span.start)
        && !comment_after(cx, guard_span.end)
        && ast
            .comments_in(Span::new(decl_span.end, guard_span.start))
            .is_empty();
    let fix = fixable.then(|| {
        let src = cx.src();
        let text = format!(
            "{}{}{}",
            &src[guard_span.start as usize..guard_span.end as usize],
            &src[decl_span.end as usize..guard_span.start as usize],
            &src[decl_span.start as usize..decl_span.end as usize]
        );
        Fix::replace(Span::new(decl_span.start, guard_span.end), text)
    });
    let span = cx.span(name);
    cx.report(span, MESSAGE)
        .id("no-declarations-before-early-exit")
        .fix_opt(fix);
}

fn check_list(cx: &mut Ctx<'_, '_>, stmts: &[NodeId]) {
    let ast = cx.ast();
    // `Some(is_guard)` once resolved; statements after one that always exits are dead.
    let mut guards: Vec<Option<bool>> = vec![None; stmts.len()];
    let first_exit = stmts.iter().position(|&s| always_exits(cx, s));
    for (decl_index, &decl) in stmts.iter().enumerate() {
        let Node::VarDecl {
            modifiers,
            kind: VarKind::Let | VarKind::Const,
            decls,
        } = *ast.node(decl)
        else {
            continue;
        };
        if modifiers.intersects(Modifiers::DECLARE | Modifiers::EXPORT) {
            continue;
        }
        for guard_index in decl_index + 1..stmts.len() {
            let guard = *guards[guard_index].get_or_insert_with(|| {
                first_exit.is_none_or(|e| e >= guard_index) && is_guard(cx, stmts[guard_index])
            });
            if !guard {
                continue;
            }
            let declarators = ast.list(decls);
            let site = Site {
                stmts,
                decl,
                decl_index,
                guard: stmts[guard_index],
                guard_index,
                declarators: declarators.len(),
            };
            for &d in declarators {
                check(cx, &site, d);
            }
            break;
        }
    }
}

impl Rule for NoDeclarationsBeforeEarlyExit {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Program, NodeKind::Block]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let (Node::Program { stmts } | Node::Block { stmts }) = *ast.node(node) else {
            return;
        };
        check_list(cx, ast.list(stmts));
    }
}
