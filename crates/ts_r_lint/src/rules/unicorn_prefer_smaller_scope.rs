//! `unicorn/prefer-smaller-scope`: a declaration used only inside one block belongs in it.
//!
//! - `const`/`let x = <literal>` right before an `if` whose consequent or alternate block
//!   holds every reference (not across a function, class, static block or `with`) moves
//!   to the start of that block.
//! - `let x;` written once by `x = ..;` directly in a block that holds every reference,
//!   none read before it, becomes `const x = ..` there.
//! - `type T = ..` whose type references all sit in one block moves to the block's first
//!   non-directive statement, reindented. No fix when `T` is also a value, recursive or
//!   shadowed there, or the alias reads a value, `typeof`, `this`, a reserved word, has a
//!   multi-line token, or a name that resolves differently inside the block.
//!
//! Exported (`export {x}`, `/* exported x */`) declarations, `declare`, type parameters
//! with variance and code with `with` or direct `eval` are left alone; a type annotation
//! or a comment next to the moved code withholds the fix.

use super::prelude::*;
use super::unicorn_ast::remove_statements;
use super::unicorn_conditions::{
    comment_after, comment_before, has_multiline_token, reindent_text,
};
use super::unicorn_text::{is_parenthesized, paren_span, paren_text};
use super::unicorn_util_h1::is_direct_eval_call;
use super::unicorn_util_h2::{indent_unit, line_indent};
use crate::semantic::{RefFlags, ScopeId, SymbolId};
use ts_r_ast::Ast;
use ts_r_ast::visit::{Visitor, Walk, walk};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-smaller-scope",
        "Prefer declaring variables and type aliases in the smallest possible scope.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<PreferSmallerScope>,
};

#[derive(Debug, Default)]
struct PreferSmallerScope;

const RESERVED: &[&str] = &[
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "function",
    "if",
    "import",
    "in",
    "instanceof",
    "new",
    "null",
    "return",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "var",
    "void",
    "while",
    "with",
    "yield",
    "implements",
    "interface",
    "let",
    "package",
    "private",
    "protected",
    "public",
    "static",
    "arguments",
    "eval",
];

fn message(name: &str) -> String {
    format!("Move `{name}` into the block where it is used.")
}

fn statement_list(ast: &Ast, owner: NodeId) -> Option<&[NodeId]> {
    match *ast.node(owner) {
        Node::Program { stmts } | Node::Block { stmts } => Some(ast.list(stmts)),
        _ => None,
    }
}

/// `export {name}` (no source) among the program's statements.
fn is_exported(cx: &Ctx<'_, '_>, owner: NodeId, name: &str) -> bool {
    let ast = cx.ast();
    let Node::Program { stmts } = *ast.node(owner) else {
        return false;
    };
    ast.list(stmts).iter().any(|&s| match *ast.node(s) {
        Node::ExportNamed {
            specifiers,
            source: None,
            ..
        } => ast.list(specifiers).iter().any(|&sp| {
            matches!(*ast.node(sp), Node::ExportSpecifier { local, .. }
                if ident_name(ast, local) == Some(name))
        }),
        _ => false,
    })
}

/// Marked used by a `/* exported */` comment (global scope only).
fn is_comment_exported(cx: &Ctx<'_, '_>, sym: SymbolId) -> bool {
    let sem = cx.semantic();
    let s = sem.symbol(sym);
    sem.scope(s.scope).parent.is_none()
        && sem.scope(s.scope).kind == crate::semantic::ScopeKind::Global
        && cx.file.exported.iter().any(|n| n == sem.name(sym))
}

/// `node` is inside `ancestor` with no function, class, static block or `with` between.
fn within_without_boundary(cx: &Ctx<'_, '_>, node: NodeId, ancestor: NodeId) -> bool {
    let mut cur = cx.parent(node);
    for _ in 0..100_000 {
        let Some(c) = cur else {
            return false;
        };
        if c == ancestor {
            return true;
        }
        if matches!(
            cx.node(c),
            Node::Arrow { .. }
                | Node::ClassDecl { .. }
                | Node::ClassExpr { .. }
                | Node::FunctionDecl { .. }
                | Node::FunctionExpr { .. }
                | Node::StaticBlock { .. }
                | Node::With { .. }
        ) {
            return false;
        }
        cur = cx.parent(c);
    }
    false
}

