//! Organize imports (`source.organizeImports`): remove unused imports, merge imports of the
//! same module, sort declarations and specifiers. Rules observed from tsserver 6.0.3 on the
//! `organize-*` fixtures:
//!
//! - Top-level import declarations form groups; an import starts a new group when a blank
//!   line or an own-line comment precedes it. Non-import statements between imports do not
//!   split a group: the group's imports are gathered where its first one was. Export
//!   declarations (`export {..} [from]`, `export *`) are grouped and organized the same way,
//!   separately from imports.
//! - Declarations sort by module specifier: bare specifiers before relative ones (`.`), then
//!   case-insensitively (lowercase) unless every group is already sorted ordinally and not
//!   case-insensitively; ties keep their order. Specifiers inside import braces sort by their
//!   (imported) name the same way; `type` specifiers go last unless the import lists put
//!   them first (or inline). Detection is per file, from the imports only: export braces
//!   always sort ordinally (`B, a`) with `type` specifiers last.
//! - Merging, per module specifier: one side-effect import is kept; type-only and regular
//!   imports stay apart; namespace imports stay separate declarations; default and named
//!   imports join (`def, { a }`); several defaults become `default as x` specifiers.
//!   Duplicate specifiers are dropped. Order per module: side-effect, type-only, namespace,
//!   default/named. Exports: named, then type-only named, then `export *`.
//! - Unused import bindings are removed (a declaration with none left goes away; a
//!   side-effect import stays). A binding is used when an identifier of the file resolves to
//!   it, when it names the classic JSX factory in a file with JSX, or when a `{@link}` names it.
//! - A changed group is printed again: `import { a, b } from "x";` (the specifier's quotes as
//!   written, a semicolon when the file uses them ([`super::style::uses_semicolons`]), braces
//!   on several lines when the first source declaration had them so, with its indentation; a
//!   trailing comma there survives in imports whose bindings were only reordered, not in
//!   reprinted exports). A reprinted multi-line export list breaks a line only between two
//!   specifiers that were neighbours, in that order and on separate lines, in one source
//!   list (`DISPLAY_SIZES, sharpImageProcessor, type DisplaySize,` then one per line); its
//!   braces open onto their own lines when such a break is printed, or when the first
//!   specifier comes from the declaration printed in place with a line break before it
//!   there (`organize-export-merge-*`: `{ Gamma, alpha, beta, zeta }` from two multi-line
//!   lists). Merged lists take the indentation of the first multi-line source list. A
//!   declaration's same-line comments go with it; a merged
//!   declaration keeps those of its first source declaration.
//! - An unchanged group is printed again too, so spacing becomes the printer's (`{a}` →
//!   `{ a }`) and semicolons the file's; only declarations whose text differs are edited,
//!   and one with a comment inside only gets its semicolon added or removed.

use super::Ctx;
use super::edit::{Edit, FileEdits};
use std::cmp::Ordering;
use std::collections::HashSet;
use ts_r_ast::{Ast, Node, NodeId, Span};
use ts_r_binder::{BoundFile, SymbolId};
use ts_r_config::JsxEmit;

/// tsserver's `organizeImports` modes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Remove unused, merge and sort.
    All,
    /// Merge and sort only.
    SortAndCombine,
    /// Remove unused only.
    RemoveUnused,
}

/// A binding an import declares: its specifier node and local name node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Binding {
    spec: NodeId,
    local: NodeId,
}

/// One specifier inside braces.
#[derive(Clone, Debug)]
struct Item {
    /// Imported (or exported-from) name as written.
    name: String,
    /// `as` alias as written.
    alias: Option<String>,
    type_only: bool,
    binding: Option<Binding>,
    /// Export specifiers: the declaration and position the specifier comes from, and whether
    /// a line break separated it from the one before there (for [`Decl::print`]'s layout).
    at: Option<(NodeId, u32)>,
    nl_before: bool,
}

impl Item {
    fn print(&self) -> String {
        let mut s = String::new();
        if self.type_only {
            s.push_str("type ");
        }
        s.push_str(&self.name);
        if let Some(a) = &self.alias {
            s.push_str(" as ");
            s.push_str(a);
        }
        s
    }

    fn same(&self, o: &Item) -> bool {
        self.name == o.name && self.alias == o.alias && self.type_only == o.type_only
    }

    fn local_name(&self) -> &str {
        self.alias.as_deref().unwrap_or(&self.name)
    }
}

