//! Completions inside `/** */` comments: tag names, `@param` names and the types of a
//! tag's `{...}` expression.
//!
//! Rules observed from tsc 7.0.2's answers on the fourslash tests (`completionInJsDoc`,
//! `jsdocParameterNameCompletion`, `jsdoc*TagCompletion`, ...), written in our own terms:
//!
//! - Right after `@` (also `{@` and `@pa|`), the tag names without `@`.
//! - Where a tag may start, with nothing typed (only blanks, `*` and the `/**` opener before
//!   the cursor on its line), the tag names with `@`.
//! - The name slot of `@param` (`@arg`, `@argument`), before or after its `{type}`: the
//!   documented function's parameters that start with the typed text and no other
//!   `@param` of the comment names. No function, or none left: no list.
//! - Inside an unclosed `{` (not an inline `{@link}`): the types in scope at the documented
//!   node, or after `A.B.` the types and namespaces `A.B` exports. A member slot of a type
//!   literal nested in it offers `readonly` while a name is typed (`{ {a| } }`), nothing
//!   before one (`{ {| } }`).
//! - In an `@import` tag's quoted module specifier, the module paths a static import
//!   offers; in a `with { type: "|" }` value, the literals the global `ImportAttributes`
//!   allows for that key.
//! - Anywhere else in the comment: nothing.

use ts_r_ast::{Ast, CommentKind, Node, NodeId};
use ts_r_checker::Checker;
use ts_r_diagnostics::FileId;

/// The JSDoc tags tsc 7 offers, in its order.
pub(super) const TAG_NAMES: &[&str] = &[
    "abstract",
    "access",
    "alias",
    "argument",
    "async",
    "augments",
    "author",
    "borrows",
    "callback",
    "class",
    "classdesc",
    "constant",
    "constructor",
    "constructs",
    "copyright",
    "default",
    "deprecated",
    "description",
    "emits",
    "enum",
    "event",
    "example",
    "exports",
    "extends",
    "external",
    "field",
    "file",
    "fileoverview",
    "fires",
    "function",
    "generator",
    "global",
    "hideconstructor",
    "host",
    "ignore",
    "implements",
    "import",
    "inheritdoc",
    "inner",
    "instance",
    "interface",
    "kind",
    "lends",
    "license",
    "link",
    "linkcode",
    "linkplain",
    "listens",
    "member",
    "memberof",
    "method",
    "mixes",
    "module",
    "name",
    "namespace",
    "overload",
    "override",
    "package",
    "param",
    "private",
    "prop",
    "property",
    "protected",
    "public",
    "readonly",
    "requires",
    "returns",
    "satisfies",
    "see",
    "since",
    "static",
    "summary",
    "template",
    "this",
    "throws",
    "todo",
    "tutorial",
    "type",
    "typedef",
    "var",
    "variation",
    "version",
    "virtual",
    "yields",
];

