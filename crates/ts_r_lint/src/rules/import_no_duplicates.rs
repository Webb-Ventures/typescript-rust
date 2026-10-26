//! `import/no-duplicates`: two import declarations of the same module in one module body
//! (the program or a `declare module` block). Namespace imports, `import type` with a
//! default binding, and other type imports (`import type { }` or inline `type`) are
//! grouped apart unless `prefer-inline` is set. Modules are keyed by their specifier as
//! written: the linter does not resolve them, so `./a` and `./a.js` stay different.
//!
//! The first import of a group gets a fix that merges the named specifiers (and a default
//! binding) of the others into it and removes them, skipping imports with comments that
//! would be lost (before the import on the line above it, after it on its last line, or
//! between its tokens outside the braces) and namespace imports; there is no fix when the
//! group has two different default names.

use super::prelude::*;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "import/no-duplicates",
        "Forbid repeated import of the same module in multiple places.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<NoDuplicates>,
};

#[derive(Debug)]
struct NoDuplicates {
    prefer_inline: bool,
}

impl FromOptions for NoDuplicates {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(NoDuplicates {
            prefer_inline: bool_field(obj_opt(opts, 0), "prefer-inline", false),
        })
    }
}

/// The tokens of `span` (comments skipped): identifiers, strings and single punctuators.
fn tokens(src: &str, span: Span) -> Vec<Span> {
    let b = src.as_bytes();
    let end = (span.end as usize).min(b.len());
    let mut out = Vec::new();
    let mut pos = span.start as usize;
    loop {
        pos = skip_trivia_forward(src, pos);
        if pos >= end {
            break;
        }
        let start = pos;
        let c = b[pos];
        if c == b'"' || c == b'\'' {
            pos += 1;
            while pos < end && b[pos] != c {
                pos += if b[pos] == b'\\' { 2 } else { 1 };
            }
            pos = (pos + 1).min(end);
        } else if src[pos..].chars().next().is_some_and(is_ident_char) {
            while pos < end {
                match src[pos..].chars().next() {
                    Some(ch) if is_ident_char(ch) => pos += ch.len_utf8(),
                    _ => break,
                }
            }
        } else {
            pos += src[pos..].chars().next().map_or(1, char::len_utf8);
        }
        out.push(Span::new(start as u32, pos as u32));
    }
    out
}

/// The text between the braces `open` and `close`, cut at its `,` tokens: like ESLint's
/// `split(',')` on that text, except that a comma inside a quoted name (`"a,b" as ab`) or a
/// comment does not cut it, so the merged list stays valid.
fn split_at_commas(src: &str, open: Span, close: Span) -> Vec<&str> {
    let mut out = Vec::new();
    let mut from = open.end;
    for t in tokens(src, Span::new(open.end, close.start)) {
        if t.text(src) == "," {
            out.push(src.get(from as usize..t.start as usize).unwrap_or(""));
            from = t.end;
        }
    }
    out.push(src.get(from as usize..close.start as usize).unwrap_or(""));
    out
}

fn has_specifiers(ast: &Ast, node: NodeId) -> bool {
    specifiers(ast, node)
        .iter()
        .any(|&s| ast.kind(s) == NodeKind::ImportSpecifier)
}

fn has_namespace(ast: &Ast, node: NodeId) -> bool {
    specifiers(ast, node)
        .iter()
        .any(|&s| ast.kind(s) == NodeKind::ImportNamespaceSpecifier)
}

fn specifiers(ast: &Ast, node: NodeId) -> &[NodeId] {
    match *ast.node(node) {
        Node::ImportDecl { specifiers, .. } => ast.list(specifiers),
        _ => &[],
    }
}

fn default_name(ast: &Ast, node: NodeId) -> Option<&str> {
    specifiers(ast, node)
        .iter()
        .find_map(|&s| match *ast.node(s) {
            Node::ImportDefaultSpecifier { local } => ast.name_of(local),
            _ => None,
        })
}

struct Info<'a> {
    ast: &'a Ast,
    src: &'a str,
    lines: &'a ts_r_span::LineIndex,
}