/// A source import or export declaration, taken apart.
#[derive(Clone, Debug)]
struct Decl {
    node: NodeId,
    export: bool,
    type_only: bool,
    /// Module specifier: as written (with quotes) and its value. `None`: `export { a }`.
    spec_raw: Option<String>,
    spec: Option<String>,
    attrs: String,
    /// `import x, ...`: default binding.
    default: Option<(String, Option<Binding>)>,
    /// `* as ns` (imports), `export * [as ns]` (name empty without `as`).
    namespace: Option<(String, Option<Binding>)>,
    /// `{ ... }` when present.
    named: Option<Vec<Item>>,
    /// Indentation of a multi-line brace list.
    multiline: Option<String>,
    /// The brace list ends with a comma.
    trailing_comma: bool,
    /// Same-line comments before and after.
    lead: String,
    trail: String,
    /// Removable extent: whole lines when the declaration has its lines to itself.
    line_start: u32,
    line_end: u32,
    /// The declaration's own span, and whether a comment lies inside it (an unchanged
    /// declaration with one only gets its semicolon fixed, see [`group_edits`]).
    span: Span,
    inner_comment: bool,
}

impl Decl {
    fn is_side_effect(&self) -> bool {
        !self.export && self.default.is_none() && self.namespace.is_none() && self.named.is_none()
    }

    /// The declaration as text. A trailing comma in a multi-line list survives only when
    /// the list is unchanged (`keep_comma`).
    fn print(&self, nl: &str, semi: bool, keep_comma: bool) -> String {
        let mut s = String::new();
        s.push_str(&self.lead);
        s.push_str(if self.export { "export " } else { "import " });
        if self.type_only {
            s.push_str("type ");
        }
        let mut clause = false;
        if let Some((name, _)) = &self.default {
            s.push_str(name);
            clause = true;
        }
        if let Some((name, _)) = &self.namespace {
            if clause {
                s.push_str(", ");
            }
            s.push('*');
            if !name.is_empty() {
                s.push_str(" as ");
                s.push_str(name);
            }
            clause = true;
        }
        if let Some(items) = &self.named {
            if clause {
                s.push_str(", ");
            }
            match (&self.multiline, items.is_empty()) {
                (_, true) => s.push_str("{}"),
                (Some(indent), false) if self.export => {
                    // Reprinted exports: a line break only between specifiers that were
                    // neighbours, in this order and on separate lines, in one source list.
                    // The braces open onto their own lines when one of those breaks is
                    // printed, or when the first specifier comes from the declaration
                    // printed in place and had a line break before it there.
                    let breaks: Vec<bool> = items
                        .windows(2)
                        .map(|w| match (w[0].at, w[1].at) {
                            (Some((a, x)), Some((b, y))) => a == b && x + 1 == y && w[1].nl_before,
                            _ => false,
                        })
                        .collect();
                    let open = breaks.contains(&true)
                        || (items[0].nl_before && items[0].at.is_some_and(|(n, _)| n == self.node));
                    s.push('{');
                    if open {
                        s.push_str(nl);
                        s.push_str(indent);
                    } else {
                        s.push(' ');
                    }
                    for (i, it) in items.iter().enumerate() {
                        if i > 0 {
                            if breaks[i - 1] {
                                s.push(',');
                                s.push_str(nl);
                                s.push_str(indent);
                            } else {
                                s.push_str(", ");
                            }
                        }
                        s.push_str(&it.print());
                    }
                    if open {
                        s.push_str(nl);
                    } else {
                        s.push(' ');
                    }
                    s.push('}');
                }
                (Some(indent), false) => {
                    s.push('{');
                    s.push_str(nl);
                    let parts: Vec<String> = items
                        .iter()
                        .map(|i| format!("{indent}{}", i.print()))
                        .collect();
                    s.push_str(&parts.join(&format!(",{nl}")));
                    if self.trailing_comma && keep_comma {
                        s.push(',');
                    }
                    s.push_str(nl);
                    s.push('}');
                }
                (None, false) => {
                    let parts: Vec<String> = items.iter().map(Item::print).collect();
                    s.push_str("{ ");
                    s.push_str(&parts.join(", "));
                    s.push_str(" }");
                }
            }
            clause = true;
        }
        if let Some(raw) = &self.spec_raw {
            if clause {
                s.push_str(" from ");
            }
            s.push_str(raw);
        }
        s.push_str(&self.attrs);
        if semi {
            s.push(';');
        }
        s.push_str(&self.trail);
        s
    }
}

