//! `unicorn/prefer-dispose`: a `try` whose `finally` only disposes resources declared just
//! before it should declare them with `using` / `await using`.
//!
//! The `finally` holds only statements `x.close()`, `x.dispose()`, `x.destroy()`, `x.end()`
//! or `x[Symbol.dispose]()` / `x[Symbol.asyncDispose]()` (no arguments, optional chaining
//! allowed, `await` allowed; `asyncDispose` or an `await` asks for `await using`), each for a
//! different name. Each name must resolve to a single-declarator `const`/`let` with a plain
//! name and an initialiser, not exported; the declarations must be exactly the statements
//! right before the `try`, in the reverse of the `finally` order; the names are not written
//! again, nor used after the `try`. `await using` needs an async function or module top
//! level (never a class static block). Types are not consulted.
//!
//! The suggestion opens a block at the first declaration, turns the keywords into
//! `using`, and replaces the `try`: with a `catch`, the `try`/`catch` stays (moved one level
//! in) and only the `finally` goes; without one, the `try` block's body is unwrapped. It is
//! withheld when a comment outside the `try` block and the `catch` would be lost, or (without
//! a `catch`) when the `try` block declares one of the names itself.

use super::prelude::*;
use super::unicorn_ast::linebreak;
use super::unicorn_iteration_fallback_style::reindent;
use super::unicorn_util::{indent_unit, line_indent, line_start};
use crate::env::SourceType;
use ts_r_ast::{Ast, FnFlags};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-dispose",
        "Prefer using `using`/`await using` over manual `try`/`finally` resource disposal.",
        NO_PRESET,
        Category::Pedantic,
    )
    .suggestions(),
    create: create_default::<PreferDispose>,
};

#[derive(Debug, Default)]
struct PreferDispose;

const DISPOSAL_METHODS: &[&str] = &["close", "dispose", "destroy", "end"];

struct Resource {
    name: NodeId,
    awaited: bool,
    symbol: SymbolId,
    /// The `VarDecl` statement.
    declaration: NodeId,
}

/// A disposal statement: the disposed identifier and whether it asks for `await using`.
fn disposal(ast: &Ast, stmt: NodeId) -> Option<(NodeId, bool)> {
    let Node::ExprStmt { expr } = *ast.node(stmt) else {
        return None;
    };
    let mut expr = skip_parens(ast, expr);
    let mut awaited = false;
    if let Node::Await { arg } = *ast.node(expr) {
        awaited = true;
        expr = skip_parens(ast, arg);
    }
    let Node::Call { callee, args, .. } = *ast.node(expr) else {
        return None;
    };
    if !ast.list(args).is_empty() {
        return None;
    }
    let (obj, ok) = match *ast.node(skip_parens(ast, callee)) {
        Node::Member { obj, prop, .. } => (
            obj,
            matches!(ast.node(prop), Node::Ident { .. })
                && ident_name(ast, prop).is_some_and(|n| DISPOSAL_METHODS.contains(&n)),
        ),
        Node::Index { obj, index, .. } => {
            let Node::Member { obj: sym, prop, .. } = *ast.node(skip_parens(ast, index)) else {
                return None;
            };
            let sym = skip_parens(ast, sym);
            if !matches!(ast.node(sym), Node::Ident { .. })
                || ident_name(ast, sym) != Some("Symbol")
                || !matches!(ast.node(prop), Node::Ident { .. })
            {
                return None;
            }
            match ident_name(ast, prop) {
                Some("dispose") => (obj, true),
                Some("asyncDispose") => {
                    awaited = true;
                    (obj, true)
                }
                _ => return None,
            }
        }
        _ => return None,
    };
    let obj = skip_parens(ast, obj);
    (ok && matches!(ast.node(obj), Node::Ident { .. })).then_some((obj, awaited))
}

/// `await` is allowed where the `try` sits: an async function, or a module's top level.
fn in_async_context(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let mut cur = node;
    for _ in 0..100_000 {
        let Some(p) = cx.parent(cur) else {
            return false;
        };
        match *ast.node(p) {
            Node::FunctionDecl { flags, .. }
            | Node::FunctionExpr { flags, .. }
            | Node::Arrow { flags, .. } => return flags.contains(FnFlags::ASYNC),
            Node::StaticBlock { .. } => return false,
            Node::Program { .. } => return cx.env().source_type == SourceType::Module,
            _ => cur = p,
        }
    }
    false
}