/// What a position in a JSDoc comment completes.
pub(super) enum Spot {
    /// After `@`: tag names without the `@`.
    TagName,
    /// Where a tag may start: tag names with the `@`.
    TagStart,
    /// The name of a `@param` tag: these parameter names.
    ParamName(Vec<String>),
    /// Inside a tag's type: types in scope at `host`, or the exports of the namespace the
    /// `qualifier` names (`A.B.|`).
    Type {
        host: NodeId,
        qualifier: Vec<String>,
    },
    /// A member name of a type literal inside a tag's type.
    TypeMember,
    /// The module specifier of an `@import` tag, typed up to the cursor.
    ImportModule(String),
    /// The string value of an `@import`'s `with { key: "|" }` attribute.
    ImportAttribute(String),
    /// Elsewhere in the comment.
    Nothing,
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

/// The spot at `offset` when it is inside a JSDoc comment (`/**`, not `/**/`); `None`
/// outside one.
pub(super) fn spot(c: &Checker<'_>, file: FileId, offset: u32) -> Option<Spot> {
    let ast = c.ast(file);
    let text = c.file(file).text.as_str();
    let b = text.as_bytes();
    let i = ast.comments.partition_point(|cm| cm.span.end < offset);
    let cm = ast.comments.get(i)?;
    let (start, end) = (cm.span.start as usize, cm.span.end as usize);
    let offset = offset as usize;
    if cm.kind != CommentKind::Block
        || !(start < offset && offset < end)
        || end > b.len()
        || !b[start..end].starts_with(b"/**")
        || end - start < 5
        || offset < start + 3
    {
        return None;
    }
    // Inside the closing `*/`.
    if offset > end - 2 {
        return Some(Spot::Nothing);
    }
    let body = start + 3;
    let mut ws = offset;
    while ws > body && is_ident_byte(b[ws - 1]) {
        ws -= 1;
    }
    let prefix = &text[ws..offset];
    if let Some(s) = import_string(text, body, offset) {
        return Some(s);
    }
    // `@|`, `@pa|`, `{@|`.
    if ws > body && b[ws - 1] == b'@' && !(ws - 1 > body && is_ident_byte(b[ws - 2])) {
        return Some(Spot::TagName);
    }
    // Unclosed braces before the word (one scan of the comment up to the cursor).
    let mut open: Vec<usize> = Vec::new();
    for (k, &ch) in b.iter().enumerate().take(ws).skip(body) {
        match ch {
            b'{' => open.push(k),
            b'}' => {
                open.pop();
            }
            _ => {}
        }
    }
    if let Some(&inner) = open.last() {
        if b.get(inner + 1) == Some(&b'@') {
            return Some(Spot::Nothing);
        }
        let before = text[inner + 1..ws].trim_end();
        let member_slot = open.len() >= 2
            && (before.is_empty() || before.ends_with(',') || before.ends_with(';'));
        if member_slot {
            return Some(if prefix.is_empty() {
                Spot::Nothing
            } else {
                Spot::TypeMember
            });
        }
        return Some(Spot::Type {
            host: documented(ast, b, end as u32),
            qualifier: qualifier(text, inner + 1, ws),
        });
    }
    if let Some(names) = param_name(c, file, text, body, end, ws, prefix) {
        return Some(if names.is_empty() {
            Spot::Nothing
        } else {
            Spot::ParamName(names)
        });
    }
    // A tag start: nothing typed, only blanks and `*` (or the opener) before it on its line.
    if prefix.is_empty() {
        let line = text[..offset]
            .rfind('\n')
            .map_or(start, |n| (n + 1).max(start));
        if b[line..offset]
            .iter()
            .all(|&ch| ch.is_ascii_whitespace() || ch == b'*' || ch == b'/')
        {
            return Some(Spot::TagStart);
        }
    }
    Some(Spot::Nothing)
}

/// Inside a quoted string of an `@import` tag: its module specifier (after `from`) or an
/// attribute value (after `key:` in `with { ... }`).
fn import_string(text: &str, body: usize, offset: usize) -> Option<Spot> {
    let b = text.as_bytes();
    let at = text[body..offset].rfind('@')? + body;
    if !text[at..offset].starts_with("@import") {
        return None;
    }
    // The quote that opens the string holding the cursor, if any.
    let mut open: Option<(usize, u8)> = None;
    for (k, &ch) in b.iter().enumerate().take(offset).skip(at) {
        match open {
            Some((_, q)) if ch == q => open = None,
            None if ch == b'"' || ch == b'\'' => open = Some((k, ch)),
            _ => {}
        }
    }
    let (q, _) = open?;
    let before = text[at..q].trim_end();
    if before.ends_with("from") {
        return Some(Spot::ImportModule(text[q + 1..offset].to_string()));
    }
    let key = before.strip_suffix(':')?.trim_end();
    let ks = key
        .bytes()
        .rposition(|ch| !is_ident_byte(ch))
        .map_or(0, |i| i + 1);
    let name = &key[ks..];
    (!name.is_empty()).then(|| Spot::ImportAttribute(name.to_string()))
}

/// The names written before `ws` as `A.B.` (up to the brace at `from`), outermost first;
/// empty when the word is not after a dot or a part is not a name.
fn qualifier(text: &str, from: usize, ws: usize) -> Vec<String> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut p = ws;
    // Each round consumes a `.` and a name, so it ends.
    while p > from && b[p - 1] == b'.' {
        let e = p - 1;
        let mut s = e;
        while s > from && is_ident_byte(b[s - 1]) {
            s -= 1;
        }
        if s == e || b[s].is_ascii_digit() {
            return Vec::new();
        }
        out.push(text[s..e].to_string());
        p = s;
    }
    out.reverse();
    out
}