/// An output declaration and the source declarations it comes from (in source order).
#[derive(Clone, Debug)]
struct Out {
    d: Decl,
    from: Vec<NodeId>,
    /// Bindings or specifiers were dropped, merged or reordered.
    changed: bool,
    /// Bindings were dropped or declarations merged (not just reordered).
    reshaped: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Case {
    Insensitive,
    Ordinal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TypeOrder {
    Last,
    Inline,
    First,
}

pub(super) fn cmp_str(a: &str, b: &str, case: Case) -> Ordering {
    match case {
        Case::Ordinal => a.cmp(b),
        Case::Insensitive => {
            let la = a.chars().flat_map(char::to_lowercase);
            let lb = b.chars().flat_map(char::to_lowercase);
            la.cmp(lb)
        }
    }
}

fn is_relative(s: &str) -> bool {
    s.starts_with('.')
}

pub(super) fn cmp_spec(a: Option<&str>, b: Option<&str>, case: Case) -> Ordering {
    match (a, b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(a), Some(b)) => is_relative(a)
            .cmp(&is_relative(b))
            .then_with(|| cmp_str(a, b, case)),
    }
}

fn cmp_item(a: &Item, b: &Item, case: Case, order: TypeOrder) -> Ordering {
    let t = match order {
        TypeOrder::Last => a.type_only.cmp(&b.type_only),
        TypeOrder::First => b.type_only.cmp(&a.type_only),
        TypeOrder::Inline => Ordering::Equal,
    };
    t.then_with(|| cmp_str(a.local_name(), b.local_name(), case))
}

fn is_sorted_by<T>(xs: &[T], mut f: impl FnMut(&T, &T) -> Ordering) -> bool {
    xs.windows(2).all(|w| f(&w[0], &w[1]) != Ordering::Greater)
}

/// Edits organizing the imports and exports of the request's file.
pub fn organize(cx: &Ctx<'_, '_>, mode: Mode) -> FileEdits {
    let sf = cx.checker.file(cx.file);
    let ast = &sf.ast;
    let text = sf.text.as_str();
    let Node::Program { stmts } = *ast.node(ast.root) else {
        return Vec::new();
    };
    let stmts = ast.list(stmts);
    let mut imports: Vec<Vec<Decl>> = Vec::new();
    let mut exports: Vec<Vec<Decl>> = Vec::new();
    let mut prev_end = None;
    for &s in stmts {
        let start = ast.span(s).start;
        let split = prev_end.is_some_and(|p| starts_group(ast, text, p, start));
        prev_end = Some(ast.span(s).end);
        let Some(d) = decl(ast, text, s) else {
            continue;
        };
        let list = if d.export { &mut exports } else { &mut imports };
        match list.last_mut() {
            Some(g) if !split => g.push(d),
            _ => list.push(vec![d]),
        }
    }
    if imports.is_empty() && exports.is_empty() {
        return Vec::new();
    }
    let usage = matches!(mode, Mode::All | Mode::RemoveUnused)
        .then(|| Usage::new(cx, ast, text, &sf.bound, stmts));
    let spec_case = detect_spec_case(&imports);
    // Detected from the import lists; export lists always sort ordinally, types last.
    let (item_case, type_order) = detect_item_order(imports.iter().flatten());
    let semi = super::style::uses_semicolons(ast, text);
    let mut edits = Vec::new();
    for (groups, is_export) in [(&imports, false), (&exports, true)] {
        for g in groups {
            let mut out: Vec<Out> = Vec::with_capacity(g.len());
            for d in g {
                let pruned = match (&usage, is_export) {
                    (Some(u), false) => u.prune(d),
                    _ => Some(d.clone()),
                };
                if let Some(p) = pruned {
                    let changed = !same_bindings(d, &p);
                    out.push(Out {
                        from: vec![p.node],
                        d: p,
                        changed,
                        reshaped: changed,
                    });
                }
            }
            if mode != Mode::RemoveUnused {
                out = combine(out, spec_case, item_case, type_order);
            }
            group_edits(text, semi, is_export, g, &out, &mut edits);
        }
    }
    edits
}

/// Whether the trivia between `from` and `to` starts a new group: a blank line or an
/// own-line comment.
fn starts_group(ast: &Ast, text: &str, from: u32, to: u32) -> bool {
    let trivia = text.get(from as usize..to as usize).unwrap_or("");
    let mut seen_nl = false;
    let mut only_ws = true;
    for ch in trivia.chars() {
        if ch == '\n' {
            if seen_nl && only_ws {
                return true;
            }
            seen_nl = true;
            only_ws = true;
        } else if !ch.is_whitespace() {
            only_ws = false;
        }
    }
    ast.comments_in(Span::new(from, to))
        .iter()
        .any(|c| c.newline_before)
}

/// Takes an import or export declaration apart.
fn decl(ast: &Ast, text: &str, s: NodeId) -> Option<Decl> {
    let src = |n: NodeId| ast.span(n).text(text).to_string();
    let spec_of = |n: NodeId| match *ast.node(n) {
        Node::StringLit { value } => Some((src(n), ast.atom(value).to_string())),
        _ => None,
    };
    let span = ast.span(s);
    let (line_start, lead) = line_lead(ast, text, span.start);
    let (line_end, trail) = line_trail(ast, text, span.end);
    let mut d = Decl {
        node: s,
        export: false,
        type_only: false,
        spec_raw: None,
        spec: None,
        attrs: String::new(),
        default: None,
        namespace: None,
        named: None,
        multiline: None,
        trailing_comma: false,
        lead,
        trail,
        line_start,
        line_end,
        span,
        inner_comment: has_comment(span.text(text)),
    };
    let attrs_of = |a: Option<NodeId>| {
        a.map(|a| format!(" {}", ast.span(a).text(text)))
            .unwrap_or_default()
    };
    match *ast.node(s) {
        Node::ImportDecl {
            type_only,
            phase,
            has_clause,
            specifiers,
            source,
            attributes,
        } => {
            if phase != ts_r_ast::ImportPhase::Evaluation {
                return None;
            }
            let (raw, value) = spec_of(source)?;
            d.type_only = type_only;
            d.spec_raw = Some(raw);
            d.spec = Some(value);
            d.attrs = attrs_of(attributes);
            if has_clause {
                for &sp in ast.list(specifiers) {
                    match *ast.node(sp) {
                        Node::ImportDefaultSpecifier { local } => {
                            d.default = Some((src(local), Some(Binding { spec: sp, local })));
                        }
                        Node::ImportNamespaceSpecifier { local } => {
                            d.namespace = Some((src(local), Some(Binding { spec: sp, local })));
                        }
                        Node::ImportSpecifier {
                            type_only,
                            imported,
                            local,
                        } => {
                            d.named.get_or_insert_with(Vec::new).push(Item {
                                name: imported.map_or_else(|| src(local), src),
                                alias: imported.map(|_| src(local)),
                                type_only,
                                binding: Some(Binding { spec: sp, local }),
                                at: None,
                                nl_before: false,
                            });
                        }
                        _ => return None,
                    }
                }
                let clause = text.get(span.start as usize..ast.span(source).start as usize)?;
                if clause.contains('{') {
                    d.named.get_or_insert_with(Vec::new);
                    d.multiline = multiline_indent(clause);
                    d.trailing_comma = d.multiline.is_some() && trailing_comma(clause);
                }
                if d.default.is_none() && d.namespace.is_none() && d.named.is_none() {
                    return None;
                }
            }
        }
        Node::ExportNamed {
            type_only,
            specifiers,
            source,
            attributes,
        } => {
            d.export = true;
            d.type_only = type_only;
            if let Some(src_node) = source {
                let (raw, value) = spec_of(src_node)?;
                d.spec_raw = Some(raw);
                d.spec = Some(value);
            }
            d.attrs = attrs_of(attributes);
            let mut items = Vec::new();
            let mut prev_end: Option<u32> = None;
            for (i, &sp) in ast.list(specifiers).iter().enumerate() {
                let Node::ExportSpecifier {
                    type_only,
                    local,
                    exported,
                } = *ast.node(sp)
                else {
                    return None;
                };
                let sp_span = ast.span(sp);
                // The first specifier: a line break after `{`.
                let from = prev_end.or_else(|| {
                    let head = text.get(span.start as usize..sp_span.start as usize)?;
                    head.rfind('{').map(|i| span.start + i as u32)
                });
                let nl_before = from.is_some_and(|p| {
                    text.get(p as usize..sp_span.start as usize)
                        .is_some_and(|t| t.contains('\n'))
                });
                prev_end = Some(sp_span.end);
                items.push(Item {
                    name: src(local),
                    alias: exported.map(src),
                    type_only,
                    binding: None,
                    at: Some((s, i as u32)),
                    nl_before,
                });
            }
            let end = source.map_or(span.end, |n| ast.span(n).start);
            let clause = text.get(span.start as usize..end as usize)?;
            d.multiline = multiline_indent(clause);
            d.trailing_comma = d.multiline.is_some() && trailing_comma(clause);
            d.named = Some(items);
        }
        Node::ExportAll {
            type_only,
            exported,
            source,
            attributes,
        } => {
            d.export = true;
            d.type_only = type_only;
            let (raw, value) = spec_of(source)?;
            d.spec_raw = Some(raw);
            d.spec = Some(value);
            d.attrs = attrs_of(attributes);
            d.namespace = Some((exported.map(src).unwrap_or_default(), None));
        }
        _ => return None,
    }
    Some(d)
}

/// Whether `src` (a declaration) holds a comment outside its string literals.
fn has_comment(src: &str) -> bool {
    let b = src.as_bytes();
    let mut quote = None;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        match quote {
            Some(q) => {
                if c == b'\\' {
                    i += 1;
                } else if c == q {
                    quote = None;
                }
            }
            None => {
                if c == b'"' || c == b'\'' || c == b'`' {
                    quote = Some(c);
                } else if c == b'/' && matches!(b.get(i + 1), Some(b'/' | b'*')) {
                    return true;
                }
            }
        }
        i += 1;
    }
    false
}

