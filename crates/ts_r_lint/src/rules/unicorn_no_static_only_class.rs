//! `unicorn/no-static-only-class`: a class whose members are all plain static fields and
//! methods (no `extends`, decorators, private names, accessibility, `declare`, `readonly`,
//! `override` or `accessor`) should be an object. The class head is reported (from the
//! `class` node's start to the token before `{`).
//!
//! The fix (none for `declare`/`abstract`/`implements` classes, named class expressions,
//! named default exports, typed fields, field initializers whose text mentions `this`,
//! `super` or `new.target`, or members referring to the class's own binding): a
//! declaration becomes `const A = {...};`; an expression or anonymous default export
//! drops `class` (`(` for a concise arrow body, closed after the body; after `return` with
//! a comment before a `{` on the next line, `class` becomes `{` and the body's `{` goes);
//! each member loses `static`, `=` becomes `:`, a field without a value gets `: undefined`,
//! and the `;` after the member becomes `,` (or `,` is added). A default export followed by
//! code that would continue the expression gets a `;`.

use super::prelude::*;
use super::unicorn_util_h4::{keyword_argument_parent, next_token_start, prev_token_end};
use ts_r_ast::{HeritageKind, Modifiers, NodeList};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-static-only-class",
        "Disallow classes that only have static members.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<NoStaticOnlyClass>,
};

#[derive(Debug, Default)]
struct NoStaticOnlyClass;

const NOT_ON_OBJECTS: &[Modifiers] = &[
    Modifiers::DECLARE,
    Modifiers::READONLY,
    Modifiers::OVERRIDE,
    Modifiers::PUBLIC,
    Modifiers::PRIVATE,
    Modifiers::PROTECTED,
    Modifiers::ACCESSOR,
    Modifiers::ABSTRACT,
];

fn is_static_member(cx: &Ctx<'_, '_>, member: NodeId) -> bool {
    let (decorators, modifiers, key) = match *cx.node(member) {
        Node::PropertyDef {
            decorators,
            modifiers,
            key,
            ..
        }
        | Node::MethodDef {
            decorators,
            modifiers,
            key,
            ..
        } => (decorators, modifiers, key),
        _ => return false,
    };
    modifiers.contains(Modifiers::STATIC)
        && !NOT_ON_OBJECTS.iter().any(|&m| modifiers.contains(m))
        && cx.ast().list(decorators).is_empty()
        && !matches!(cx.node(key), Node::PrivateName { .. })
}

/// Skips the word `word` and the trivia after it at `pos`.
fn skip_word(src: &str, pos: u32, word: &str) -> Option<u32> {
    let rest = &src[pos as usize..];
    let after = rest.strip_prefix(word)?;
    if after.starts_with(is_ident_char) {
        return None;
    }
    Some(skip_trivia_forward(src, pos as usize + word.len()) as u32)
}

struct Class {
    expression: bool,
    modifiers: Modifiers,
    name: Option<NodeId>,
    implements: bool,
    members: NodeList,
    /// Start of the ESTree class node (after `export`/`export default`).
    start: u32,
    /// The `class` keyword.
    keyword: u32,
    /// The body's `{`.
    open: u32,
    end: u32,
}

fn class_of(cx: &Ctx<'_, '_>, node: NodeId) -> Option<Class> {
    let ast = cx.ast();
    let src = cx.src();
    let (expression, decorators, modifiers, name, type_params, heritage, members) =
        match *cx.node(node) {
            Node::ClassDecl {
                decorators,
                modifiers,
                name,
                type_params,
                heritage,
                members,
            } => (
                false,
                decorators,
                modifiers,
                name,
                type_params,
                heritage,
                members,
            ),
            Node::ClassExpr {
                decorators,
                modifiers,
                name,
                type_params,
                heritage,
                members,
            } => (
                true,
                decorators,
                modifiers,
                name,
                type_params,
                heritage,
                members,
            ),
            _ => return None,
        };
    if !ast.list(decorators).is_empty() {
        return None;
    }
    let mut implements = false;
    let mut head_end = None;
    for &h in ast.list(heritage) {
        match *ast.node(h) {
            Node::HeritageClause {
                kind: HeritageKind::Extends,
                ..
            } => return None,
            Node::HeritageClause { .. } => implements = true,
            _ => {}
        }
        head_end = Some(cx.span(h).end);
    }
    let span = cx.span(node);
    let mut start = span.start;
    for word in ["export", "default"] {
        if let Some(p) = skip_word(src, start, word) {
            start = p;
        }
    }
    let mut keyword = start;
    for word in ["declare", "abstract"] {
        if let Some(p) = skip_word(src, keyword, word) {
            keyword = p;
        }
    }
    skip_word(src, keyword, "class")?;
    let head_end = head_end
        .or(type_params.map(|t| cx.span(t).end))
        .or(name.map(|n| cx.span(n).end))
        .unwrap_or(keyword + 5);
    let open = next_token_start(cx, head_end);
    if src.as_bytes().get(open as usize) != Some(&b'{') {
        return None;
    }
    Some(Class {
        expression,
        modifiers,
        name,
        implements,
        members,
        start,
        keyword,
        open,
        end: span.end,
    })
}

/// A reference to the class's own binding inside `range`.
fn references_class(cx: &Ctx<'_, '_>, name: Option<NodeId>, range: Span) -> bool {
    let Some(name) = name else {
        return false;
    };
    let sem = cx.semantic();
    let Some(sym) = sem.symbol_of_decl(name) else {
        return false;
    };
    sem.symbol(sym).refs.iter().any(|&r| {
        let s = cx.span(sem.reference(r).node);
        s.start >= range.start && s.end <= range.end
    })
}

