//! `unicorn/no-anonymous-default-export`: an anonymous function, class or arrow function
//! as `export default`, `module.exports = ` or `exports = ` (as an expression statement).
//!
//! The report covers the function head (or the class head: its first decorator or `class`
//! through the token before the body). The suggestion names it after the file: the base name before its first `.`,
//! camel-cased (PascalCase for classes), made unique in the node's scope and the scopes
//! inside it. A function gets the name before its `(`, a class after `class`; an arrow
//! function becomes `const name = <arrow>;` with the export statement re-emitted on the
//! next line (only where a statement list holds it).

use super::prelude::*;
use super::unicorn_text::paren_span;
use super::unicorn_util::{available_name, is_identifier_name, line_indent, scope_within};
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-anonymous-default-export",
        "Disallow anonymous functions and classes as the default export.",
        NO_PRESET,
        Category::Style,
    )
    .suggestions(),
    create: create_default::<NoAnonymousDefaultExport>,
};

#[derive(Debug, Default)]
struct NoAnonymousDefaultExport;

/// change-case's `camelCase`: words split at case changes and non-alphanumerics; the first
/// lower-cased, the others capitalized (a word starting with a digit gets a `_`).
pub(crate) fn camel_case(input: &str) -> String {
    let chars: Vec<char> = input.trim().chars().collect();
    let mut words: Vec<String> = Vec::new();
    let mut cur = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if !(c.is_alphabetic() || c.is_ascii_digit()) {
            if !cur.is_empty() {
                words.push(std::mem::take(&mut cur));
            }
            continue;
        }
        if let Some(&prev) = i.checked_sub(1).and_then(|j| chars.get(j))
            && !cur.is_empty()
        {
            let lower_upper = (prev.is_lowercase() || prev.is_ascii_digit()) && c.is_uppercase();
            let upper_upper = prev.is_uppercase()
                && c.is_uppercase()
                && chars.get(i + 1).is_some_and(|n| n.is_lowercase());
            if lower_upper || upper_upper {
                words.push(std::mem::take(&mut cur));
            }
        }
        cur.push(c);
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    let mut out = String::new();
    for (i, w) in words.iter().enumerate() {
        if i == 0 {
            out.push_str(&w.to_lowercase());
            continue;
        }
        let mut cs = w.chars();
        if let Some(first) = cs.next() {
            if first.is_ascii_digit() {
                out.push('_');
                out.push(first);
            } else {
                out.extend(first.to_uppercase());
            }
            out.push_str(&cs.as_str().to_lowercase());
        }
    }
    out
}

fn upper_first(s: &str) -> String {
    let mut cs = s.chars();
    match cs.next() {
        Some(c) => c.to_uppercase().chain(cs).collect(),
        None => String::new(),
    }
}

fn is_class(ast: &Ast, id: NodeId) -> bool {
    matches!(ast.kind(id), NodeKind::ClassDecl | NodeKind::ClassExpr)
}

/// The position of the `class` keyword of a class node.
fn class_keyword(cx: &Ctx<'_, '_>, class: NodeId) -> Option<u32> {
    let ast = cx.ast();
    let src = cx.src();
    let (Node::ClassDecl { decorators, .. } | Node::ClassExpr { decorators, .. }) =
        *ast.node(class)
    else {
        return None;
    };
    let span = cx.span(class);
    let mut pos = ast
        .list(decorators)
        .last()
        .map_or(span.start, |&d| cx.span(d).end) as usize;
    for _ in 0..16 {
        pos = skip_trivia_forward(src, pos);
        if pos >= span.end as usize {
            return None;
        }
        let word: String = src[pos..]
            .chars()
            .take_while(|&c| is_ident_char(c))
            .collect();
        if word == "class" {
            return Some(pos as u32);
        }
        if word.is_empty() {
            pos += 1;
        } else {
            pos += word.len();
        }
    }
    None
}

/// `class` through the token before the class body.
fn class_head(cx: &Ctx<'_, '_>, class: NodeId) -> Span {
    let ast = cx.ast();
    let src = cx.src();
    let span = cx.span(class);
    let Some(start) = class_keyword(cx, class) else {
        return span;
    };
    let (Node::ClassDecl {
        decorators,
        name,
        type_params,
        heritage,
        ..
    }
    | Node::ClassExpr {
        decorators,
        name,
        type_params,
        heritage,
        ..
    }) = *ast.node(class)
    else {
        return span;
    };
    // The class node starts at its first decorator, unless the decorators come before
    // `export` (then the export declaration holds them).
    let head_start = match ast.list(decorators).first() {
        Some(&d) if !src[cx.span(d).end as usize..start as usize].contains("export") => {
            cx.span(d).start.min(start)
        }
        _ => start,
    };
    let mut after = start + "class".len() as u32;
    for id in name
        .into_iter()
        .chain(type_params)
        .chain(ast.list(heritage).iter().copied())
    {
        after = after.max(cx.span(id).end);
    }
    let Some(open) = find_char(src, after, b'{') else {
        return span;
    };
    let end = skip_trivia_backward(ast, src, open as usize) as u32;
    Span::new(head_start, end)
}

/// The first `(` of a function (its parameter list).
fn open_paren(cx: &Ctx<'_, '_>, f: NodeId) -> Option<u32> {
    let src = cx.src();
    let span = cx.span(f);
    let mut pos = span.start as usize;
    while pos < span.end as usize {
        pos = skip_trivia_forward(src, pos);
        match src.as_bytes().get(pos) {
            Some(b'(') => return Some(pos as u32),
            Some(_) => pos += src[pos..].chars().next().map_or(1, char::len_utf8),
            None => return None,
        }
    }
    None
}