fn has_dynamic_scope(ast: &Ast, root: NodeId) -> bool {
    struct Find {
        found: bool,
    }
    impl Visitor for Find {
        fn enter(&mut self, ast: &Ast, id: NodeId) -> Walk {
            if matches!(ast.node(id), Node::With { .. }) || is_direct_eval_call(ast, id) {
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

/// The binding name with its type annotation, as ESTree's `Identifier` spans it.
fn binding_span(cx: &Ctx<'_, '_>, name: NodeId, type_ann: Option<NodeId>) -> Span {
    let span = cx.span(name);
    type_ann.map_or(span, |t| Span::new(span.start, cx.span(t).end))
}

fn has_comment_inside(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    !cx.ast().comments_in(cx.span(id)).is_empty()
}

fn comment_around(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let span = cx.span(id);
    comment_before(cx, span.start) || comment_after(cx, span.end)
}

fn is_inside(cx: &Ctx<'_, '_>, node: NodeId, outer: Span) -> bool {
    let s = cx.span(node);
    s.start >= outer.start && s.end <= outer.end
}

/// The move of `const x = <literal>` into the following `if`'s block.
fn initialized_problem(
    cx: &mut Ctx<'_, '_>,
    decl: NodeId,
    declarator: NodeId,
    init: NodeId,
    refs: &[NodeId],
) {
    let ast = cx.ast();
    let literal = match *ast.node(skip_parens(ast, init)) {
        Node::StringLit { .. }
        | Node::NumberLit { .. }
        | Node::BigIntLit { .. }
        | Node::BoolLit { .. }
        | Node::NullLit {} => true,
        Node::TemplateLit { parts } => ast.list(parts).len() <= 1,
        _ => false,
    };
    let Some(owner) = cx.parent(decl) else {
        return;
    };
    let Some(list) = statement_list(ast, owner) else {
        return;
    };
    let next = list
        .iter()
        .position(|&s| s == decl)
        .and_then(|i| list.get(i + 1))
        .copied();
    let Some(next) = next.filter(|_| literal) else {
        return;
    };
    let Node::If { cons, alt, .. } = *ast.node(next) else {
        return;
    };
    if refs.is_empty() {
        return;
    }
    let block = [Some(cons), alt].into_iter().flatten().find(|&b| {
        matches!(ast.node(b), Node::Block { .. })
            && refs.iter().all(|&r| within_without_boundary(cx, r, b))
    });
    let Some(block) = block else {
        return;
    };
    if has_dynamic_scope(ast, owner) {
        return;
    }
    let Node::VarDeclarator { name, type_ann, .. } = *ast.node(declarator) else {
        return;
    };
    let Some(&first) = statement_list(ast, block).and_then(|l| l.first()) else {
        return;
    };
    let fix = (type_ann.is_none()
        && !has_comment_inside(cx, decl)
        && !comment_around(cx, decl)
        && !comment_before(cx, cx.span(first).start))
    .then(|| {
        let src = cx.src();
        let brace_end = cx.span(block).start + 1;
        let first_start = cx.span(first).start;
        let separator = match &src[brace_end as usize..first_start as usize] {
            "" => " ",
            s => s,
        };
        let text = cx.text(decl);
        let semi = if text.ends_with(';') { "" } else { ";" };
        Fix::replace(Span::new(cx.span(decl).start, cx.span(next).start), "").and(
            Span::new(first_start, first_start),
            format!("{text}{semi}{separator}"),
        )
    });
    let ident = cx.text(name).to_owned();
    cx.report(binding_span(cx, name, type_ann), message(&ident))
        .id("prefer-smaller-scope")
        .fix_opt(fix);
}

/// `let x;` assigned once in a block: `const x = ..` there.
fn uninitialized_problem(
    cx: &mut Ctx<'_, '_>,
    decl: NodeId,
    declarator: NodeId,
    sym: SymbolId,
    refs: &[crate::semantic::RefId],
) {
    let ast = cx.ast();
    let sem = cx.semantic();
    let writes: Vec<NodeId> = refs
        .iter()
        .map(|&r| sem.reference(r))
        .filter(|r| r.is_write())
        .map(|r| r.node)
        .collect();
    if writes.len() != 1 || refs.iter().all(|&r| !sem.reference(r).is_read()) {
        return;
    }
    let ident = writes[0];
    let Some(assign) = cx.parent(ident) else {
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
    if left != ident {
        return;
    }
    let Some(stmt) = parent_skip_parens(ast, cx.file.parents, assign) else {
        return;
    };
    if !matches!(ast.node(stmt), Node::ExprStmt { .. }) {
        return;
    }
    let Some(block) = cx
        .parent(stmt)
        .filter(|&b| matches!(ast.node(b), Node::Block { .. }))
    else {
        return;
    };
    let Some(owner) = cx.parent(decl) else {
        return;
    };
    if !within_without_boundary(cx, block, owner) || has_dynamic_scope(ast, owner) {
        return;
    }
    let stmt_span = cx.span(stmt);
    if stmt_span.start < cx.span(decl).end {
        return;
    }
    if refs.iter().any(|&r| {
        let r = sem.reference(r);
        !within_without_boundary(cx, r.node, block)
            || (r.is_read() && cx.span(r.node).start < stmt_span.end)
    }) {
        return;
    }
    let Node::VarDeclarator { name, type_ann, .. } = *ast.node(declarator) else {
        return;
    };
    let parenthesized = is_parenthesized(cx, assign);
    let sequence_in_parens =
        parenthesized && matches!(ast.node(skip_parens(ast, right)), Node::Seq { .. });
    let blocked = type_ann.is_some()
        || sequence_in_parens
        || has_comment_inside(cx, decl)
        || has_comment_inside(cx, stmt)
        || comment_around(cx, decl)
        || comment_around(cx, stmt);
    let fix = (!blocked).then(|| {
        let name_text = sem.name(sym);
        let prefix = format!("const {name_text} = ");
        let removal = remove_statements(cx, decl, decl, false);
        if parenthesized {
            removal.and(stmt_span, format!("{prefix}{};", paren_text(cx, right)))
        } else {
            removal.and(
                Span::new(cx.span(assign).start, paren_span(cx, right).start),
                prefix,
            )
        }
    });
    let ident_text = cx.text(name).to_owned();
    cx.report(binding_span(cx, name, type_ann), message(&ident_text))
        .id("prefer-smaller-scope")
        .fix_opt(fix);
}

fn variable_problem(cx: &mut Ctx<'_, '_>, decl: NodeId) {
    let ast = cx.ast();
    let Node::VarDecl {
        modifiers,
        kind,
        decls,
    } = *ast.node(decl)
    else {
        return;
    };
    let Some(owner) = cx.parent(decl) else {
        return;
    };
    let &[declarator] = ast.list(decls) else {
        return;
    };
    let Node::VarDeclarator { name, init, .. } = *ast.node(declarator) else {
        return;
    };
    if !matches!(ast.node(owner), Node::Program { .. } | Node::Block { .. })
        || !matches!(kind, VarKind::Let | VarKind::Const)
        || modifiers.intersects(Modifiers::DECLARE | Modifiers::EXPORT)
        || !matches!(ast.node(name), Node::Ident { .. })
    {
        return;
    }
    let sem = cx.semantic();
    let Some(sym) = sem.symbol_of_decl(name) else {
        return;
    };
    if is_comment_exported(cx, sym) || is_exported(cx, owner, cx.text(name)) {
        return;
    }
    let refs: Vec<crate::semantic::RefId> = sem
        .symbol(sym)
        .refs
        .iter()
        .copied()
        .filter(|&r| !sem.reference(r).flags.contains(RefFlags::INIT))
        .collect();
    if let Some(init) = init {
        let nodes: Vec<NodeId> = refs.iter().map(|&r| sem.reference(r).node).collect();
        initialized_problem(cx, decl, declarator, init, &nodes);
    } else if kind == VarKind::Let {
        uninitialized_problem(cx, decl, declarator, sym, &refs);
    }
}

/// Something the alias can't take into the block: a type query, `this` type or a reserved
/// word.
fn has_unmovable_node(ast: &Ast, root: NodeId) -> bool {
    struct Find {
        found: bool,
    }
    impl Visitor for Find {
        fn enter(&mut self, ast: &Ast, id: NodeId) -> Walk {
            let hit = match ast.node(id) {
                Node::TypeQuery { .. } | Node::ThisType {} => true,
                Node::Ident { name } => RESERVED.contains(&ast.atom(*name)),
                _ => false,
            };
            if hit {
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

/// The closest block from the first reference up that holds every reference.
fn type_alias_block(cx: &Ctx<'_, '_>, owner: NodeId, refs: &[NodeId]) -> Option<NodeId> {
    let &first = refs.first()?;
    let mut cur = cx.parent(first);
    for _ in 0..100_000 {
        let c = cur?;
        if c == owner {
            return None;
        }
        if matches!(cx.node(c), Node::Block { .. })
            && refs.iter().all(|&r| is_inside(cx, r, cx.span(c)))
        {
            return Some(c);
        }
        cur = cx.parent(c);
    }
    None
}

fn block_scope(cx: &Ctx<'_, '_>, block: NodeId) -> ScopeId {
    let sem = cx.semantic();
    sem.scope_of_node(block)
        .unwrap_or_else(|| sem.enclosing_scope(block))
}

fn type_alias_problem(cx: &mut Ctx<'_, '_>, decl: NodeId) {
    let ast = cx.ast();
    let Node::TypeAlias {
        modifiers,
        name,
        type_params,
        ..
    } = *ast.node(decl)
    else {
        return;
    };
    let Some(owner) = cx.parent(decl) else {
        return;
    };
    let variance = type_params.is_some_and(|tp| {
        matches!(*ast.node(tp), Node::TypeParams { params } if ast.list(params).iter().any(|&p| {
            matches!(*ast.node(p), Node::TypeParam { modifiers, .. }
                if modifiers.intersects(Modifiers::IN | Modifiers::OUT))
        }))
    });
    let alias_name = cx.text(name).to_owned();
    if !matches!(ast.node(owner), Node::Program { .. } | Node::Block { .. })
        || modifiers.intersects(Modifiers::DECLARE | Modifiers::EXPORT)
        || variance
        || is_exported(cx, owner, &alias_name)
    {
        return;
    }
    let sem = cx.semantic();
    let Some(sym) = sem.symbol_of_decl(name) else {
        return;
    };
    if is_comment_exported(cx, sym) {
        return;
    }
    let decl_span = cx.span(decl);
    let symbol = sem.symbol(sym);
    let refs: Vec<NodeId> = symbol
        .refs
        .iter()
        .map(|&r| sem.reference(r))
        .filter(|r| {
            (r.is_type() || r.flags.contains(RefFlags::EXPORT)) && !is_inside(cx, r.node, decl_span)
        })
        .map(|r| r.node)
        .collect();
    let Some(block) = type_alias_block(cx, owner, &refs) else {
        return;
    };
    let Some(first) = statement_list(ast, block).and_then(|l| {
        l.iter()
            .copied()
            .find(|&s| !matches!(ast.node(s), Node::Directive { .. }))
    }) else {
        return;
    };
    let dest = block_scope(cx, block);
    let unsafe_dependency = sem.references().iter().any(|r| {
        if !is_inside(cx, r.node, decl_span) {
            return false;
        }
        if !r.is_type() || r.flags.contains(RefFlags::TYPEOF_TYPE) {
            return true;
        }
        let resolved = r.symbol();
        let local = resolved.is_some_and(|s| {
            sem.symbol(s)
                .decls
                .first()
                .is_some_and(|d| is_inside(cx, d.name, decl_span))
        });
        !local && sem.lookup(dest, ast.atom(r.name)) != resolved
    });
    let movable = !symbol.is_value()
        && !unsafe_dependency
        && !symbol
            .refs
            .iter()
            .any(|&r| is_inside(cx, sem.reference(r).node, decl_span))
        && sem.lookup(dest, &alias_name) == Some(sym)
        && !has_unmovable_node(ast, decl)
        && !has_multiline_token(cx, decl)
        && !has_comment_inside(cx, decl)
        && !comment_around(cx, decl)
        && !comment_before(cx, cx.span(first).start);
    let fix = movable.then(|| {
        let src = cx.src();
        let first_start = cx.span(first).start;
        let prev_end = skip_trivia_backward(ast, src, first_start as usize);
        let separator = match &src[prev_end..first_start as usize] {
            "" => " ",
            s => s,
        };
        let target = if src[prev_end..first_start as usize].contains(['\n', '\r']) {
            line_indent(src, first_start as usize).to_owned()
        } else {
            format!(
                "{}{}",
                line_indent(src, cx.span(block).start as usize),
                indent_unit(cx)
            )
        };
        let text = reindent_text(
            cx.text(decl),
            line_indent(src, decl_span.start as usize),
            &target,
        );
        let semi = if text.ends_with(';') { "" } else { ";" };
        remove_statements(cx, decl, decl, false).and(
            Span::new(first_start, first_start),
            format!("{text}{semi}{separator}"),
        )
    });
    cx.report(cx.span(name), message(&alias_name))
        .id("prefer-smaller-scope")
        .fix_opt(fix);
}

impl Rule for PreferSmallerScope {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::VarDecl, NodeKind::TypeAlias]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        match cx.node(node) {
            Node::VarDecl { .. } => variable_problem(cx, node),
            Node::TypeAlias { .. } => type_alias_problem(cx, node),
            _ => {}
        }
    }
}