impl Info<'_> {
    /// Comments between the token before `pos` and `pos`.
    fn comments_before(&self, pos: u32) -> impl Iterator<Item = Span> + '_ {
        let prev = skip_trivia_backward(self.ast, self.src, pos as usize) as u32;
        self.ast
            .comments
            .iter()
            .filter(move |c| c.span.start >= prev && c.span.end <= pos)
            .map(|c| c.span)
    }

    /// Comments between `pos` and the next token.
    fn comments_after(&self, pos: u32) -> impl Iterator<Item = Span> + '_ {
        let next = skip_trivia_forward(self.src, pos as usize) as u32;
        self.ast
            .comments
            .iter()
            .filter(move |c| c.span.start >= pos && c.span.end <= next)
            .map(|c| c.span)
    }

    fn problematic_comments(&self, node: NodeId) -> bool {
        let span = self.ast.span(node);
        let start_line = self.lines.line_of(span.start);
        let end_line = self.lines.line_of(span.end);
        if self
            .comments_before(span.start)
            .any(|c| self.lines.line_of(c.end) + 1 >= start_line)
        {
            return true;
        }
        if self
            .comments_after(span.end)
            .any(|c| self.lines.line_of(c.start) == end_line)
        {
            return true;
        }
        let toks = tokens(self.src, span);
        let text = |t: &Span| t.text(self.src);
        let open = toks.iter().position(|t| text(t) == "{");
        let close = toks.iter().position(|t| text(t) == "}");
        let some: Vec<Span> = match (open, close) {
            (Some(o), Some(c)) => toks[1.min(toks.len())..=o]
                .iter()
                .chain(&toks[(c + 1).min(toks.len())..])
                .copied()
                .collect(),
            _ => toks.iter().skip(1).copied().collect(),
        };
        some.iter()
            .any(|t| self.comments_before(t.start).next().is_some())
    }

    /// The braces of an import (first `{` and first `}` token).
    fn braces(&self, node: NodeId) -> Option<(Span, Span)> {
        let toks = tokens(self.src, self.ast.span(node));
        let open = toks.iter().find(|t| t.text(self.src) == "{")?;
        let close = toks.iter().find(|t| t.text(self.src) == "}")?;
        Some((*open, *close))
    }
}

struct Spec {
    node: NodeId,
    identifiers: Vec<String>,
    is_empty: bool,
}

impl NoDuplicates {
    fn fix(&self, info: &Info<'_>, first: NodeId, rest: &[NodeId]) -> Option<Fix> {
        let ast = info.ast;
        let src = info.src;
        if info.problematic_comments(first) || has_namespace(ast, first) {
            return None;
        }
        let mut defaults: Vec<&str> = Vec::new();
        for &n in std::iter::once(&first).chain(rest) {
            if let Some(d) = default_name(ast, n)
                && !defaults.contains(&d)
            {
                defaults.push(d);
            }
        }
        if defaults.len() > 1 {
            return None;
        }
        let clean: Vec<NodeId> = rest
            .iter()
            .copied()
            .filter(|&n| !info.problematic_comments(n) && !has_namespace(ast, n))
            .collect();
        let specs: Vec<Spec> = clean
            .iter()
            .filter_map(|&n| {
                let (open, close) = info.braces(n)?;
                Some(Spec {
                    node: n,
                    identifiers: split_at_commas(src, open, close)
                        .into_iter()
                        .map(str::to_owned)
                        .collect(),
                    is_empty: !has_specifiers(ast, n),
                })
            })
            .collect();
        let unnecessary: Vec<NodeId> = clean
            .iter()
            .copied()
            .filter(|&n| {
                !has_specifiers(ast, n)
                    && !has_namespace(ast, n)
                    && !specs.iter().any(|s| s.node == n)
            })
            .collect();
        let add_default = default_name(ast, first).is_none() && defaults.len() == 1;
        let add_specs = !specs.is_empty();
        if !(add_default || add_specs || !unnecessary.is_empty()) {
            return None;
        }
        let first_span = ast.span(first);
        let toks = tokens(src, first_span);
        let braces = info.braces(first);
        let first_token = toks.first().copied()?;
        let default = defaults.first().copied().unwrap_or("");
        let trailing_comma = braces.is_some_and(|(_, close)| {
            toks.iter()
                .rposition(|t| t.end <= close.start)
                .is_some_and(|i| toks[i].text(src) == ",")
        });
        let first_empty = !has_specifiers(ast, first);
        let mut existing: Vec<String> = match braces {
            Some((open, close)) if !first_empty => split_at_commas(src, open, close)
                .into_iter()
                .map(|x| x.trim().to_owned())
                .collect(),
            _ => Vec::new(),
        };
        let first_existing = existing.clone();
        let mut result = String::new();
        let mut needs_comma = !trailing_comma && !first_empty;
        for spec in &specs {
            let is_type = matches!(
                ast.node(spec.node),
                Node::ImportDecl {
                    type_only: true,
                    ..
                }
            );
            let mut text = String::new();
            for cur in &spec.identifiers {
                let trimmed = cur.trim();
                let with_type = if !trimmed.is_empty() && self.prefer_inline && is_type {
                    format!("type {cur}")
                } else {
                    cur.clone()
                };
                if existing.iter().any(|e| e == trimmed) {
                    continue;
                }
                if text.is_empty() {
                    text = with_type;
                } else {
                    text = format!("{text},{with_type}");
                }
                existing.push(trimmed.to_owned());
            }
            if needs_comma && !spec.is_empty && !text.is_empty() {
                result = format!("{result},{text}");
            } else {
                result.push_str(&text);
            }
            if !spec.is_empty {
                needs_comma = true;
            }
        }
        let mut edits: Vec<(Span, String)> = Vec::new();
        let first_type_only = matches!(
            ast.node(first),
            Node::ImportDecl {
                type_only: true,
                ..
            }
        );
        if add_specs && self.prefer_inline && first_type_only {
            if let Some(t) = toks.iter().find(|t| t.text(src) == "type") {
                edits.push((Span::new(t.start, t.end + 1), String::new()));
            }
            for t in &toks {
                let v = t.text(src);
                if first_existing.iter().any(|e| e == v) {
                    edits.push((*t, format!("type {v}")));
                }
            }
        }
        let after_first = Span::at(first_token.end);
        match braces {
            None if add_default && add_specs => {
                edits.push((after_first, format!(" {default}, {{{result}}} from")));
            }
            None if add_default => edits.push((after_first, format!(" {default} from"))),
            Some((_, close)) if add_default => {
                edits.push((after_first, format!(" {default},")));
                if add_specs {
                    edits.push((Span::at(close.start), result.clone()));
                }
            }
            None if add_specs => match specifiers(ast, first).first() {
                None => edits.push((after_first, format!(" {{{result}}} from"))),
                Some(&s) => edits.push((Span::at(ast.span(s).end), format!(", {{{result}}}"))),
            },
            Some((_, close)) => edits.push((Span::at(close.start), result.clone())),
            None => {}
        }
        for n in specs
            .iter()
            .map(|s| s.node)
            .chain(unnecessary.iter().copied())
        {
            let span = ast.span(n);
            let mut end = span.end;
            if src.as_bytes().get(end as usize) == Some(&b'\n') {
                end += 1;
            }
            edits.push((Span::new(span.start, end), String::new()));
        }
        edits.sort_by_key(|(s, _)| (s.start, s.end));
        let mut fix: Option<Fix> = None;
        for (s, t) in edits {
            fix = Some(match fix {
                None => Fix::replace(s, t),
                Some(f) => f.and(s, t),
            });
        }
        fix
    }
}