/// The `;` ending `member`, if any: the last token of a field, the token after a method.
fn member_semicolon(cx: &Ctx<'_, '_>, member: NodeId) -> Option<u32> {
    let src = cx.src();
    let span = cx.span(member);
    if matches!(cx.node(member), Node::PropertyDef { .. }) {
        return src[..span.end as usize]
            .ends_with(';')
            .then(|| span.end - 1);
    }
    let next = next_token_start(cx, span.end);
    (src.as_bytes().get(next as usize) == Some(&b';')).then_some(next)
}

fn member_edits(cx: &Ctx<'_, '_>, member: NodeId, edits: &mut Vec<(Span, String)>) -> Option<()> {
    let src = cx.src();
    let span = cx.span(member);
    let st = span.start;
    skip_word(src, st, "static")?;
    let rest = &src[st as usize + 6..];
    let ws = rest.len() - rest.trim_start().len();
    edits.push((Span::new(st, st + 6 + ws as u32), String::new()));
    let semicolon = member_semicolon(cx, member);
    // ESTree ends a field without `;` at its last token, before any comment.
    let node_end = match semicolon {
        Some(_) => span.end,
        None => prev_token_end(cx, span.end),
    };
    let mut tail = String::new();
    if let Node::PropertyDef { key, value, .. } = *cx.node(member) {
        if value.is_some() {
            let mut pos = cx.span(key).end as usize;
            loop {
                pos = skip_trivia_forward(src, pos);
                match src.as_bytes().get(pos) {
                    Some(b'=') => break,
                    Some(_) => pos += 1,
                    None => return None,
                }
            }
            edits.push((Span::new(pos as u32, pos as u32 + 1), ":".to_owned()));
        } else if let Some(s) = semicolon {
            edits.push((Span::at(s), ": undefined".to_owned()));
        } else {
            tail.push_str(": undefined");
        }
    }
    match semicolon {
        Some(s) => edits.push((Span::new(s, s + 1), ",".to_owned())),
        None => {
            tail.push(',');
            edits.push((Span::at(node_end), tail));
        }
    }
    Some(())
}

fn fix(cx: &Ctx<'_, '_>, node: NodeId, c: &Class) -> Option<Fix> {
    let ast = cx.ast();
    let src = cx.src();
    if c.modifiers.contains(Modifiers::DECLARE)
        || c.modifiers.contains(Modifiers::ABSTRACT)
        || c.implements
        || (c.expression && c.name.is_some())
    {
        return None;
    }
    let export_default = !c.expression && c.modifiers.contains(Modifiers::DEFAULT);
    if export_default && c.name.is_some() {
        return None;
    }
    for &m in ast.list(c.members) {
        let (key, value, typed, field) = match *ast.node(m) {
            Node::PropertyDef {
                key,
                value,
                type_ann,
                ..
            } => (key, value, type_ann.is_some(), true),
            Node::MethodDef { key, value, .. } => (key, Some(value), false, false),
            _ => return None,
        };
        let key_range = match *ast.node(key) {
            Node::ComputedKey { expr } => cx.span(expr),
            _ => cx.span(key),
        };
        if references_class(cx, c.name, key_range) {
            return None;
        }
        if field {
            let mentions = value.is_some_and(|v| {
                let t = cx.text(v);
                t.contains("this") || t.contains("super") || t.contains("new.target")
            });
            if typed || mentions || references_class(cx, c.name, cx.span(m)) {
                return None;
            }
        }
    }
    let mut edits = Vec::new();
    let kw = Span::new(c.keyword, c.keyword + 5);
    if export_default || c.expression {
        let before = prev_token_end(cx, c.keyword);
        let concise = src[..before as usize].ends_with("=>");
        let parent = cx.parent(node);
        let lines = cx.file.lines;
        if c.expression
            && keyword_argument_parent(cx, node).is_some()
            && parent.is_some_and(|p| lines.line_of(c.open) != lines.line_of(cx.span(p).start))
            && !src[kw.end as usize..c.open as usize].trim().is_empty()
        {
            edits.push((kw, "{".to_owned()));
            edits.push((Span::new(c.open, c.open + 1), String::new()));
        } else {
            let rest = &src[kw.end as usize..];
            let ws = rest.len() - rest.trim_start().len();
            edits.push((
                Span::new(kw.start, kw.end + ws as u32),
                if concise { "(" } else { "" }.to_owned(),
            ));
        }
        if concise {
            edits.push((Span::at(c.end), ")".to_owned()));
        }
        if export_default {
            let next = next_token_start(cx, c.end) as usize;
            if src[next..].starts_with(['[', '(', '/', '`', '+', '-', '*', ',', '.', '<']) {
                edits.push((Span::at(c.end), ";".to_owned()));
            }
        }
    } else {
        edits.push((kw, "const".to_owned()));
        edits.push((Span::at(c.open), "= ".to_owned()));
        edits.push((Span::at(c.end), ";".to_owned()));
    }
    for &m in ast.list(c.members) {
        member_edits(cx, m, &mut edits)?;
    }
    Some(super::unicorn_util_h4::fix_of(edits))
}

impl Rule for NoStaticOnlyClass {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::ClassDecl, NodeKind::ClassExpr]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Some(class) = class_of(cx, node) else {
            return;
        };
        let members = cx.ast().list(class.members);
        if members.is_empty() || !members.iter().all(|&m| is_static_member(cx, m)) {
            return;
        }
        let head_end = prev_token_end(cx, class.open);
        let fix = fix(cx, node, &class);
        cx.report(
            Span::new(class.start, head_end),
            "Use an object instead of a class with only static members.",
        )
        .id("no-static-only-class")
        .fix_opt(fix);
    }
}