/// Indentation of the first specifier when a brace list spans lines.
fn multiline_indent(clause: &str) -> Option<String> {
    let open = clause.find('{')?;
    let inner = &clause[open + 1..];
    let nl = inner.find('\n')?;
    let ws: String = inner[nl + 1..]
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect();
    Some(if ws.is_empty() { "    ".into() } else { ws })
}

/// Whether the last `}` of `clause` follows a comma.
fn trailing_comma(clause: &str) -> bool {
    clause
        .rfind('}')
        .is_some_and(|i| clause[..i].trim_end().ends_with(','))
}

/// Start of the declaration's line when only whitespace and comments precede it there, with
/// those comments' text (`/* a */ `).
fn line_lead(ast: &Ast, text: &str, start: u32) -> (u32, String) {
    let ls = text[..start as usize]
        .rfind('\n')
        .map_or(0, |i| i as u32 + 1);
    let comments = ast.comments_in(Span::new(ls, start));
    let mut covered = ls;
    for c in comments {
        if !text[covered as usize..c.span.start as usize]
            .trim()
            .is_empty()
        {
            return (start, String::new());
        }
        covered = c.span.end.min(start);
    }
    if !text[covered as usize..start as usize].trim().is_empty() {
        return (start, String::new());
    }
    let first = comments.first().map_or(start, |c| c.span.start);
    (ls, text[first as usize..start as usize].to_string())
}