/// The parameter names a `@param` name slot at `ws` offers, or `None` when `ws` is not one.
fn param_name(
    c: &Checker<'_>,
    file: FileId,
    text: &str,
    body: usize,
    end: usize,
    ws: usize,
    prefix: &str,
) -> Option<Vec<String>> {
    let b = text.as_bytes();
    // The tag the word belongs to: the last `@name` before it.
    let at = text[body..ws].rfind('@')? + body;
    let mut ne = at + 1;
    while ne < ws && is_ident_byte(b[ne]) {
        ne += 1;
    }
    if !matches!(&text[at + 1..ne], "param" | "arg" | "argument") {
        return None;
    }
    // Between the tag name and the word: blanks, at most one `{...}` type, blanks.
    let between = text[ne..ws].trim();
    if !(between.is_empty() || (between.starts_with('{') && between.ends_with('}'))) {
        return None;
    }
    if ne == ws {
        return None;
    }
    let ast = c.ast(file);
    let host = documented(ast, b, end as u32);
    let params = function_params(ast, host)?;
    let documented = documented_params(text, body, end, at);
    Some(
        params
            .into_iter()
            .filter(|p| p.starts_with(prefix) && !documented.iter().any(|d| d == p))
            .collect(),
    )
}

/// The names other `@param` tags of the comment give (not the tag at `skip`).
fn documented_params(text: &str, body: usize, end: usize, skip: usize) -> Vec<String> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut p = body;
    let stop = end.saturating_sub(2);
    // Each round moves past one `@`.
    while let Some(k) = text[p..stop].find('@') {
        let at = p + k;
        p = at + 1;
        if at == skip {
            continue;
        }
        let mut ne = at + 1;
        while ne < stop && is_ident_byte(b[ne]) {
            ne += 1;
        }
        if !matches!(&text[at + 1..ne], "param" | "arg" | "argument") {
            continue;
        }
        let mut q = ne;
        while q < stop && b[q].is_ascii_whitespace() {
            q += 1;
        }
        // Skip a balanced `{...}` type.
        if q < stop && b[q] == b'{' {
            let mut depth = 0u32;
            while q < stop {
                match b[q] {
                    b'{' => depth += 1,
                    b'}' => {
                        depth -= 1;
                        if depth == 0 {
                            q += 1;
                            break;
                        }
                    }
                    _ => {}
                }
                q += 1;
            }
            while q < stop && b[q].is_ascii_whitespace() {
                q += 1;
            }
        }
        if q < stop && b[q] == b'[' {
            q += 1;
        }
        let s = q;
        while q < stop && is_ident_byte(b[q]) {
            q += 1;
        }
        if q > s {
            out.push(text[s..q].to_string());
        }
    }
    out
}

/// [`documented`] for every comment of a file at once: built in one walk of the tree and
/// one pass over the comments, so asking for each of many comments stays linear.
pub(super) struct DocHosts {
    /// The outermost non-empty node starting at each offset, by offset.
    starts: Vec<(u32, NodeId)>,
    /// For each comment, where the first token after it (and the comments right after it)
    /// starts.
    next_code: Vec<u32>,
}

impl DocHosts {
    pub(super) fn new(ast: &Ast, b: &[u8]) -> DocHosts {
        struct V(Vec<(u32, NodeId)>, NodeId);
        impl ts_r_ast::visit::Visitor for V {
            fn enter(&mut self, ast: &Ast, id: NodeId) -> ts_r_ast::visit::Walk {
                let s = ast.span(id);
                if id != self.1 && s.end > s.start {
                    self.0.push((s.start, id));
                }
                ts_r_ast::visit::Walk::Continue
            }
        }
        let mut v = V(Vec::new(), ast.root);
        ts_r_ast::visit::walk(ast, ast.root, &mut v);
        let mut starts = v.0;
        // A parent is entered before its children: a stable sort keeps the outermost first.
        starts.sort_by_key(|&(s, _)| s);
        starts.dedup_by_key(|&mut (s, _)| s);
        let n = ast.comments.len();
        let mut next_code = vec![0u32; n];
        for i in (0..n).rev() {
            let mut p = ast.comments[i].span.end as usize;
            while p < b.len() && b[p].is_ascii_whitespace() {
                p += 1;
            }
            next_code[i] = match ast.comments.get(i + 1) {
                Some(cm) if cm.span.start as usize == p => next_code[i + 1],
                _ => p as u32,
            };
        }
        DocHosts { starts, next_code }
    }

    /// The node comment `i` documents (see [`documented`]).
    pub(super) fn host(&self, ast: &Ast, i: usize) -> NodeId {
        let Some(&p) = self.next_code.get(i) else {
            return ast.root;
        };
        match self.starts.binary_search_by_key(&p, |&(s, _)| s) {
            Ok(k) => self.starts[k].1,
            Err(_) => ast.root,
        }
    }
}

