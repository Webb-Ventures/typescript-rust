//! `unicorn/prefer-string-match-all`: a `while` loop whose test is `(m = re.exec(s))`, alone
//! or compared with `null` by `!==` / `!=` (either side), right after `let m;` (one
//! declarator, no initializer) in the same statement list, is a `for (const m of
//! s.matchAll(re))` loop. Only when it is provably equivalent:
//! - `exec` is a plain, non-optional, non-computed call with one non-spread argument;
//! - `re` is a single `let` / `const` declaration (in the statement list holding the loop)
//!   initialized with a regex literal or `new RegExp(...)` of static, side-effect-free
//!   arguments, whose static value has the `g` flag and cannot match the empty string
//!   (anything unparsable counts as possibly empty), and it is only used in its
//!   initializer and in the loop test;
//! - `s` is a `const` whose side-effect-free static value is a string;
//! - `m` is not used outside the loop, not written in the body, and not read in a function
//!   or class member in the body.
//!
//! The loop is reported; the fix drops the `let` and rewrites the loop header, unless a
//! comment sits between the `let` and the header's `)`.

use super::prelude::*;
use super::unicorn_ast::{method_call, would_remove_comments};
use super::unicorn_static::{Static, static_value, static_value_no_side_effects};
use crate::regex::{self, Flags, RId, RKind, Regex};
use crate::semantic::{DeclKind, RefFlags, SymbolId};
use ts_r_ast::{Ast, VarKind};
use ts_r_span::Span;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-string-match-all",
        "Prefer `String#matchAll()` over `RegExp#exec()` loops.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<PreferStringMatchAll>,
};

#[derive(Debug, Default)]
struct PreferStringMatchAll;

const MAX_DEPTH: u32 = 128;

/// The assignment the loop test performs: the test itself or the non-`null` side of a
/// `!==` / `!=` comparison with `null`.
fn test_assignment(ast: &Ast, test: NodeId) -> Option<NodeId> {
    let test = skip_parens(ast, test);
    let id = match *ast.node(test) {
        Node::Assign { .. } => test,
        Node::Binary {
            left,
            op: BinaryOp::StrictNotEq | BinaryOp::NotEq,
            right,
        } => {
            let is_null = |id| matches!(ast.node(skip_parens(ast, id)), Node::NullLit {});
            if is_null(right) {
                left
            } else if is_null(left) {
                right
            } else {
                return None;
            }
        }
        _ => return None,
    };
    let id = skip_parens(ast, id);
    matches!(ast.node(id), Node::Assign { .. }).then_some(id)
}

/// The pattern matches the empty string, or might (parse errors, back references,
/// assertions and lookarounds all count as "might").
fn nullable(re: &Regex<'_>, id: RId, depth: u32) -> bool {
    if depth > MAX_DEPTH {
        return true;
    }
    let node = re.node(id);
    let any = || node.children.iter().any(|&c| nullable(re, c, depth + 1));
    match node.kind {
        RKind::Pattern | RKind::Capture { .. } => any(),
        RKind::Group => {
            // Only a plain `(?:...)`; a modifiers group counts as unknown.
            !re.text(id).starts_with("(?:") || any()
        }
        RKind::Alternative => node.children.iter().all(|&c| nullable(re, c, depth + 1)),
        RKind::Quantifier { min, .. } => min == 0 || any(),
        RKind::Char { .. } | RKind::Range | RKind::CharSet { .. } | RKind::Dot => false,
        RKind::Class { .. } | RKind::SetOp { .. } => node
            .children
            .iter()
            .any(|&c| matches!(re.kind(c), RKind::ClassStrings) || nullable(re, c, depth + 1)),
        _ => true,
    }
}

fn can_match_empty(source: &str, flags: &str) -> bool {
    match regex::parse(source, Flags::new(flags)) {
        Ok(re) => nullable(&re, re.root, 0),
        Err(_) => true,
    }
}

/// The regex a `let` / `const` initializer statically builds.
fn static_regexp(cx: &Ctx<'_, '_>, init: NodeId) -> Option<(String, String)> {
    let ast = cx.ast();
    let init = skip_parens(ast, init);
    match *ast.node(init) {
        Node::RegexLit { .. } => {}
        Node::New {
            callee,
            args,
            has_args,
            ..
        } if ident_name(ast, callee) == Some("RegExp") => {
            let args = if has_args { ast.list(args) } else { &[] };
            let safe = args.iter().all(|&a| {
                !matches!(ast.node(a), Node::Spread { .. })
                    && matches!(
                        static_value_no_side_effects(cx, a),
                        Some(
                            Static::RegExp { .. }
                                | Static::Null
                                | Static::Undefined
                                | Static::Bool(_)
                                | Static::Num(_)
                                | Static::Str(_)
                                | Static::BigInt(_)
                        )
                    )
            });
            if !safe {
                return None;
            }
        }
        _ => return None,
    }
    match static_value(cx, init)? {
        Static::RegExp { source, flags } => Some((source, flags)),
        _ => None,
    }
}

fn contains(outer: Span, inner: Span) -> bool {
    outer.start <= inner.start && inner.end <= outer.end
}