/// End of the declaration's line (after the line break) when only whitespace and comments
/// follow it there, with those comments' text (` // x`).
fn line_trail(ast: &Ast, text: &str, end: u32) -> (u32, String) {
    let le = text[end as usize..]
        .find('\n')
        .map_or(text.len() as u32, |i| end + i as u32);
    let comments = ast.comments_in(Span::new(end, le));
    let mut covered = end;
    for c in comments {
        if c.span.end > le
            || !text[covered as usize..c.span.start as usize]
                .trim()
                .is_empty()
        {
            return (end, String::new());
        }
        covered = c.span.end;
    }
    if !text[covered as usize..le as usize].trim().is_empty() {
        return (end, String::new());
    }
    let trail = text[end as usize..covered as usize].to_string();
    let next = if (le as usize) < text.len() {
        le + 1
    } else {
        le
    };
    (next, trail)
}

/// Which import bindings are used.
struct Usage<'a> {
    b: &'a BoundFile,
    used: HashSet<SymbolId>,
    names: HashSet<String>,
}

impl<'a> Usage<'a> {
    fn new(
        cx: &Ctx<'_, '_>,
        ast: &Ast,
        text: &str,
        b: &'a BoundFile,
        stmts: &[NodeId],
    ) -> Usage<'a> {
        let import_spans: Vec<(u32, u32)> = stmts
            .iter()
            .filter(|&&s| matches!(ast.node(s), Node::ImportDecl { .. }))
            .map(|&s| (ast.span(s).start, ast.span(s).end))
            .collect();
        let inside = |p: u32| {
            let i = import_spans.partition_point(|&(_, e)| e <= p);
            import_spans.get(i).is_some_and(|&(s, _)| s <= p)
        };
        let mut used = HashSet::new();
        for &(n, s) in &b.refs.resolved {
            if !inside(ast.span(n).start) {
                used.insert(s);
            }
        }
        let mut names = HashSet::new();
        // The classic JSX factory is used by any JSX.
        let classic = !matches!(
            cx.program.options.jsx(),
            Some(JsxEmit::ReactJsx | JsxEmit::ReactJsxDev)
        );
        if classic && has_jsx(ast) {
            let factory = cx.program.options.jsx_factory().unwrap_or("React");
            names.insert(factory.split('.').next().unwrap_or("React").to_string());
        }
        // `{@link X}` and `@jsx` pragmas name bindings too.
        for c in &ast.comments {
            let mut rest = c.span.text(text);
            while let Some(i) = rest.find('@') {
                rest = &rest[i + 1..];
                let tag_end = rest
                    .find(|ch: char| !ch.is_ascii_alphanumeric())
                    .unwrap_or(rest.len());
                let tag = &rest[..tag_end];
                if !matches!(
                    tag,
                    "link" | "linkcode" | "linkplain" | "see" | "jsx" | "jsxFrag"
                ) {
                    continue;
                }
                let after = rest[tag_end..].trim_start();
                let end = after
                    .find(|ch: char| !(ch.is_alphanumeric() || ch == '_' || ch == '$'))
                    .unwrap_or(after.len());
                if end > 0 {
                    names.insert(after[..end].to_string());
                }
            }
        }
        Usage { b, used, names }
    }

    fn keeps(&self, bnd: Option<Binding>, name: &str) -> bool {
        let Some(bnd) = bnd else {
            return true;
        };
        if self.names.contains(name) {
            return true;
        }
        match self
            .b
            .symbol_of_decl(bnd.spec)
            .or_else(|| self.b.symbol_of_decl(bnd.local))
        {
            Some(s) => self.used.contains(&s),
            None => true,
        }
    }

    /// The declaration without its unused bindings; `None` when nothing is left.
    fn prune(&self, d: &Decl) -> Option<Decl> {
        if d.is_side_effect() {
            return Some(d.clone());
        }
        let mut d = d.clone();
        if let Some((name, b)) = &d.default
            && !self.keeps(*b, name)
        {
            d.default = None;
        }
        if let Some((name, b)) = &d.namespace
            && !self.keeps(*b, name)
        {
            d.namespace = None;
        }
        if let Some(items) = d.named.as_mut() {
            items.retain(|it| self.keeps(it.binding, it.local_name()));
            if items.is_empty() {
                d.named = None;
            }
        }
        if d.default.is_none() && d.namespace.is_none() && d.named.is_none() {
            return None;
        }
        Some(d)
    }
}