/// Which group of a module body an import falls in.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Group {
    Value,
    Namespace,
    DefaultType,
    NamedType,
}

impl Rule for NoDuplicates {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Program, NodeKind::ModuleBlock]
    }
    fn enter(&self, body: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some(stmts) = statement_list(ast, body) else {
            return;
        };
        // (group, specifier) -> imports, in first-seen order.
        let mut groups: Vec<((Group, &str), Vec<NodeId>)> = Vec::new();
        for &n in stmts {
            let Node::ImportDecl {
                type_only, source, ..
            } = *ast.node(n)
            else {
                continue;
            };
            let Some(value) = super::import_util::string_literal(ast, source) else {
                continue;
            };
            let specs = specifiers(ast, n);
            let group = if !self.prefer_inline && type_only {
                if specs
                    .first()
                    .is_some_and(|&s| ast.kind(s) == NodeKind::ImportDefaultSpecifier)
                {
                    Group::DefaultType
                } else {
                    Group::NamedType
                }
            } else if !self.prefer_inline
                && specs.iter().any(|&s| {
                    matches!(
                        ast.node(s),
                        Node::ImportSpecifier {
                            type_only: true,
                            ..
                        }
                    )
                })
            {
                Group::NamedType
            } else if has_namespace(ast, n) {
                Group::Namespace
            } else {
                Group::Value
            };
            match groups.iter_mut().find(|(k, _)| *k == (group, value)) {
                Some((_, v)) => v.push(n),
                None => groups.push(((group, value), vec![n])),
            }
        }
        let info = Info {
            ast,
            src: cx.src(),
            lines: cx.file.lines,
        };
        let mut reports = Vec::new();
        for ((_, value), nodes) in &groups {
            let [first, rest @ ..] = nodes.as_slice() else {
                continue;
            };
            if rest.is_empty() {
                continue;
            }
            let message = format!("'{value}' imported multiple times.");
            let fix = self.fix(&info, *first, rest);
            reports.push((*first, message.clone(), fix));
            for &r in rest {
                reports.push((r, message.clone(), None));
            }
        }
        for (n, message, fix) in reports {
            let Node::ImportDecl { source, .. } = *ast.node(n) else {
                continue;
            };
            let span = cx.span(source);
            cx.report(span, message).fix_opt(fix);
        }
    }
}