impl PreferDispose {
    /// The resources of the `try`, in declaration order, when it can be converted.
    fn resources(&self, cx: &Ctx<'_, '_>, try_stmt: NodeId) -> Option<Vec<Resource>> {
        let ast = cx.ast();
        let Node::Try {
            finalizer: Some(finalizer),
            ..
        } = *ast.node(try_stmt)
        else {
            return None;
        };
        let Node::Block { stmts } = *ast.node(finalizer) else {
            return None;
        };
        let stmts = ast.list(stmts);
        if stmts.is_empty() {
            return None;
        }
        let disposals: Vec<(NodeId, bool)> = stmts
            .iter()
            .map(|&s| disposal(ast, s))
            .collect::<Option<_>>()?;
        for (i, &(a, _)) in disposals.iter().enumerate() {
            if disposals[..i]
                .iter()
                .any(|&(b, _)| ident_name(ast, a) == ident_name(ast, b))
            {
                return None;
            }
        }
        let parent = cx.parent(try_stmt)?;
        let body = match *ast.node(parent) {
            Node::Block { stmts } | Node::Program { stmts } => ast.list(stmts),
            _ => return None,
        };
        let sem = cx.semantic();
        let scope = sem.enclosing_scope(try_stmt);
        let mut resources = Vec::with_capacity(disposals.len());
        for &(id, awaited) in &disposals {
            let symbol = sem.lookup(scope, ident_name(ast, id)?)?;
            let &[decl] = &sem.symbol(symbol).decls[..] else {
                return None;
            };
            if !matches!(decl.kind, DeclKind::Const | DeclKind::Let) {
                return None;
            }
            let Node::VarDeclarator {
                name,
                init: Some(_),
                ..
            } = *ast.node(decl.node)
            else {
                return None;
            };
            if name != decl.name || !matches!(ast.node(name), Node::Ident { .. }) {
                return None;
            }
            let Node::VarDecl {
                modifiers,
                kind: VarKind::Const | VarKind::Let,
                decls,
            } = *ast.node(decl.parent)
            else {
                return None;
            };
            if ast.list(decls).len() != 1
                || modifiers.contains(Modifiers::EXPORT)
                || !body.contains(&decl.parent)
            {
                return None;
            }
            resources.push(Resource {
                name,
                awaited,
                symbol,
                declaration: decl.parent,
            });
        }
        // `using` disposes in reverse declaration order: the `finally` must already do so.
        let disposal_order: Vec<NodeId> = resources.iter().map(|r| r.declaration).collect();
        let index_of = |d: NodeId| body.iter().position(|&s| s == d).unwrap_or(usize::MAX);
        resources.sort_by_key(|r| index_of(r.declaration));
        let try_index = index_of(try_stmt);
        let first = try_index.checked_sub(resources.len())?;
        if resources
            .iter()
            .enumerate()
            .any(|(i, r)| body.get(first + i) != Some(&r.declaration))
            || disposal_order
                .iter()
                .zip(resources.iter().rev())
                .any(|(&d, r)| d != r.declaration)
        {
            return None;
        }
        let try_end = cx.span(try_stmt).end;
        for r in &resources {
            for &rid in &sem.symbol(r.symbol).refs {
                let rf = sem.reference(rid);
                if (rf.is_write() && !rf.flags.contains(RefFlags::INIT))
                    || cx.span(rf.node).start > try_end
                {
                    return None;
                }
            }
        }
        if resources.iter().any(|r| r.awaited) && !in_async_context(cx, try_stmt) {
            return None;
        }
        Some(resources)
    }