/// The node a comment ending at `end` documents: the outermost node starting at the first
/// token after it (other comments skipped), else the file.
pub(super) fn documented(ast: &Ast, b: &[u8], end: u32) -> NodeId {
    let mut p = end as usize;
    let mut i = ast.comments.partition_point(|cm| cm.span.start < end);
    // Each round skips blanks and at most one comment.
    loop {
        while p < b.len() && b[p].is_ascii_whitespace() {
            p += 1;
        }
        match ast.comments.get(i) {
            Some(cm) if cm.span.start as usize == p => {
                p = cm.span.end as usize;
                i += 1;
            }
            _ => break,
        }
    }
    let p = p as u32;
    let mut cur = ast.root;
    // Child ids are smaller than their parent's, so this descends at most node_count times.
    for _ in 0..ast.node_count() {
        let mut next = None;
        ast.for_each_child(cur, |ch| {
            let s = ast.span(ch);
            if next.is_none() && s.start <= p && p < s.end {
                next = Some(ch);
            }
        });
        match next {
            Some(ch) if ast.span(ch).start == p => return ch,
            Some(ch) => cur = ch,
            None => break,
        }
    }
    ast.root
}

/// The identifier parameter names of the function `host` declares or holds (a variable,
/// property or assignment initialized with one, a method, an exported function).
fn function_params(ast: &Ast, host: NodeId) -> Option<Vec<String>> {
    let mut n = host;
    for _ in 0..8 {
        n = match *ast.node(n) {
            Node::FunctionDecl { params, .. }
            | Node::FunctionExpr { params, .. }
            | Node::Arrow { params, .. }
            | Node::MethodSignature { params, .. }
            | Node::CallSignature { params, .. }
            | Node::FunctionType { params, .. } => {
                let names = ast
                    .list(params)
                    .iter()
                    .filter_map(|&p| match *ast.node(p) {
                        Node::Param { pattern, .. } => match ast.node(pattern) {
                            Node::Ident { name } if ast.atom(*name) != "this" => {
                                Some(ast.atom(*name).to_string())
                            }
                            _ => None,
                        },
                        _ => None,
                    })
                    .collect();
                return Some(names);
            }
            Node::MethodDef { value, .. } => value,
            Node::PropertyDef { value, .. } => value?,
            Node::Property { value, .. } => value,
            Node::PropertySignature { type_ann, .. } => type_ann?,
            Node::VarDecl { decls, .. } => {
                let first = *ast.list(decls).first()?;
                match *ast.node(first) {
                    Node::VarDeclarator { init, .. } => init?,
                    _ => return None,
                }
            }
            Node::ExprStmt { expr } => expr,
            Node::Assign { right, .. } => right,
            Node::Paren { expr } => expr,
            Node::ExportDefaultExpr { expr } => expr,
            _ => return None,
        };
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hosts_agree(src: &str) {
        let ast = ts_r_parser::parse(src, ts_r_ast::SourceKind::Ts).ast;
        let b = src.as_bytes();
        let hosts = DocHosts::new(&ast, b);
        for (i, cm) in ast.comments.iter().enumerate() {
            assert_eq!(hosts.host(&ast, i), documented(&ast, b, cm.span.end), "{i}");
        }
    }

    #[test]
    fn doc_hosts_match_documented() {
        hosts_agree(
            "/** a */ /* b */\n// c\nexport function f(/** p */ x: number) {}\n\
             class C {\n  /** m */ m() {}\n  /** n */\n  /** n2 */ n = 1;\n}\n/** end */",
        );
        hosts_agree("");
        hosts_agree("/** only */");
    }

    /// Many declarations, each with a doc comment, and a long run of adjacent comments:
    /// building the hosts is one walk and one pass, not a walk per comment.
    #[test]
    fn doc_hosts_are_linear() {
        let mut src = String::new();
        for i in 0..20_000 {
            src.push_str(&format!("/** {{@link X}} */ const v{i} = 1;\n"));
        }
        for _ in 0..20_000 {
            src.push_str("/** a */");
        }
        src.push_str("\nconst last = 1;\n");
        let ast = ts_r_parser::parse(&src, ts_r_ast::SourceKind::Ts).ast;
        let started = std::time::Instant::now();
        let hosts = DocHosts::new(&ast, src.as_bytes());
        let n = ast.comments.len();
        let last = hosts.host(&ast, n - 1);
        assert!(matches!(ast.node(last), Node::VarDecl { .. }));
        assert_eq!(hosts.host(&ast, 20_000), last);
        for i in 0..n {
            let _ = hosts.host(&ast, i);
        }
        assert!(started.elapsed().as_secs() < 5, "{:?}", started.elapsed());
    }
}