fn has_jsx(ast: &Ast) -> bool {
    let mut stack = vec![ast.root];
    let mut guard = 0u32;
    while let Some(n) = stack.pop() {
        guard += 1;
        if guard > 5_000_000 {
            return false;
        }
        if matches!(
            ast.node(n),
            Node::JsxElement { .. } | Node::JsxFragment { .. }
        ) {
            return true;
        }
        ast.for_each_child(n, |c| stack.push(c));
    }
    false
}

/// Same bindings and specifiers, in the same order.
fn same_bindings(a: &Decl, b: &Decl) -> bool {
    let items = |d: &Decl| {
        d.named
            .as_ref()
            .map(|v| v.iter().map(Item::print).collect::<Vec<_>>())
    };
    a.default.as_ref().map(|x| &x.0) == b.default.as_ref().map(|x| &x.0)
        && a.namespace.as_ref().map(|x| &x.0) == b.namespace.as_ref().map(|x| &x.0)
        && items(a) == items(b)
}

/// Case mode of module specifiers: insensitive unless every group is sorted ordinally and
/// some group is not sorted insensitively.
fn detect_spec_case(groups: &[Vec<Decl>]) -> Case {
    let sorted = |case: Case| {
        groups.iter().all(|g| {
            let specs: Vec<Option<&str>> = g.iter().map(|d| d.spec.as_deref()).collect();
            is_sorted_by(&specs, |a, b| cmp_spec(*a, *b, case))
        })
    };
    if !sorted(Case::Insensitive) && sorted(Case::Ordinal) {
        Case::Ordinal
    } else {
        Case::Insensitive
    }
}

/// Case and type order of specifiers in braces: the (case, order) under which the lists
/// (with more than one specifier) have the fewest neighbours out of order; ties go to the
/// first of (insensitive, last), (insensitive, inline), (insensitive, first), then ordinal.
fn detect_item_order<'a>(decls: impl Iterator<Item = &'a Decl>) -> (Case, TypeOrder) {
    let lists: Vec<&Vec<Item>> = decls
        .filter_map(|d| d.named.as_ref())
        .filter(|v| v.len() > 1)
        .collect();
    let mut best = ((Case::Insensitive, TypeOrder::Last), usize::MAX);
    for case in [Case::Insensitive, Case::Ordinal] {
        for order in [TypeOrder::Last, TypeOrder::Inline, TypeOrder::First] {
            let bad: usize = lists
                .iter()
                .map(|l| {
                    l.windows(2)
                        .filter(|w| cmp_item(&w[0], &w[1], case, order) == Ordering::Greater)
                        .count()
                })
                .sum();
            if bad < best.1 {
                best = ((case, order), bad);
            }
        }
    }
    best.0
}