impl NoAnonymousDefaultExport {
    fn suggestion_name(&self, cx: &Ctx<'_, '_>, node: NodeId) -> Option<String> {
        let path = cx.file.path.replace('\\', "/");
        let base = path.rsplit('/').next().unwrap_or("");
        if base == "<input>" || base == "<text>" {
            return None;
        }
        let first = base.split('.').next().unwrap_or("");
        let mut name = camel_case(first);
        if !is_identifier_name(&name) {
            return None;
        }
        if is_class(cx.ast(), node) {
            name = upper_first(&name);
        }
        let sem = cx.semantic();
        let root = sem
            .scope_of_node(node)
            .unwrap_or_else(|| sem.enclosing_scope(node));
        let scopes: Vec<ScopeId> = (0..sem.scopes().len() as u32)
            .map(ScopeId)
            .filter(|&s| scope_within(sem, s, root))
            .collect();
        available_name(cx, &name, &scopes, &|_| true)
    }

    fn problem(&self, cx: &mut Ctx<'_, '_>, node: NodeId, statement: NodeId) {
        let ast = cx.ast();
        let src = cx.src();
        let (span, description) = if is_class(ast, node) {
            (class_head(cx, node), "class".to_owned())
        } else {
            let head = function_head_span(ast, src, cx.file.parents, node);
            let kind = function_name_with_kind(ast, src, cx.file.parents, node);
            let kind = match kind.find(" '") {
                Some(i) if kind.ends_with('\'') => kind[..i].to_owned(),
                _ => kind,
            };
            (head, kind)
        };
        let message = format!("The {description} should be named.");
        let name = self.suggestion_name(cx, node);
        let is_arrow = ast.kind(node) == NodeKind::Arrow;
        let in_list = cx
            .parent(statement)
            .is_some_and(|p| statement_list(ast, p).is_some());
        let fix = match &name {
            Some(name) if !is_arrow || in_list => self.add_name(cx, node, statement, name),
            _ => None,
        };
        let r = cx
            .report(span, message)
            .id("no-anonymous-default-export/error");
        if let (Some(name), Some(fix)) = (name, fix) {
            r.suggest(
                "no-anonymous-default-export/suggestion",
                format!("Name it as `{name}`."),
                fix,
            );
        }
    }

    fn add_name(
        &self,
        cx: &Ctx<'_, '_>,
        node: NodeId,
        statement: NodeId,
        name: &str,
    ) -> Option<Fix> {
        let ast = cx.ast();
        let src = cx.src();
        match ast.kind(node) {
            NodeKind::ClassDecl | NodeKind::ClassExpr => {
                let kw = class_keyword(cx, node)?;
                Some(Fix::insert(kw + "class".len() as u32, format!(" {name}")))
            }
            NodeKind::FunctionDecl | NodeKind::FunctionExpr => {
                let paren = open_paren(cx, node)?;
                let before = src[..paren as usize].chars().next_back();
                let space = if before == Some(' ') { "" } else { " " };
                Some(Fix::insert(paren, format!("{space}{name} ")))
            }
            NodeKind::Arrow => {
                let stmt = cx.span(statement);
                let arrow = paren_span(cx, node);
                let mut before = format!(
                    "{}{}{}",
                    super::unicorn_ast::linebreak(src),
                    line_indent(src, stmt.start as usize),
                    &src[stmt.start as usize..arrow.start as usize]
                );
                if !before.ends_with(char::is_whitespace) {
                    before.push(' ');
                }
                let mut after = src[arrow.end as usize..stmt.end as usize].to_owned();
                if !after.ends_with(';') {
                    after.push(';');
                }
                Some(
                    Fix::replace(
                        Span::new(stmt.start, arrow.start),
                        format!("const {name} = "),
                    )
                    .and(Span::new(arrow.end, stmt.end), ";")
                    .and(Span::at(stmt.end), format!("{before}{name}{after}")),
                )
            }
            _ => None,
        }
    }
}

fn is_anonymous(ast: &Ast, id: NodeId) -> bool {
    match *ast.node(id) {
        Node::FunctionDecl { name, body, .. } => name.is_none() && body.is_some(),
        Node::FunctionExpr { name, .. }
        | Node::ClassDecl { name, .. }
        | Node::ClassExpr { name, .. } => name.is_none(),
        Node::Arrow { .. } => true,
        _ => false,
    }
}

impl Rule for NoAnonymousDefaultExport {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::FunctionDecl,
            NodeKind::ClassDecl,
            NodeKind::ExportDefaultExpr,
            NodeKind::Assign,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        match *cx.node(node) {
            Node::FunctionDecl { modifiers, .. } | Node::ClassDecl { modifiers, .. }
                if modifiers.contains(Modifiers::DEFAULT) && is_anonymous(ast, node) =>
            {
                self.problem(cx, node, node);
            }
            Node::ExportDefaultExpr { expr } => {
                let expr = skip_parens(ast, expr);
                if is_anonymous(ast, expr) {
                    self.problem(cx, expr, node);
                }
            }
            Node::Assign { left, right, .. } => {
                let right = skip_parens(ast, right);
                if !is_anonymous(ast, right) {
                    return;
                }
                let Some(stmt) = parent_skip_parens(ast, cx.file.parents, node) else {
                    return;
                };
                if ast.kind(stmt) != NodeKind::ExprStmt {
                    return;
                }
                let target = match *ast.node(left) {
                    Node::Member {
                        obj,
                        optional: false,
                        prop,
                    } => {
                        ident_name(ast, obj) == Some("module")
                            && ident_name(ast, prop) == Some("exports")
                    }
                    Node::Ident { .. } => ident_name(ast, left) == Some("exports"),
                    _ => false,
                };
                if target {
                    self.problem(cx, right, stmt);
                }
            }
            _ => {}
        }
    }
}