/// Between `id` and `body`, a function or class member defers evaluation.
fn in_delayed_scope(cx: &Ctx<'_, '_>, id: NodeId, body: NodeId) -> bool {
    let ast = cx.ast();
    let mut cur = id;
    while cur != body {
        let Some(parent) = cx.parent(cur) else {
            return false;
        };
        match *ast.node(parent) {
            Node::FunctionExpr { .. } | Node::FunctionDecl { .. } | Node::Arrow { .. } => {
                return true;
            }
            Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. }
                if ast.list(members).contains(&cur) =>
            {
                return true;
            }
            _ => {}
        }
        cur = parent;
    }
    false
}

impl Rule for PreferStringMatchAll {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::While]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::While { test, body } = *cx.node(node) else {
            return;
        };
        let Some(assign) = test_assignment(ast, test) else {
            return;
        };
        let Node::Assign {
            left,
            op: AssignOp::Assign,
            right,
        } = *ast.node(assign)
        else {
            return;
        };
        let Some(match_name) = ident_name(ast, left) else {
            return;
        };
        let Some(call) = method_call(ast, skip_parens(ast, right)) else {
            return;
        };
        if call.name != "exec"
            || !call.plain()
            || call.args.len() != 1
            || matches!(ast.node(call.args[0]), Node::Spread { .. })
        {
            return;
        }
        let (regexp, string) = (call.obj, call.args[0]);

        // `let m;` right before the loop.
        let Some(list_owner) = cx.parent(node) else {
            return;
        };
        let stmts = match *ast.node(list_owner) {
            Node::Program { stmts } | Node::Block { stmts } => ast.list(stmts),
            Node::StaticBlock { body } => ast.list(body),
            _ => return,
        };
        let Some(index) = stmts.iter().position(|&s| s == node) else {
            return;
        };
        let Some(&previous) = index.checked_sub(1).and_then(|i| stmts.get(i)) else {
            return;
        };
        let Node::VarDecl {
            kind: VarKind::Let,
            decls,
            ..
        } = *ast.node(previous)
        else {
            return;
        };
        let &[declarator] = ast.list(decls) else {
            return;
        };
        let Node::VarDeclarator {
            name, init: None, ..
        } = *ast.node(declarator)
        else {
            return;
        };
        if ident_name(ast, name) != Some(match_name) {
            return;
        }

        // The regex variable.
        if !matches!(ast.node(regexp), Node::Ident { .. }) {
            return;
        }
        let sem = cx.semantic();
        let Some(re_sym) = sem.symbol_of_ident(regexp) else {
            return;
        };
        let [re_decl] = sem.symbol(re_sym).decls.as_slice() else {
            return;
        };
        if !matches!(
            re_decl.kind,
            DeclKind::Let | DeclKind::Const | DeclKind::Using
        ) {
            return;
        }
        let Node::VarDeclarator {
            init: Some(re_init),
            ..
        } = *ast.node(re_decl.node)
        else {
            return;
        };
        let Some((source, flags)) = static_regexp(cx, re_init) else {
            return;
        };
        if !flags.contains('g') || can_match_empty(&source, &flags) {
            return;
        }

        // The string constant.
        if !matches!(ast.node(string), Node::Ident { .. }) {
            return;
        }
        let Some(str_sym) = sem.symbol_of_ident(string) else {
            return;
        };
        let [str_decl] = sem.symbol(str_sym).decls.as_slice() else {
            return;
        };
        if str_decl.kind != DeclKind::Const
            || !matches!(
                static_value_no_side_effects(cx, string),
                Some(Static::Str(_))
            )
        {
            return;
        }

        // The regex is declared next to the loop and only used in the test.
        if cx.parent(re_decl.parent) != Some(list_owner) {
            return;
        }
        let test_span = cx.span(test);
        let refs_of = |sym: SymbolId| {
            sem.symbol(sym)
                .refs
                .iter()
                .map(|&r| sem.reference(r))
                .collect::<Vec<_>>()
        };
        if !refs_of(re_sym)
            .iter()
            .all(|r| r.flags.contains(RefFlags::INIT) || contains(test_span, cx.span(r.node)))
        {
            return;
        }

        // The match variable stays inside the loop.
        let Some(m_sym) = sem.symbol_of_ident(left) else {
            return;
        };
        let (loop_span, body_span) = (cx.span(node), cx.span(body));
        for r in refs_of(m_sym) {
            if r.node == name {
                continue;
            }
            let at = cx.span(r.node);
            if !contains(loop_span, at) {
                return;
            }
            if contains(body_span, at) && (r.is_write() || in_delayed_scope(cx, r.node, body)) {
                return;
            }
        }

        // The header ends at the `)` before the body.
        let src = cx.src();
        let close = skip_trivia_backward(ast, src, body_span.start as usize);
        if close == 0 || src.as_bytes()[close - 1] != b')' {
            return;
        }
        let start = cx.span(previous).start;
        let replaced = Span::new(start, close as u32);
        let fix = (!would_remove_comments(ast, replaced, &[])).then(|| {
            Fix::replace(Span::new(start, loop_span.start), "").and(
                Span::new(loop_span.start, close as u32),
                format!(
                    "for (const {match_name} of {}.matchAll({}))",
                    cx.text(string),
                    cx.text(regexp)
                ),
            )
        });
        cx.report_node(
            node,
            "Prefer `String#matchAll()` over a `RegExp#exec()` loop.",
        )
        .id("prefer-string-match-all")
        .fix_opt(fix);
    }
}