/// Merges and sorts one group's declarations.
fn combine(decls: Vec<Out>, spec_case: Case, item_case: Case, order: TypeOrder) -> Vec<Out> {
    // Module keys in order of first appearance, then sorted (stable).
    let mut keys: Vec<(bool, Option<String>)> = Vec::new();
    for o in &decls {
        let k = (o.d.export, o.d.spec.clone());
        if !keys.contains(&k) {
            keys.push(k);
        }
    }
    keys.sort_by(|a, b| cmp_spec(a.1.as_deref(), b.1.as_deref(), spec_case));
    let mut out = Vec::new();
    for (export, spec) in keys {
        let same: Vec<&Out> = decls
            .iter()
            .filter(|o| o.d.export == export && o.d.spec == spec)
            .collect();
        if export {
            combine_exports(&same, Case::Ordinal, TypeOrder::Last, &mut out);
        } else {
            combine_imports(&same, item_case, order, &mut out);
        }
    }
    out
}

fn sort_items(items: &mut Vec<Item>, case: Case, order: TypeOrder) -> bool {
    let mut uniq: Vec<Item> = Vec::with_capacity(items.len());
    for it in items.drain(..) {
        if !uniq.iter().any(|u| u.same(&it)) {
            uniq.push(it);
        }
    }
    let before: Vec<String> = uniq.iter().map(Item::print).collect();
    uniq.sort_by(|a, b| cmp_item(a, b, case, order));
    let after: Vec<String> = uniq.iter().map(Item::print).collect();
    *items = uniq;
    before != after
}

/// Merges `parts` (same module, same kind) into one output declaration based on the first.
fn merged(parts: &[&Out], d: Decl) -> Out {
    let mut from: Vec<NodeId> = parts.iter().flat_map(|o| o.from.iter().copied()).collect();
    from.dedup();
    Out {
        changed: parts.len() > 1 || parts.iter().any(|o| o.changed),
        reshaped: parts.len() > 1 || parts.iter().any(|o| o.reshaped),
        d,
        from,
    }
}

fn combine_imports(same: &[&Out], case: Case, order: TypeOrder, out: &mut Vec<Out>) {
    // One side-effect import.
    let side: Vec<&Out> = same
        .iter()
        .copied()
        .filter(|o| o.d.is_side_effect())
        .collect();
    if let Some(first) = side.first() {
        out.push(merged(&side, first.d.clone()));
    }
    for type_only in [true, false] {
        let kind: Vec<&Out> = same
            .iter()
            .copied()
            .filter(|o| !o.d.is_side_effect() && o.d.type_only == type_only)
            .collect();
        if kind.is_empty() {
            continue;
        }
        // Namespace imports, each its own declaration (duplicates dropped).
        let mut seen_ns: Vec<String> = Vec::new();
        for o in &kind {
            if let Some((ns, b)) = &o.d.namespace {
                if seen_ns.contains(ns) {
                    continue;
                }
                seen_ns.push(ns.clone());
                let alone = o.d.default.is_none() && o.d.named.is_none();
                let mut d = o.d.clone();
                d.default = None;
                d.named = None;
                d.namespace = Some((ns.clone(), *b));
                out.push(Out {
                    d,
                    from: o.from.clone(),
                    changed: o.changed || !alone,
                    reshaped: o.reshaped || !alone,
                });
            }
        }
        // Defaults and named imports.
        let rest: Vec<&Out> = kind
            .iter()
            .copied()
            .filter(|o| o.d.default.is_some() || o.d.named.is_some())
            .collect();
        let Some(first) = rest.first() else {
            continue;
        };
        let mut defaults: Vec<(String, Option<Binding>)> = Vec::new();
        let mut items: Vec<Item> = Vec::new();
        let mut braces = false;
        for o in &rest {
            if let Some(def) = &o.d.default
                && !defaults.iter().any(|x| x.0 == def.0)
            {
                defaults.push(def.clone());
            }
            if let Some(v) = &o.d.named {
                braces = true;
                items.extend(v.iter().cloned());
            }
        }
        let mut d = first.d.clone();
        d.namespace = None;
        if defaults.len() > 1 {
            for (name, b) in defaults.drain(..) {
                items.push(Item {
                    name: "default".into(),
                    alias: Some(name),
                    type_only: false,
                    binding: b,
                    at: None,
                    nl_before: false,
                });
            }
            braces = true;
        }
        d.default = defaults.pop();
        let reordered = sort_items(&mut items, case, order);
        d.named = (braces && (!items.is_empty() || d.default.is_none())).then_some(items);
        if d.multiline.is_none()
            && let Some(m) = rest.iter().find(|o| o.d.multiline.is_some())
        {
            d.multiline = m.d.multiline.clone();
            d.trailing_comma = m.d.trailing_comma;
        }
        let mut o = merged(&rest, d);
        o.changed |= reordered || first.d.namespace.is_some();
        o.reshaped |= first.d.namespace.is_some();
        out.push(o);
    }
}