    fn fix(&self, cx: &Ctx<'_, '_>, try_stmt: NodeId, resources: &[Resource]) -> Option<Fix> {
        let ast = cx.ast();
        let src = cx.src();
        let Node::Try { block, handler, .. } = *ast.node(try_stmt) else {
            return None;
        };
        let lb = linebreak(src);
        let unit = indent_unit(cx);
        let try_span = cx.span(try_stmt);
        let block_indent = line_indent(src, try_span.start as usize);
        let mut edits: Vec<(Span, String)> = Vec::with_capacity(resources.len() + 1);
        for (i, r) in resources.iter().enumerate() {
            let Node::VarDecl { kind, .. } = *ast.node(r.declaration) else {
                return None;
            };
            let start = cx.span(r.declaration).start;
            let len = match kind {
                VarKind::Const => 5,
                _ => 3,
            };
            let keyword = if r.awaited { "await using" } else { "using" };
            let text = if i == 0 {
                format!("{{{lb}{}{unit}{keyword}", line_indent(src, start as usize))
            } else {
                format!("{unit}{keyword}")
            };
            edits.push((Span::new(start, start + len), text));
        }
        let body_indent = format!("{block_indent}{unit}");
        let line = line_start(src, try_span.start as usize);
        let replace_start = if src[line..try_span.start as usize].trim().is_empty() {
            line as u32
        } else {
            try_span.start
        };
        let multiline_template = has_multiline_template(cx, try_stmt);
        let re = |text: &str, from: &str, to: &str| {
            if multiline_template {
                text.to_owned()
            } else {
                reindent(text, from, to)
            }
        };
        let replace = Span::new(replace_start, try_span.end);
        let text = if let Some(handler) = handler {
            let handler_end = cx.span(handler).end;
            let kept = re(
                &format!("{lb}{}", &src[replace_start as usize..handler_end as usize]),
                block_indent,
                &body_indent,
            );
            let kept = kept[lb.len()..].trim_end();
            format!("{kept}{lb}{block_indent}}}")
        } else {
            let span = cx.span(block);
            let raw = re(
                &src[span.start as usize + 1..(span.end as usize).saturating_sub(1)],
                &body_indent,
                &body_indent,
            );
            let is_break = |c: char| matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}');
            let body = if raw.starts_with(is_break) {
                raw.trim_start_matches(is_break).trim_end().to_owned()
            } else {
                let rest = raw.trim_start_matches(|c: char| c.is_whitespace() && !is_break(c));
                format!("{body_indent}{}", rest.trim_end())
            };
            if body.trim().is_empty() {
                format!("{lb}{block_indent}}}")
            } else {
                format!("{lb}{body}{lb}{block_indent}}}")
            }
        };
        let (first, rest) = edits.split_first()?;
        let mut fix = Fix::replace(first.0, first.1.clone());
        for (span, text) in rest {
            fix = fix.and(*span, text.clone());
        }
        Some(fix.and(replace, text))
    }
}

/// A template piece inside `root` that spans lines (re-indenting would change its value).
fn has_multiline_template(cx: &Ctx<'_, '_>, root: NodeId) -> bool {
    let ast = cx.ast();
    let src = cx.src();
    let mut stack = vec![root];
    let mut guard = 0u32;
    while let Some(n) = stack.pop() {
        guard += 1;
        if guard > 1_000_000 {
            return true;
        }
        if matches!(ast.node(n), Node::TemplateElement { .. })
            && ast
                .span(n)
                .text(src)
                .contains(['\n', '\r', '\u{2028}', '\u{2029}'])
        {
            return true;
        }
        ast.for_each_child(n, |c| stack.push(c));
    }
    false
}

impl Rule for PreferDispose {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Try]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Some(resources) = self.resources(cx, node) else {
            return;
        };
        let ast = cx.ast();
        let Node::Try { block, handler, .. } = *ast.node(node) else {
            return;
        };
        let keyword = if resources.iter().any(|r| r.awaited) {
            "await using"
        } else {
            "using"
        };
        let span = cx.span(node);
        let block_span = cx.span(block);
        let handler_span = handler.map(|h| cx.span(h));
        let loses_comment = ast.comments_in(span).iter().any(|c| {
            !block_span.contains_span(c.span)
                && !handler_span.is_some_and(|h| h.contains_span(c.span))
        });
        let shadowed = handler.is_none() && {
            let sem = cx.semantic();
            sem.scope_of_node(block).is_some_and(|s| {
                sem.scope(s).symbols.iter().any(|&sym| {
                    let n = sem.name(sym);
                    resources.iter().any(|r| ident_name(ast, r.name) == Some(n))
                })
            })
        };
        let fix = (!loses_comment && !shadowed)
            .then(|| self.fix(cx, node, &resources))
            .flatten();
        let report = cx
            .report(
                span,
                format!(
                    "Prefer `{keyword}` (Explicit Resource Management) over the `try`/`finally` disposal pattern."
                ),
            )
            .id("prefer-dispose/error");
        if let Some(fix) = fix {
            report.suggest(
                "prefer-dispose/suggestion",
                format!("Replace the `try`/`finally` with `{keyword}`."),
                fix,
            );
        }
    }
}