fn combine_exports(same: &[&Out], case: Case, order: TypeOrder, out: &mut Vec<Out>) {
    for type_only in [false, true] {
        let named: Vec<&Out> = same
            .iter()
            .copied()
            .filter(|o| o.d.named.is_some() && o.d.type_only == type_only)
            .collect();
        let Some(first) = named.first() else {
            continue;
        };
        let mut items: Vec<Item> = named
            .iter()
            .flat_map(|o| o.d.named.iter().flatten().cloned())
            .collect();
        let reordered = sort_items(&mut items, case, order);
        let mut d = first.d.clone();
        d.named = Some(items);
        if d.multiline.is_none()
            && let Some(m) = named.iter().find(|o| o.d.multiline.is_some())
        {
            d.multiline = m.d.multiline.clone();
        }
        let mut o = merged(&named, d);
        o.changed |= reordered;
        out.push(o);
    }
    let mut seen: Vec<(bool, String)> = Vec::new();
    for o in same.iter().filter(|o| o.d.namespace.is_some()) {
        let key = (o.d.type_only, o.d.namespace.clone().unwrap_or_default().0);
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        out.push((*o).clone());
    }
}

/// Edits turning group `g` into `out`: the printed declarations replace the first one's lines,
/// the others' lines are removed. Nothing when the group does not change.
fn group_edits(
    text: &str,
    semi: bool,
    is_export: bool,
    g: &[Decl],
    out: &[Out],
    edits: &mut FileEdits,
) {
    let unchanged = out.len() == g.len()
        && out
            .iter()
            .zip(g)
            .all(|(o, d)| !o.changed && o.from.len() == 1 && o.from[0] == d.node);
    let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
    if unchanged {
        // tsserver prints the group again even when nothing moved: spacing becomes the
        // printer's (`{a}` → `{ a }`) and the semicolon the file's. Only what differs is
        // edited; a declaration with a comment inside keeps its text but the semicolon.
        for (o, d) in out.iter().zip(g) {
            let own = d.span.text(text);
            let has_semi = own.trim_end().ends_with(';');
            if d.inner_comment {
                if semi && !has_semi {
                    edits.push(Edit::new(d.span.end, d.span.end, ";"));
                } else if !semi && has_semi {
                    let at = d.span.start + own.trim_end().len() as u32 - 1;
                    edits.push(Edit::delete(at, at + 1));
                }
                continue;
            }
            let lines = &text[d.line_start as usize..d.line_end as usize];
            let lines = lines.strip_suffix('\n').unwrap_or(lines);
            let lines = lines.strip_suffix('\r').unwrap_or(lines);
            let printed = o.d.print(nl, semi, !is_export);
            if printed != lines {
                edits.push(Edit::new(
                    d.line_start,
                    d.line_start + lines.len() as u32,
                    printed,
                ));
            }
        }
        return;
    }
    let first = &g[0];
    let mut printed = String::new();
    for o in out {
        // A multi-line import list keeps its trailing comma unless bindings were dropped or
        // merged; reprinted export lists lose theirs.
        printed.push_str(&o.d.print(nl, semi, !is_export && !o.reshaped));
        printed.push_str(nl);
    }
    if !text[..first.line_end as usize].ends_with('\n') {
        printed.truncate(printed.len().saturating_sub(nl.len()));
    }
    edits.push(Edit::new(first.line_start, first.line_end, printed));
    for d in &g[1..] {
        edits.push(Edit::delete(d.line_start, d.line_end));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insensitive_compare_lowercases() {
        assert_eq!(cmp_str("_x", "aaa", Case::Insensitive), Ordering::Less);
        assert_eq!(cmp_str("Abc", "aaa", Case::Insensitive), Ordering::Greater);
        assert_eq!(cmp_str("Bar", "bar", Case::Insensitive), Ordering::Equal);
        assert_eq!(cmp_str("B", "a", Case::Ordinal), Ordering::Less);
    }

    #[test]
    fn bare_specifiers_first() {
        let c = Case::Insensitive;
        assert_eq!(cmp_spec(Some("pkg"), Some("./a"), c), Ordering::Less);
        assert_eq!(cmp_spec(Some("../a"), Some("./a"), c), Ordering::Less);
        assert_eq!(cmp_spec(None, Some("./a"), c), Ordering::Greater);
    }
}
