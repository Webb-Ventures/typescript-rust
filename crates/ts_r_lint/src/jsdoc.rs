//! Shared machinery for the `jsdoc/*` rules (eslint-plugin-jsdoc): the plugin settings, which
//! comment is a node's JSDoc block, a block model with the same tokens as the `comment-parser`
//! package (so fixes rebuild the comment text the way the plugin does), and the walk that hands
//! each (node, block) pair to a rule.
//!
//! The behaviour is described in the crate's AGENTS.md ("jsdoc rules"); it was learned from
//! the plugin's tests and from reading its source, not translated from it.

use serde_json::{Map, Value};
use ts_r_ast::{Ast, CommentKind, KeywordTypeKind, Modifiers, Node, NodeId, ParentMap};
use ts_r_span::{LineIndex, Span};

use std::sync::Arc;

use crate::ast_util::{ident_name, skip_parens};
use crate::esquery::{self, Selector};
use crate::estree::{EsTree, Origin};
use crate::jsdoc_tree::JsdocTree;
use crate::rule::Ctx;

/// Recursion limit for the AST helpers below; deeper input is treated as "no".
const MAX_DEPTH: u32 = 400;

/// `settings.jsdoc`.
#[derive(Debug, Clone)]
pub struct Settings {
    pub ignore_private: bool,
    pub ignore_internal: bool,
    pub max_lines: i64,
    pub min_lines: i64,
    pub skip_invoked: bool,
    pub tag_name_preference: Map<String, Value>,
    pub ignore_replaces_docs: bool,
    pub override_replaces_docs: bool,
    pub implements_replaces_docs: bool,
    pub augments_extends_replaces_docs: bool,
    pub exempt_destructured_roots: bool,
    pub mode: String,
    pub contexts: Option<Vec<Value>>,
}

impl Settings {
    pub fn from_cx(cx: &Ctx<'_, '_>) -> Settings {
        let empty = Map::new();
        let s = cx
            .env()
            .settings
            .get("jsdoc")
            .and_then(Value::as_object)
            .unwrap_or(&empty);
        Settings::from_map(s)
    }

    /// The defaults, for checking options before any file is seen.
    pub fn default_for_validation() -> Settings {
        Settings::from_map(&Map::new())
    }

    fn from_map(s: &Map<String, Value>) -> Settings {
        let flag = |k: &str| s.get(k).and_then(Value::as_bool);
        Settings {
            ignore_private: flag("ignorePrivate").unwrap_or(false),
            ignore_internal: flag("ignoreInternal").unwrap_or(false),
            max_lines: s.get("maxLines").and_then(Value::as_i64).unwrap_or(1),
            min_lines: s.get("minLines").and_then(Value::as_i64).unwrap_or(0),
            skip_invoked: flag("skipInvokedExpressionsForCommentFinding").unwrap_or(false),
            tag_name_preference: s
                .get("tagNamePreference")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default(),
            ignore_replaces_docs: flag("ignoreReplacesDocs") != Some(false),
            override_replaces_docs: flag("overrideReplacesDocs") != Some(false),
            implements_replaces_docs: flag("implementsReplacesDocs") != Some(false),
            augments_extends_replaces_docs: flag("augmentsExtendsReplacesDocs") == Some(true),
            exempt_destructured_roots: flag("exemptDestructuredRootsFromChecks").unwrap_or(false),
            mode: s
                .get("mode")
                .and_then(Value::as_str)
                .unwrap_or("typescript")
                .to_owned(),
            contexts: s.get("contexts").and_then(Value::as_array).cloned(),
        }
    }
}

/// The error the plugin raises for contradictory `settings.jsdoc.structuredTags` (a tag
/// whose `name` or `type` is `false` but `required`), in entry order.
pub fn structured_tags_error(cx: &Ctx<'_, '_>) -> Option<&'static str> {
    let tags = cx
        .env()
        .settings
        .get("jsdoc")?
        .get("structuredTags")?
        .as_object()?;
    for v in tags.values() {
        let off = |k: &str| v.get(k) == Some(&Value::Bool(false));
        let required = |k: &str| {
            v.get("required")
                .and_then(Value::as_array)
                .is_some_and(|r| r.iter().any(|x| x.as_str() == Some(k)))
        };
        if required("name") && off("name") {
            return Some("Cannot add \"name\" to `require` with the tag's `name` set to `false`");
        }
        if required("type") && off("type") {
            return Some("Cannot add \"type\" to `require` with the tag's `type` set to `false`");
        }
        if required("typeOrNameRequired") && off("name") {
            return Some(
                "Cannot add \"typeOrNameRequired\" to `require` with the tag's `name` set to `false`",
            );
        }
        if required("typeOrNameRequired") && off("type") {
            return Some(
                "Cannot add \"typeOrNameRequired\" to `require` with the tag's `type` set to `false`",
            );
        }
    }
    None
}

/// Whether the jsdoc settings load; if not, reports the problem at 1:2 (as the plugin does
/// once per rule, which then checks nothing).
pub fn settings_load(cx: &mut Ctx<'_, '_>) -> bool {
    match structured_tags_error(cx) {
        None => true,
        Some(msg) => {
            cx.report(Span::at(0), msg)
                .loc(crate::diagnostic::Loc::at(1, 2));
            false
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Block model (the `comment-parser` tokens).

/// The pieces of one source line of a block. Concatenated in field order they give the line.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Tokens {
    pub start: String,
    pub delimiter: String,
    pub post_delimiter: String,
    pub tag: String,
    pub post_tag: String,
    pub typ: String,
    pub post_type: String,
    pub name: String,
    pub post_name: String,
    pub description: String,
    pub end: String,
    pub line_end: String,
}

#[derive(Debug, Clone)]
pub struct Line {
    /// Line index within the comment (0 is the `/**` line).
    pub number: i64,
    /// The raw text of the line.
    pub raw: String,
    pub tokens: Tokens,
}

#[derive(Debug, Clone)]
pub struct Tag {
    /// Without the `@`.
    pub tag: String,
    pub name: String,
    pub typ: String,
    pub description: String,
    /// Indexes into [`Block::lines`].
    pub source: Vec<usize>,
    /// comment-parser's critical problems (`unpaired brackets`, `empty name`, ...).
    pub problems: Vec<&'static str>,
}

/// A parsed `/** ... */` block. `lines` is an arena: the block's lines, in order, are
/// `source`, and every tag's lines are a run of the same arena entries.
#[derive(Debug, Clone, Default)]
pub struct Block {
    pub lines: Vec<Line>,
    pub source: Vec<usize>,
    pub tags: Vec<Tag>,
}

fn is_ws(c: char) -> bool {
    c.is_whitespace() || c == '\u{feff}'
}

fn split_space(s: &str) -> (String, String) {
    let rest = s.trim_start_matches(is_ws);
    (s[..s.len() - rest.len()].to_owned(), rest.to_owned())
}

/// Drops the first character.
fn drop_first(s: &str) -> &str {
    let mut it = s.chars();
    it.next();
    it.as_str()
}

/// Drops the last character.
fn drop_last(s: &str) -> &str {
    let mut it = s.chars();
    it.next_back();
    it.as_str()
}

/// `s[..n]` / `s[n..]` with JavaScript's forgiving bounds (clamped to the string, rounded down
/// to a character boundary).
fn clamp(s: &str, n: usize) -> usize {
    let mut n = n.min(s.len());
    while !s.is_char_boundary(n) {
        n -= 1;
    }
    n
}

/// A tag-starting description: `@name` followed by whitespace or the end, no `/` in it.
fn starts_tag(d: &str) -> bool {
    let Some(rest) = d.strip_prefix('@') else {
        return false;
    };
    let end = rest.find(is_ws).unwrap_or(rest.len());
    end > 0 && !rest[..end].contains('/')
}

const NO_TYPES: &[&str] = &[
    "default",
    "defaultvalue",
    "description",
    "example",
    "file",
    "fileoverview",
    "license",
    "overview",
    "see",
    "summary",
];

const NO_NAMES: &[&str] = &[
    "access",
    "author",
    "default",
    "defaultvalue",
    "description",
    "example",
    "exception",
    "file",
    "fileoverview",
    "kind",
    "license",
    "overview",
    "return",
    "returns",
    "since",
    "summary",
    "throws",
    "version",
    "variation",
];

impl Block {
    /// Parses the text of a block comment (`/** ... */`).
    pub fn parse(text: &str) -> Block {
        let mut b = Block::default();
        let mut open = false;
        for (num, raw) in text.split('\n').enumerate() {
            let mut t = Tokens::default();
            let body = raw.trim_end_matches('\r');
            t.line_end = raw[body.len()..].to_owned();
            let (start, rest) = split_space(body);
            t.start = start;
            let mut rest = rest;
            if !open && rest.starts_with("/**") && !rest.starts_with("/***") {
                open = true;
                t.delimiter = "/**".to_owned();
                (t.post_delimiter, rest) = split_space(&rest[3..]);
            }
            if !open {
                continue;
            }
            let closed = rest.trim_end().ends_with("*/");
            if t.delimiter.is_empty() && rest.starts_with('*') && !rest.starts_with("*/") {
                t.delimiter = "*".to_owned();
                (t.post_delimiter, rest) = split_space(&rest[1..]);
            }
            if closed {
                let trimmed_len = rest.trim_end().len();
                t.end = rest[trimmed_len - 2..].to_owned();
                rest = rest[..trimmed_len - 2].to_owned();
            }
            t.description = rest;
            b.source.push(b.lines.len());
            b.lines.push(Line {
                number: num as i64,
                raw: raw.to_owned(),
                tokens: t,
            });
            if closed {
                break;
            }
        }
        // Sections: the description, then one per tag.
        let mut sections: Vec<Vec<usize>> = vec![Vec::new()];
        let mut fenced = false;
        for &id in &b.source {
            let d = &b.lines[id].tokens.description;
            if starts_tag(d) && !fenced {
                sections.push(vec![id]);
            } else if let Some(last) = sections.last_mut() {
                last.push(id);
            }
            if d.matches("```").count() % 2 == 1 {
                fenced = !fenced;
            }
        }
        for sec in sections.into_iter().skip(1) {
            let tag = parse_spec(&mut b.lines, sec);
            b.tags.push(tag);
        }
        b
    }

    pub fn stringify(&self) -> String {
        let mut out = String::new();
        for (i, &id) in self.source.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            let t = &self.lines[id].tokens;
            for p in [
                &t.start,
                &t.delimiter,
                &t.post_delimiter,
                &t.tag,
                &t.post_tag,
                &t.typ,
                &t.post_type,
                &t.name,
                &t.post_name,
                &t.description,
                &t.end,
                &t.line_end,
            ] {
                out.push_str(p);
            }
        }
        out
    }

    /// Case-insensitive tag test (`hasTag`).
    pub fn has_tag(&self, name: &str) -> bool {
        self.tags.iter().any(|t| t.tag.eq_ignore_ascii_case(name))
    }

    pub fn has_any_tag(&self, names: &[&str]) -> bool {
        names.iter().any(|n| self.has_tag(n))
    }

    /// Tags named exactly `name`.
    pub fn tags_named<'b>(&'b self, name: &'b str) -> impl Iterator<Item = (usize, &'b Tag)> {
        self.tags
            .iter()
            .enumerate()
            .filter(move |(_, t)| t.tag == name)
    }

    /// Line number (within the comment) of a tag's first line.
    pub fn tag_line(&self, tag: usize) -> i64 {
        self.tags[tag]
            .source
            .first()
            .map_or(0, |&id| self.lines[id].number)
    }

    /// Adds a new line for the arena and returns its id.
    pub fn new_line(&mut self, number: i64, tokens: Tokens) -> usize {
        self.lines.push(Line {
            number,
            raw: String::new(),
            tokens,
        });
        self.lines.len() - 1
    }
}

/// The tag, type, name and description tokenizers on one tag section.
fn parse_spec(lines: &mut [Line], source: Vec<usize>) -> Tag {
    let mut spec = Tag {
        tag: String::new(),
        name: String::new(),
        typ: String::new(),
        description: String::new(),
        source,
        problems: Vec::new(),
    };
    // Tag.
    {
        let t = &mut lines[spec.source[0]].tokens;
        let d = t.description.clone();
        let end = d.find(is_ws).unwrap_or(d.len());
        let after = d[end..].trim_start_matches(is_ws);
        let tag = &d[..end];
        if !tag.contains('/') {
            t.tag = tag.to_owned();
            t.post_tag = d[end..d.len() - after.len()].to_owned();
            t.description = after.to_owned();
            spec.tag = tag[1..].to_owned();
        }
    }
    // Type.
    if !NO_TYPES.contains(&spec.tag.as_str()) && !tokenize_type(lines, &mut spec) {
        return spec;
    }
    // Name.
    if spec.tag == "template" {
        tokenize_template_name(lines, &mut spec);
    } else {
        let skip_name = NO_NAMES.contains(&spec.tag.as_str())
            || (spec.tag == "see" && has_link(&lines[spec.source[0]].raw));
        if !skip_name && !tokenize_name(lines, &mut spec) {
            return spec;
        }
    }
    // Description (the "preserve" joiner).
    let mut ids: &[usize] = &spec.source;
    if let Some(&first) = ids.first() {
        let t = &lines[first].tokens;
        if t.description.is_empty() && t.delimiter == "/**" {
            ids = &ids[1..];
        }
    }
    if let Some(&last) = ids.last() {
        let t = &lines[last].tokens;
        if t.description.is_empty() && t.end.ends_with("*/") {
            ids = &ids[..ids.len() - 1];
        }
    }
    let from = ids.iter().enumerate().fold(0, |n, (i, &id)| {
        if lines[id].tokens.typ.is_empty() {
            n
        } else {
            i
        }
    });
    let ids = &ids[from.min(ids.len())..];
    let parts: Vec<String> = ids
        .iter()
        .map(|&id| {
            let t = &lines[id].tokens;
            let lead = if t.delimiter.is_empty() {
                t.start.as_str()
            } else {
                drop_first(&t.post_delimiter)
            };
            format!("{lead}{}", t.description)
        })
        .collect();
    spec.description = parts.join("\n");
    spec
}

fn has_link(raw: &str) -> bool {
    let Some(i) = raw.find("{@link") else {
        return false;
    };
    let rest = &raw[i + 6..];
    let mut chars = rest.char_indices();
    match chars.next() {
        Some((_, c)) if c != '\n' && c != '\r' => {}
        _ => return false,
    }
    for (_, c) in chars {
        if c == '}' {
            return true;
        }
        if c == '\n' || c == '\r' || c == '\u{2028}' || c == '\u{2029}' {
            return false;
        }
    }
    false
}

/// Returns false on a critical problem (unpaired curlies).
fn tokenize_type(lines: &mut [Line], spec: &mut Tag) -> bool {
    let mut curlies = 0i32;
    let mut parts: Vec<(usize, String)> = Vec::new();
    let mut begun = false;
    let mut first = true;
    for &id in &spec.source {
        let desc = &lines[id].tokens.description;
        if !begun && !desc.trim().is_empty() {
            begun = true;
        } else if !begun {
            continue;
        }
        if first && !desc.starts_with('{') {
            return true;
        }
        first = false;
        let mut ty = String::new();
        for ch in desc.chars() {
            if ch == '{' {
                curlies += 1;
            }
            if ch == '}' {
                curlies -= 1;
            }
            ty.push(ch);
            if curlies == 0 {
                break;
            }
        }
        parts.push((id, ty));
        if curlies == 0 {
            break;
        }
    }
    if !begun {
        return true;
    }
    if curlies != 0 {
        spec.problems.push("unpaired curlies");
        return false;
    }
    let offset = lines[parts[0].0].tokens.post_delimiter.len();
    let mut joined = Vec::with_capacity(parts.len());
    for (i, (id, ty)) in parts.iter().enumerate() {
        let t = &mut lines[*id].tokens;
        t.typ = ty.clone();
        if i > 0 {
            let pd = std::mem::take(&mut t.post_delimiter);
            let cut = clamp(&pd, offset);
            t.typ = format!("{}{ty}", &pd[cut..]);
            t.post_delimiter = pd[..cut].to_owned();
        }
        let rest = t.description[ty.len()..].to_owned();
        (t.post_type, t.description) = split_space(&rest);
        joined.push(t.typ.clone());
    }
    if let Some(f) = joined.first_mut() {
        *f = drop_first(f).to_owned();
    }
    if let Some(l) = joined.last_mut() {
        *l = drop_last(l).to_owned();
    }
    spec.typ = joined.join("\n");
    true
}

fn is_quoted(s: &str) -> bool {
    !s.is_empty() && s.starts_with('"') && s.ends_with('"')
}

/// `=` not followed by `>`.
fn find_eq(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    (0..b.len()).find(|&i| b[i] == b'=' && b.get(i + 1) != Some(&b'>'))
}

fn identifier_eq_prefix(s: &str) -> bool {
    let mut it = s.chars();
    match it.next() {
        Some(c) if c.is_alphabetic() || matches!(c, '$' | '_' | '.') => {}
        _ => return false,
    }
    for c in it {
        if c == '=' {
            return true;
        }
        if !(c.is_alphanumeric() || matches!(c, '$' | '_' | '.' | '\u{200c}' | '\u{200d}')) {
            return false;
        }
    }
    false
}

/// Returns false on a critical problem.
fn tokenize_name(lines: &mut [Line], spec: &mut Tag) -> bool {
    let mut fin = spec.source.iter().enumerate().fold(0, |n, (i, &id)| {
        if lines[id].tokens.typ.is_empty() {
            n
        } else {
            i
        }
    });
    if !spec.typ.is_empty() {
        while fin + 1 < spec.source.len()
            && lines[spec.source[fin]].tokens.description.trim().is_empty()
        {
            fin += 1;
        }
    }
    let t = &mut lines[spec.source[fin]].tokens;
    let source = t.description.trim_start_matches(is_ws).to_owned();
    let groups: Vec<&str> = source.split('"').collect();
    if groups.len() > 1 && groups[0].is_empty() && groups.len() % 2 == 1 {
        spec.name = groups[1].to_owned();
        t.name = format!("\"{}\"", groups[1]);
        (t.post_name, t.description) = split_space(&source[t.name.len()..]);
        return true;
    }
    let mut brackets = 0i32;
    let mut name = String::new();
    for ch in source.chars() {
        if brackets == 0 && is_ws(ch) {
            break;
        }
        if ch == '[' {
            brackets += 1;
        }
        if ch == ']' {
            brackets -= 1;
        }
        name.push(ch);
    }
    if brackets != 0 {
        spec.problems.push("unpaired brackets");
        return false;
    }
    let token = name.clone();
    let mut optional = false;
    if name.starts_with('[') && name.ends_with(']') && name.len() >= 2 {
        optional = true;
        let inner = name[1..name.len() - 1].to_owned();
        let mut parts = inner.split('=');
        name = parts.next().unwrap_or("").trim().to_owned();
        let rest: Vec<&str> = parts.collect();
        let default = (!rest.is_empty()).then(|| rest.join("=").trim().to_owned());
        if name.is_empty() {
            spec.problems.push("empty name");
            return false;
        }
        if let Some(d) = &default {
            if d.is_empty() {
                spec.problems.push("empty default value");
                return false;
            }
            if !is_quoted(d) && find_eq(d).is_some() {
                spec.problems.push("invalid default value syntax");
                return false;
            }
        }
    }
    if !optional && let Some(eq) = find_eq(&name) {
        let pre = name[..eq].trim().to_owned();
        let post = name[eq + 1..].trim().to_owned();
        if pre.is_empty() {
            spec.problems.push("empty name");
            return false;
        }
        if post.is_empty() {
            spec.problems.push("empty default value");
            return false;
        }
        if identifier_eq_prefix(&name) {
            if !is_quoted(&post) && find_eq(&post).is_some() {
                spec.problems.push("invalid default value syntax");
                return false;
            }
            name = pre;
        }
    }
    spec.name = name;
    t.name = token;
    (t.post_name, t.description) = split_space(&source[t.name.len()..]);
    true
}

fn tokenize_template_name(lines: &mut [Line], spec: &mut Tag) {
    let t = &mut lines[spec.source[0]].tokens;
    let rem = t.description.clone();
    // A whitespace character not preceded by whitespace or a comma.
    let search = |s: &str| -> Option<usize> {
        let mut prev: Option<char> = None;
        for (i, c) in s.char_indices() {
            if is_ws(c) && !prev.is_some_and(|p| is_ws(p) || p == ',') {
                return Some(i);
            }
            prev = Some(c);
        }
        None
    };
    let pos = match rem.rfind(']') {
        Some(e) if rem.starts_with('[') => search(&rem[e..]).map(|p| p + e),
        _ => search(&rem),
    };
    let name = pos.map_or(rem.as_str(), |p| &rem[..p]).to_owned();
    let (mut post_name, mut description, mut line_end) = (String::new(), String::new(), "");
    if let Some(p) = pos {
        let extra = &rem[p..];
        let body = extra.trim_start_matches(is_ws);
        post_name = extra[..extra.len() - body.len()].to_owned();
        let d_end = body.find('\r').unwrap_or(body.len());
        description = body[..d_end].to_owned();
        if body[d_end..].starts_with('\r') {
            line_end = "\r";
        }
    }
    spec.name = name.clone();
    t.name = name;
    t.post_name = post_name;
    t.description = description;
    if !line_end.is_empty() {
        t.line_end = line_end.to_owned();
    }
}

// ---------------------------------------------------------------------------------------------
// Finding the block of a node.

#[derive(Debug, Clone, Copy)]
enum Before {
    None,
    Comment(usize),
    /// Offset of a `(` token.
    Paren(u32),
    Other,
}

/// The node a rule sees: a node, or the `export` declaration wrapping an exported
/// declaration (ESTree has a separate node for it; our declarations carry the modifier).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Node(NodeId),
    Export(NodeId),
}

impl Target {
    pub fn id(self) -> NodeId {
        match self {
            Target::Node(id) | Target::Export(id) => id,
        }
    }
}

#[derive(Debug)]
pub struct Finder<'a> {
    pub ast: &'a Ast,
    pub src: &'a str,
    pub parents: &'a ParentMap,
    pub lines: &'a LineIndex,
    pub settings: &'a Settings,
}

impl<'a> Finder<'a> {
    pub fn new(cx: &Ctx<'_, 'a>, settings: &'a Settings) -> Finder<'a> {
        Finder {
            ast: cx.file.ast,
            src: cx.file.src,
            parents: cx.file.parents,
            lines: cx.file.lines,
            settings,
        }
    }

    fn token_before(&self, pos: u32) -> Before {
        let head = &self.src[..clamp(self.src, pos as usize)];
        let t = head.trim_end_matches(is_ws);
        if t.is_empty() {
            return Before::None;
        }
        let end = t.len() as u32;
        let cs = &self.ast.comments;
        let i = cs.partition_point(|c| c.span.end < end);
        if i < cs.len() && cs[i].span.end == end && cs[i].span.start < end {
            return Before::Comment(i);
        }
        if t.ends_with('(') {
            return Before::Paren(end - 1);
        }
        Before::Other
    }

    /// The parent, skipping parentheses (ESTree has no node for them).
    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        let mut p = self.parents.parent(id)?;
        while matches!(self.ast.node(p), Node::Paren { .. }) {
            p = self.parents.parent(p)?;
        }
        Some(p)
    }

    fn decorators(&self, id: NodeId) -> Option<NodeId> {
        match *self.ast.node(id) {
            Node::ClassDecl { decorators, .. }
            | Node::ClassExpr { decorators, .. }
            | Node::MethodDef { decorators, .. }
            | Node::PropertyDef { decorators, .. }
            | Node::Param { decorators, .. } => self.ast.list(decorators).first().copied(),
            _ => None,
        }
    }

    /// Where a node starts for comment lookup: its first decorator (or its parent's), else
    /// itself.
    pub fn start_of(&self, id: NodeId) -> u32 {
        let own = self.ast.span(id).start;
        let dec = self
            .decorators(id)
            .or_else(|| self.parents.parent(id).and_then(|p| self.decorators(p)));
        dec.map_or(own, |d| self.ast.span(d).start.min(own))
    }

    /// The JSDoc comment (index into `ast.comments`) of `id` (`getJSDocComment`).
    pub fn jsdoc_of(&self, id: NodeId) -> Option<usize> {
        self.find(self.reduce(id))
    }

    /// The JSDoc of an exported declaration's `export` wrapper: the same position as the
    /// declaration (its span includes the modifiers).
    pub fn jsdoc_of_target(&self, t: Target) -> Option<usize> {
        match t {
            Target::Node(id) => self.jsdoc_of(id),
            Target::Export(id) => self.find(id),
        }
    }

    /// The JSDoc block right before `id` (`findJSDocComment`), without reducing it first.
    pub fn find(&self, id: NodeId) -> Option<usize> {
        let mut cur = self.start_of(id);
        let mut paren: Option<u32> = None;
        let cs = &self.ast.comments;
        let found = loop {
            let mut t = self.token_before(cur);
            if let Before::Paren(p) = t {
                paren = Some(p + 1);
                t = self.token_before(p);
            }
            match t {
                Before::Comment(i) if cs[i].kind == CommentKind::Line => {
                    if cs[i].span.start >= cur {
                        return None;
                    }
                    cur = cs[i].span.start;
                }
                Before::Comment(i) => break i,
                _ => return None,
            }
        };
        let c = &cs[found];
        if c.kind != CommentKind::Block || !is_jsdoc_text(c.span.text(self.src)) {
            return None;
        }
        let node_line = i64::from(self.lines.line_of(cur));
        let end_line = i64::from(self.lines.line_of(paren.unwrap_or(c.span.end)));
        let d = node_line - end_line;
        (d >= self.settings.min_lines && d <= self.settings.max_lines).then_some(found)
    }

    fn has_comment_before(&self, id: NodeId) -> bool {
        matches!(
            self.token_before(self.ast.span(id).start),
            Before::Comment(_)
        )
    }

    /// The node whose comment documents `id` (`getReducedASTNode`).
    pub fn reduce(&self, id: NodeId) -> NodeId {
        match self.ast.node(id) {
            Node::FunctionDecl { body: None, .. }
            | Node::ClassExpr { .. }
            | Node::ObjectLit { .. }
            | Node::Arrow { .. }
            | Node::FunctionExpr { .. } => self.reduce_expr(id),
            Node::FunctionType { .. } => self.reduce_fn_type(id),
            _ => id,
        }
    }

    /// A function type written as an annotation documents what it annotates: a property,
    /// a method or declared function's return type, a method's function, or the variable
    /// declaration of an annotated variable or of an arrow / function declaration parameter.
    /// (`@es-joy/jsdoccomment: src/jsdoccomment.js`, `getTSFunctionComment`.)
    fn reduce_fn_type(&self, id: NodeId) -> NodeId {
        let mut child = id;
        let mut p = self.parent(id);
        let mut guard = 0;
        while let Some(q) = p
            && matches!(self.ast.node(q), Node::ParenType { .. })
            && guard < 1000
        {
            guard += 1;
            child = q;
            p = self.parent(q);
        }
        let Some(p) = p else { return id };
        let is = |o: Option<NodeId>| o == Some(child);
        let var_decl_of = |declarator: NodeId| {
            self.parent(declarator)
                .filter(|&d| matches!(self.ast.node(d), Node::VarDecl { .. }))
        };
        match *self.ast.node(p) {
            Node::TypeAlias { ty, .. } if ty == child && self.is_exported_decl(p) => p,
            Node::PropertyDef { type_ann, .. } | Node::PropertySignature { type_ann, .. }
                if is(type_ann) =>
            {
                p
            }
            Node::MethodSignature { return_type, .. } if is(return_type) => p,
            Node::FunctionDecl {
                return_type,
                body: None,
                ..
            } if is(return_type) => p,
            Node::FunctionExpr { return_type, .. } if is(return_type) => self
                .parent(p)
                .filter(|&m| matches!(self.ast.node(m), Node::MethodDef { .. }))
                .unwrap_or(id),
            Node::VarDeclarator { type_ann, .. } if is(type_ann) => var_decl_of(p).unwrap_or(id),
            // An arrow's return type in a variable's initializer borrows the comment of what
            // holds the declaration (the plugin's walk goes one level past it); the
            // program starts at its first token, which is its first statement's.
            Node::Arrow { return_type, .. } if is(return_type) => {
                let Some(holder) = self
                    .parent(p)
                    .filter(|&d| matches!(self.ast.node(d), Node::VarDeclarator { .. }))
                    .and_then(var_decl_of)
                    .and_then(|d| self.parent(d))
                else {
                    return id;
                };
                match *self.ast.node(holder) {
                    Node::Program { stmts } => {
                        self.ast.list(stmts).first().copied().unwrap_or(holder)
                    }
                    _ => holder,
                }
            }
            Node::Param {
                type_ann, pattern, ..
            } if is(type_ann) && matches!(self.ast.node(pattern), Node::Ident { .. }) => {
                let Some(f) = self.parent(p) else { return id };
                match self.ast.node(f) {
                    Node::FunctionDecl { .. } => f,
                    Node::Arrow { .. } => self
                        .parent(f)
                        .filter(|&d| matches!(self.ast.node(d), Node::VarDeclarator { .. }))
                        .and_then(var_decl_of)
                        .unwrap_or(id),
                    _ => id,
                }
            }
            _ => id,
        }
    }

    fn reduce_expr(&self, id: NodeId) -> NodeId {
        let Some(parent) = self.parent(id) else {
            return id;
        };
        if matches!(self.ast.node(parent), Node::Call { .. } | Node::New { .. })
            && !self.settings.skip_invoked
        {
            return id;
        }
        let start = self.ast.span(id).start;
        let mut pos = start;
        loop {
            match self.token_before(pos) {
                Before::Paren(p) if p < pos => pos = p,
                Before::Comment(i) if self.ast.comments[i].kind == CommentKind::Block => {
                    return id;
                }
                _ => break,
            }
        }
        if matches!(self.token_before(start), Before::Comment(_)) {
            return id;
        }
        let mut p = Some(parent);
        let mut guard = 0;
        while let Some(q) = p {
            guard += 1;
            if guard > 10_000
                || self.has_comment_before(q)
                || self.is_function_like(q)
                || self.allowable(q)
            {
                break;
            }
            p = self.parent(q);
        }
        match p {
            Some(q)
                if !matches!(
                    self.ast.node(q),
                    Node::FunctionDecl { .. } | Node::Program { .. }
                ) =>
            {
                q
            }
            _ => id,
        }
    }

    fn is_function_like(&self, id: NodeId) -> bool {
        matches!(
            self.ast.node(id),
            Node::FunctionDecl { .. }
                | Node::FunctionExpr { .. }
                | Node::Arrow { .. }
                | Node::FunctionType { .. }
        )
    }

    fn allowable(&self, id: NodeId) -> bool {
        match *self.ast.node(id) {
            Node::AssignPattern { .. }
            | Node::VarDecl { .. }
            | Node::ExprStmt { .. }
            | Node::Property { .. }
            | Node::BindingProperty { .. }
            | Node::ExportDefaultExpr { .. }
            | Node::Return { .. } => true,
            Node::Param { init, .. } => init.is_some(),
            Node::MethodDef { modifiers, .. } => !modifiers.contains(Modifiers::ABSTRACT),
            Node::PropertyDef { modifiers, .. } => {
                !modifiers.intersects(Modifiers::ABSTRACT | Modifiers::ACCESSOR)
            }
            _ => false,
        }
    }

    /// True when `id` is a method in an object literal (an ESTree `Property`).
    pub fn in_object_literal(&self, id: NodeId) -> bool {
        self.parents
            .parent(id)
            .is_some_and(|p| matches!(self.ast.node(p), Node::ObjectLit { .. }))
    }

    /// The ESTree type name of a node, for `contexts`.
    pub fn es_type(&self, t: Target) -> &'static str {
        let id = match t {
            Target::Export(id) => {
                return if self.exported_default(id) {
                    "ExportDefaultDeclaration"
                } else {
                    "ExportNamedDeclaration"
                };
            }
            Target::Node(id) => id,
        };
        match *self.ast.node(id) {
            Node::FunctionDecl { body: Some(_), .. } => "FunctionDeclaration",
            Node::FunctionDecl { body: None, .. } => "TSDeclareFunction",
            Node::FunctionExpr { body: Some(_), .. } => "FunctionExpression",
            Node::FunctionExpr { body: None, .. } => "TSEmptyBodyFunctionExpression",
            Node::Arrow { .. } => "ArrowFunctionExpression",
            Node::ClassDecl { .. } => "ClassDeclaration",
            Node::ClassExpr { .. } => "ClassExpression",
            Node::MethodDef { modifiers, .. } => {
                if self.in_object_literal(id) {
                    "Property"
                } else if modifiers.contains(Modifiers::ABSTRACT) {
                    "TSAbstractMethodDefinition"
                } else {
                    "MethodDefinition"
                }
            }
            Node::PropertyDef { modifiers, .. } => {
                if modifiers.contains(Modifiers::ABSTRACT) {
                    "TSAbstractPropertyDefinition"
                } else if modifiers.contains(Modifiers::ACCESSOR) {
                    "AccessorProperty"
                } else {
                    "PropertyDefinition"
                }
            }
            Node::Property { .. } | Node::ShorthandProp { .. } | Node::BindingProperty { .. } => {
                "Property"
            }
            Node::VarDecl { .. } => "VariableDeclaration",
            Node::VarDeclarator { .. } => "VariableDeclarator",
            Node::ExprStmt { .. } => "ExpressionStatement",
            Node::ObjectLit { .. } => "ObjectExpression",
            Node::Interface { .. } => "TSInterfaceDeclaration",
            Node::TypeAlias { .. } => "TSTypeAliasDeclaration",
            Node::Enum { .. } => "TSEnumDeclaration",
            Node::Module { .. } => "TSModuleDeclaration",
            Node::FunctionType { .. } => "TSFunctionType",
            Node::MethodSignature { .. } => "TSMethodSignature",
            Node::PropertySignature { .. } => "TSPropertySignature",
            Node::CallSignature { .. } => "TSCallSignatureDeclaration",
            Node::ConstructSignature { .. } => "TSConstructSignatureDeclaration",
            Node::TypeLiteral { .. } => "TSTypeLiteral",
            Node::ExportDefaultExpr { .. } => "ExportDefaultDeclaration",
            Node::ExportNamed { .. } => "ExportNamedDeclaration",
            Node::Return { .. } => "ReturnStatement",
            Node::Ident { .. } => "Identifier",
            Node::Call { .. } => "CallExpression",
            Node::New { .. } => "NewExpression",
            Node::Assign { .. } => "AssignmentExpression",
            Node::Member { .. } => "MemberExpression",
            Node::Block { .. } => "BlockStatement",
            _ => "",
        }
    }

    fn decl_modifiers(&self, id: NodeId) -> Option<Modifiers> {
        match *self.ast.node(id) {
            Node::FunctionDecl { modifiers, .. }
            | Node::ClassDecl { modifiers, .. }
            | Node::VarDecl { modifiers, .. }
            | Node::Interface { modifiers, .. }
            | Node::TypeAlias { modifiers, .. }
            | Node::Enum { modifiers, .. }
            | Node::Module { modifiers, .. } => Some(modifiers),
            _ => None,
        }
    }

    /// A declaration with `export` (ESTree wraps it in an export node).
    pub fn is_exported_decl(&self, id: NodeId) -> bool {
        self.decl_modifiers(id)
            .is_some_and(|m| m.intersects(Modifiers::EXPORT | Modifiers::DEFAULT))
    }

    fn exported_default(&self, id: NodeId) -> bool {
        self.decl_modifiers(id)
            .is_some_and(|m| m.contains(Modifiers::DEFAULT))
    }

    /// The nearest class around (or at) `id`.
    pub fn enclosing_class(&self, id: NodeId) -> Option<NodeId> {
        let mut cur = Some(id);
        let mut guard = 0;
        while let Some(c) = cur {
            guard += 1;
            if guard > 10_000 {
                return None;
            }
            if matches!(
                self.ast.node(c),
                Node::ClassDecl { .. } | Node::ClassExpr { .. }
            ) {
                return Some(c);
            }
            cur = self.parents.parent(c);
        }
        None
    }

    /// The parsed JSDoc of the class around `t` (`classHasTag`).
    pub fn class_block(&self, t: Option<Target>) -> Option<Block> {
        let class = self.enclosing_class(t?.id())?;
        let i = self.jsdoc_of(class)?;
        Some(Block::parse(self.ast.comments[i].span.text(self.src)))
    }

    /// The parameters ESLint's plugin reads off a node (`getFunctionParameterNames`).
    pub fn params(&self, t: Option<Target>) -> Vec<NodeId> {
        let Some(Target::Node(id)) = t else {
            return Vec::new();
        };
        let fn_params = |f: NodeId| -> Option<Vec<NodeId>> {
            match *self.ast.node(f) {
                Node::FunctionDecl { params, .. }
                | Node::FunctionExpr { params, .. }
                | Node::Arrow { params, .. }
                | Node::FunctionType { params, .. }
                | Node::ConstructorType { params, .. }
                | Node::MethodSignature { params, .. }
                | Node::CallSignature { params, .. }
                | Node::ConstructSignature { params, .. } => Some(self.ast.list(params).to_vec()),
                _ => None,
            }
        };
        if let Some(p) = fn_params(id) {
            return p;
        }
        match *self.ast.node(id) {
            Node::PropertySignature {
                type_ann: Some(ty), ..
            } => {
                if let Node::FunctionType { params, .. } = *self.ast.node(ty) {
                    return self.ast.list(params).to_vec();
                }
                Vec::new()
            }
            Node::PropertyDef {
                type_ann, value, ..
            } => {
                if let Some(ty) = type_ann
                    && let Node::FunctionType { params, .. } = *self.ast.node(ty)
                {
                    return self.ast.list(params).to_vec();
                }
                value.and_then(fn_params).unwrap_or_default()
            }
            Node::MethodDef { value, .. } | Node::Property { value, .. } => {
                fn_params(value).unwrap_or_default()
            }
            _ => Vec::new(),
        }
    }

    /// True for a parameter the plugin treats as a destructured root.
    pub fn is_destructured(&self, param: NodeId) -> bool {
        match *self.ast.node(param) {
            Node::Param {
                rest,
                pattern,
                type_ann,
                ..
            } => {
                if type_ann.is_some_and(|t| matches!(self.ast.node(t), Node::TypeLiteral { .. })) {
                    return true;
                }
                !rest
                    && matches!(
                        self.ast.node(pattern),
                        Node::ObjectPattern { .. } | Node::ArrayPattern { .. }
                    )
            }
            Node::ObjectPattern { .. } | Node::ArrayPattern { .. } => true,
            _ => false,
        }
    }
}

/// A block comment whose value starts `*` and whitespace.
pub fn is_jsdoc_text(text: &str) -> bool {
    let Some(rest) = text.strip_prefix("/**") else {
        return false;
    };
    let value = rest.strip_suffix("*/").unwrap_or(rest);
    // The value is `*` + rest; `/**/` has value "" and `/***/` has "*".
    let full_value = &text[2..text.len().saturating_sub(2).max(2)];
    full_value.starts_with('*') && value.chars().next().is_some_and(is_ws)
}

// ---------------------------------------------------------------------------------------------
// Contexts and iteration.

#[derive(Debug, Clone, Default)]
pub struct ContextEntry {
    /// The ESTree type; `None` is `any`. For a selector context, its text.
    pub ty: Option<String>,
    pub is_object: bool,
    pub force_require_return: bool,
    /// The parsed context when it is a selector rather than a plain node type.
    pub selector: Option<Arc<Selector>>,
    /// The `comment` selector an object entry matches the JSDoc block against.
    pub comment: Option<Arc<Selector>>,
    /// An object entry's `message` (the forbid rules report it).
    pub message: Option<String>,
}

impl ContextEntry {
    pub fn comment_raw(&self) -> Option<&str> {
        self.comment.as_deref().map(|s| s.raw.as_str())
    }
}

#[derive(Debug, Clone)]
pub struct Contexts {
    pub entries: Vec<ContextEntry>,
    pub all: bool,
    /// A plain `"any"` entry: every block is checked once, whatever the `comment` filters.
    pub plain_any: bool,
    /// The plugin's `matchContext` under object `any` entries: every block is checked once
    /// per entry whose `comment` matches it (or that has none), not just the first.
    pub additive: bool,
    /// The plugin's `noTracking`: under `any`, a block is checked once for every node it
    /// documents (an export and its declaration both), not just the first.
    pub no_tracking: bool,
}

pub const DEFAULT_CONTEXTS: &[&str] = &[
    "ArrowFunctionExpression",
    "FunctionDeclaration",
    "FunctionExpression",
    "TSDeclareFunction",
];

fn plain_type_name(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric())
}

fn parse_selector(raw: &str) -> Result<Arc<Selector>, String> {
    let mut sel =
        esquery::parse(raw).map_err(|e| format!("jsdoc: invalid selector `{raw}`: {e}"))?;
    sel.raw = raw.to_owned();
    Ok(Arc::new(sel))
}

impl Contexts {
    /// From `options[0].contexts`, then `settings.jsdoc.contexts`, then the defaults: node
    /// types, esquery selectors, `any` and `{context, comment, forceRequireReturn}` objects.
    pub fn from_options(opts: &Map<String, Value>, settings: &Settings) -> Result<Self, String> {
        let from_options = opts.get("contexts").and_then(Value::as_array);
        let list: Vec<Value> = match from_options.or(settings.contexts.as_ref()) {
            Some(l) => l.clone(),
            None => DEFAULT_CONTEXTS.iter().map(|s| Value::from(*s)).collect(),
        };
        let mut entries = Vec::new();
        let mut all = false;
        let mut plain_any = false;
        for v in &list {
            let (ty, is_object, force, comment, message) = match v {
                Value::String(s) => (s.clone(), false, false, None, None),
                Value::Object(o) => {
                    let comment = match o.get("comment") {
                        Some(Value::String(c)) => Some(parse_selector(c)?),
                        None | Some(Value::Null) => None,
                        Some(_) => return Err("jsdoc: `comment` must be a selector".into()),
                    };
                    let ty = o
                        .get("context")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .unwrap_or("any")
                        .to_owned();
                    let force = o
                        .get("forceRequireReturn")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    let message = o.get("message").and_then(Value::as_str).map(str::to_owned);
                    (ty, true, force, comment, message)
                }
                _ => return Err("jsdoc: invalid `contexts` entry".into()),
            };
            if ty == "any" {
                all = true;
                plain_any |= !is_object;
                entries.push(ContextEntry {
                    ty: None,
                    is_object,
                    force_require_return: force,
                    selector: None,
                    comment,
                    message,
                });
                continue;
            }
            let selector = if plain_type_name(&ty) {
                None
            } else {
                Some(parse_selector(&ty)?)
            };
            entries.push(ContextEntry {
                ty: Some(ty),
                is_object,
                force_require_return: force,
                selector,
                comment,
                message,
            });
        }
        Ok(Contexts {
            entries,
            all,
            plain_any,
            additive: false,
            no_tracking: false,
        })
    }

    /// `findContext`: the entry the block was found through ([`Doc::context`]).
    pub fn find(&self, doc: &Doc) -> Option<&ContextEntry> {
        doc.context.and_then(|i| self.entries.get(i))
    }

    fn uses_selectors(&self) -> bool {
        self.entries.iter().any(|e| e.selector.is_some())
    }

    /// The first entry matching the node (with no ancestry, like `esquery.matches` without
    /// parents) whose `comment` is the one the block was found through.
    fn found(
        &self,
        finder: &Finder<'_>,
        tree: Option<&EsTree<'_>>,
        t: Option<Target>,
        comment: Option<&str>,
    ) -> Option<usize> {
        let es = |t: Target| {
            let (id, export) = match t {
                Target::Export(id) => (id, true),
                _ => (t.id(), false),
            };
            tree.and_then(|tr| tr.of_ast(id, export))
        };
        let node_matches = |e: &ContextEntry| match (&e.ty, t) {
            (None, _) | (_, None) => false,
            (Some(name), Some(t)) => match (&e.selector, tree) {
                (Some(sel), Some(tr)) => es(t).is_some_and(|n| sel.matches(tr, n, 0)),
                (Some(_), None) => false,
                (None, _) => finder.es_type(t) == name,
            },
        };
        self.entries.iter().position(|e| {
            if e.is_object {
                (e.ty.is_none() || node_matches(e)) && e.comment_raw() == comment
            } else {
                node_matches(e)
            }
        })
    }
}

/// One block to check.
#[derive(Debug)]
pub struct Item {
    /// `None` for blocks no node claimed (only when iterating over all blocks).
    pub target: Option<Target>,
    pub comment: usize,
    /// The entry `findContext` gives for the block.
    pub context: Option<usize>,
    /// The `comment` selector the block was found through (`info.comment`).
    pub info_comment: Option<String>,
    /// The index of the `contexts` entry the block is checked for (`info.lastIndex`).
    pub last_index: Option<usize>,
}

/// Whether the block's ESTree view matches a `comment` selector (`commentHandler`).
fn comment_matches(finder: &Finder<'_>, comment: usize, sel: &Selector) -> bool {
    let text = finder.ast.comments[comment].span.text(finder.src);
    let tree = JsdocTree::build(&Block::parse(text));
    sel.matches(&tree, 0, 0)
}

/// The (node, block) pairs a rule checks, in visiting order.
pub fn collect(finder: &Finder<'_>, contexts: &Contexts) -> Vec<Item> {
    if !contexts.all
        && (contexts.uses_selectors() || contexts.entries.iter().any(|e| e.comment.is_some()))
    {
        return collect_listeners(finder, contexts);
    }
    let tree = contexts
        .uses_selectors()
        .then(|| EsTree::build(finder.ast, finder.src));
    let tree = tree.as_ref();
    // With object `any` entries, a block is checked through the first entry without a
    // `comment` or whose `comment` matches it; if none, it is skipped.
    // A plain `any` checks every block whatever the other entries say.
    let filtered =
        contexts.all && !contexts.plain_any && contexts.entries.iter().any(|e| e.comment.is_some());
    // With `additive` (and no plain `any`), a block is checked once through every entry
    // without a `comment` or whose `comment` matches it.
    let additive = contexts.all && !contexts.plain_any && contexts.additive;
    // Each entry comes with its index (`info.lastIndex`): the entry it is checked for
    // (additive), or the one the search for a matching `comment` stopped at.
    let via = |c: usize| -> Vec<(Option<usize>, Option<&str>)> {
        if additive {
            return contexts
                .entries
                .iter()
                .enumerate()
                .filter_map(|(i, e)| match &e.comment {
                    None => Some((Some(i), None)),
                    Some(sel) if comment_matches(finder, c, sel) => {
                        Some((Some(i), Some(sel.raw.as_str())))
                    }
                    Some(_) => None,
                })
                .collect();
        }
        if !filtered {
            let first = (contexts.all && !contexts.plain_any).then_some(0);
            return vec![(first, None)];
        }
        for (i, e) in contexts.entries.iter().enumerate() {
            match &e.comment {
                None => return vec![(Some(i), None)],
                Some(sel) if comment_matches(finder, c, sel) => {
                    return vec![(Some(i), Some(&sel.raw))];
                }
                Some(_) => {}
            }
        }
        Vec::new()
    };
    let ast = finder.ast;
    let mut out = Vec::new();
    let mut claimed = vec![false; ast.comments.len()];
    let mut stack = vec![(ast.root, 0u32)];
    let mut kids = Vec::new();
    while let Some((id, depth)) = stack.pop() {
        let is_root = id == ast.root;
        if !is_root {
            let mut targets = [None, Some(Target::Node(id))];
            if finder.is_exported_decl(id) {
                targets[0] = Some(Target::Export(id));
            }
            for t in targets.into_iter().flatten() {
                if contexts.all {
                    if let Some(c) = finder.jsdoc_of_target(t)
                        && (!claimed[c] || contexts.no_tracking)
                    {
                        claimed[c] = true;
                        for (last_index, comment) in via(c) {
                            out.push(Item {
                                target: Some(t),
                                comment: c,
                                context: contexts.found(finder, tree, Some(t), comment),
                                info_comment: comment.map(str::to_owned),
                                last_index,
                            });
                        }
                    }
                } else {
                    let ty = finder.es_type(t);
                    if let Some(last_index) = contexts
                        .entries
                        .iter()
                        .position(|e| e.ty.as_deref() == Some(ty))
                        && let Some(c) = finder.jsdoc_of_target(t)
                    {
                        out.push(Item {
                            target: Some(t),
                            comment: c,
                            context: contexts.found(finder, tree, Some(t), None),
                            info_comment: None,
                            last_index: Some(last_index),
                        });
                    }
                }
            }
        }
        if depth >= MAX_DEPTH * 4 {
            continue;
        }
        kids.clear();
        ast.for_each_child(id, |c| kids.push(c));
        for &c in kids.iter().rev() {
            stack.push((c, depth + 1));
        }
    }
    if contexts.all {
        for (i, c) in ast.comments.iter().enumerate() {
            if !claimed[i] && c.kind == CommentKind::Block && is_jsdoc_text(c.span.text(finder.src))
            {
                for (last_index, comment) in via(i) {
                    out.push(Item {
                        target: None,
                        comment: i,
                        context: contexts.found(finder, tree, None, comment),
                        info_comment: comment.map(str::to_owned),
                        last_index,
                    });
                }
            }
        }
    }
    out
}

/// Contexts as ESLint listeners (`getContextObject`): every ESTree node in traversal order,
/// each distinct context matched in selector-specificity order, and every entry under that
/// context run in option order (a block can be checked more than once).
fn collect_listeners(finder: &Finder<'_>, contexts: &Contexts) -> Vec<Item> {
    let tree = EsTree::build(finder.ast, finder.src);
    let mut owned: Vec<Selector> = Vec::new();
    // (selector index, entries using it)
    let mut keys: Vec<(usize, Vec<usize>)> = Vec::new();
    let mut key_raw: Vec<String> = Vec::new();
    for (i, e) in contexts.entries.iter().enumerate() {
        let Some(raw) = e.ty.as_deref() else { continue };
        if let Some(k) = key_raw.iter().position(|r| r == raw) {
            keys[k].1.push(i);
            continue;
        }
        let Ok(sel) = esquery::parse(raw.strip_suffix(":exit").unwrap_or(raw)) else {
            continue;
        };
        owned.push(sel);
        key_raw.push(raw.to_owned());
        keys.push((owned.len() - 1, vec![i]));
    }
    keys.sort_by(|a, b| owned[a.0].compare(&owned[b.0]));
    let prepared: Vec<_> = owned.iter().map(Selector::prepare).collect();
    let mut out = Vec::new();
    for n in 0..tree.len() as u32 {
        for (sel, entries) in &keys {
            if !prepared[*sel].matches_listener(&tree, n) {
                continue;
            }
            let node = &tree.nodes[n as usize];
            let t = match node.origin {
                Origin::Real => Target::Node(node.ast),
                Origin::Export => Target::Export(node.ast),
                Origin::Synthetic => continue,
            };
            let Some(c) = finder.jsdoc_of_target(t) else {
                continue;
            };
            for &i in entries {
                let e = &contexts.entries[i];
                if let Some(cs) = &e.comment
                    && !comment_matches(finder, c, cs)
                {
                    continue;
                }
                out.push(Item {
                    target: Some(t),
                    comment: c,
                    context: contexts.found(finder, Some(&tree), Some(t), e.comment_raw()),
                    info_comment: e.comment_raw().map(str::to_owned),
                    last_index: Some(i),
                });
            }
        }
    }
    out
}

/// Whether the iteration skips a block (`ignorePrivate` / `ignoreInternal`).
pub fn skipped(settings: &Settings, block: &Block) -> bool {
    if settings.ignore_internal && block.has_tag("internal") {
        return true;
    }
    settings.ignore_private
        && (block.has_tag("private")
            || block
                .tags
                .iter()
                .any(|t| t.tag == "access" && t.description == "private"))
}

/// The tag name to use for `name` after `settings.jsdoc.tagNamePreference`.
#[derive(Debug)]
pub enum Preferred {
    Name(String),
    /// The tag is disallowed; the message to report if the block uses it.
    Blocked(Option<String>),
}

pub fn preferred_tag(settings: &Settings, name: &str) -> Preferred {
    let prefs = &settings.tag_name_preference;
    let is_value = prefs.values().any(|v| match v {
        Value::String(s) => s == name,
        Value::Object(o) => o.get("replacement").and_then(Value::as_str) == Some(name),
        _ => false,
    });
    if is_value {
        return Preferred::Name(name.to_owned());
    }
    let found = prefs
        .iter()
        .find(|(k, _)| k.strip_prefix("tag ").unwrap_or(k) == name)
        .map(|(_, v)| v);
    match found {
        Some(Value::String(s)) => Preferred::Name(s.clone()),
        Some(Value::Bool(false)) => Preferred::Blocked(None),
        Some(Value::Object(o)) => match o.get("replacement").and_then(Value::as_str) {
            Some(r) => Preferred::Name(r.to_owned()),
            None => Preferred::Blocked(o.get("message").and_then(Value::as_str).map(str::to_owned)),
        },
        _ => Preferred::Name(name.to_owned()),
    }
}

/// One block being checked: where it is and helpers to report on it.
#[derive(Debug)]
pub struct Doc {
    pub target: Option<Target>,
    pub comment: Span,
    pub block: Block,
    /// The `contexts` entry the block was found through (`findContext`).
    pub context: Option<usize>,
    /// The `comment` selector the block was found through (`info.comment`).
    pub info_comment: Option<String>,
    /// See [`Item::last_index`].
    pub last_index: Option<usize>,
    /// 0-based line of the comment start.
    pub line: u32,
    /// Whitespace the comment's line starts with, repeated to the comment's column.
    pub indent: String,
}

impl Doc {
    pub fn new(finder: &Finder<'_>, item: &Item) -> Doc {
        let c = &finder.ast.comments[item.comment];
        let text = c.span.text(finder.src);
        let line = finder.lines.line_of(c.span.start);
        let line_start = finder.lines.line_start(line).unwrap_or(c.span.start);
        let prefix = &finder.src[line_start as usize..c.span.start as usize];
        let col = prefix.encode_utf16().count();
        let ch = prefix
            .chars()
            .next()
            .filter(|c| matches!(c, ' ' | '\t'))
            .unwrap_or(' ');
        Doc {
            target: item.target,
            comment: c.span,
            context: item.context,
            info_comment: item.info_comment.clone(),
            last_index: item.last_index,
            block: Block::parse(text),
            line,
            indent: std::iter::repeat_n(ch, col).collect(),
        }
    }

    /// The span reported for a line of the comment: column 0 of that line.
    pub fn line_span(&self, finder: &Finder<'_>, rel: i64) -> Span {
        let l = (i64::from(self.line) + rel).max(0) as u32;
        let at = finder
            .lines
            .line_start(l)
            .unwrap_or(self.comment.start)
            .min(finder.src.len() as u32);
        Span::new(at, at)
    }

    /// Where the plugin's `report` puts a `jsdocLoc` of `{ line: rel, column }`: that line of
    /// the comment, at column 0 or, given `column`, the comment's own column plus it (which
    /// may lie past the end of the line). The span is the line's start.
    pub fn report_at(
        &self,
        finder: &Finder<'_>,
        rel: i64,
        column: Option<u32>,
    ) -> (Span, crate::diagnostic::Loc) {
        let line = (i64::from(self.line) + rel).max(0) as u32 + 1;
        let col = match column {
            Some(c) => self.indent.chars().count() as u32 + c,
            None => 0,
        };
        (
            self.line_span(finder, rel),
            crate::diagnostic::Loc::at(line, col + 1),
        )
    }

    /// Replaces the whole comment with the current block.
    pub fn fix(&self) -> crate::diagnostic::Fix {
        crate::diagnostic::Fix::replace(self.comment, self.block.stringify())
    }

    /// Resolves the preferred name of `name`, reporting a blocked tag the block uses.
    pub fn preferred(
        &self,
        cx: &mut Ctx<'_, '_>,
        finder: &Finder<'_>,
        name: &str,
    ) -> Option<String> {
        match preferred_tag(finder.settings, name) {
            Preferred::Name(n) => Some(n),
            Preferred::Blocked(msg) => {
                if self.block.has_tag(name) {
                    let msg = msg.unwrap_or_else(|| format!("Unexpected tag `@{name}`"));
                    let span = match self.block.tags_named(name).next() {
                        Some((i, _)) => self.line_span(finder, self.block.tag_line(i)),
                        None => self.comment,
                    };
                    cx.report(span, msg);
                }
                None
            }
        }
    }
}

/// Runs `f` on every block the contexts select.
pub fn for_each_doc(
    cx: &mut Ctx<'_, '_>,
    settings: &Settings,
    contexts: &Contexts,
    mut f: impl FnMut(&mut Ctx<'_, '_>, &Finder<'_>, &mut Doc),
) {
    if !settings_load(cx) {
        return;
    }
    let file = cx.file;
    let finder = Finder {
        ast: file.ast,
        src: file.src,
        parents: file.parents,
        lines: file.lines,
        settings,
    };
    for item in collect(&finder, contexts) {
        let mut doc = Doc::new(&finder, &item);
        if skipped(settings, &doc.block) {
            continue;
        }
        f(cx, &finder, &mut doc);
    }
}

// ---------------------------------------------------------------------------------------------
// Return values (`hasReturnValue` and the Promise executor check).

fn is_new_promise(ast: &Ast, id: NodeId) -> bool {
    matches!(*ast.node(id), Node::New { callee, .. } if ident_name(ast, skip_parens(ast, callee)) == Some("Promise"))
}

fn is_void_promise(ast: &Ast, id: NodeId) -> bool {
    let Node::New {
        type_args: Some(ta),
        ..
    } = *ast.node(id)
    else {
        return false;
    };
    let Node::TypeArgs { args } = *ast.node(ta) else {
        return false;
    };
    ast.list(args).first().is_some_and(|&a| {
        matches!(
            ast.node(a),
            Node::KeywordType {
                kind: KeywordTypeKind::Void
            }
        )
    })
}

fn is_void_like_type(ast: &Ast, ty: NodeId) -> bool {
    matches!(
        ast.node(ty),
        Node::KeywordType {
            kind: KeywordTypeKind::Void | KeywordTypeKind::Never | KeywordTypeKind::Undefined
        }
    )
}

/// `hasValueOrExecutorHasNonEmptyResolveValue`: the function returns a value, or returns a
/// `new Promise` whose executor resolves with one (any Promise when `any_promise`).
pub fn has_return_value(ast: &Ast, id: NodeId, any_promise: bool) -> bool {
    returns_value(ast, id, Some(any_promise), 0)
}

/// `hasReturnValue` without a Promise filter: any `return` with a value counts.
pub fn has_plain_return_value(ast: &Ast, id: NodeId) -> bool {
    returns_value(ast, id, None, 0)
}

/// `promises`: `None` counts every value return; `Some(any_promise)` looks into returned
/// `new Promise`s.
fn returns_value(ast: &Ast, id: NodeId, promises: Option<bool>, depth: u32) -> bool {
    if depth > MAX_DEPTH {
        return false;
    }
    let rec = |n: NodeId| returns_value(ast, n, promises, depth + 1);
    match *ast.node(id) {
        Node::FunctionDecl {
            body: None,
            return_type,
            ..
        } => return_type.is_some_and(|t| !is_void_like_type(ast, t)),
        Node::FunctionExpr { body: None, .. } => false,
        Node::FunctionDecl { body: Some(b), .. } | Node::FunctionExpr { body: Some(b), .. } => {
            rec(b)
        }
        Node::Arrow { flags, body, .. } => {
            if flags.contains(ts_r_ast::FnFlags::EXPRESSION_BODY) {
                let b = skip_parens(ast, body);
                !(is_new_promise(ast, b) && is_void_promise(ast, b))
            } else {
                rec(body)
            }
        }
        Node::FunctionType { return_type, .. } => !is_void_like_type(ast, return_type),
        Node::MethodSignature { return_type, .. } => {
            return_type.is_some_and(|t| !is_void_like_type(ast, t))
        }
        Node::MethodDef { value, .. } => rec(value),
        Node::Block { stmts } => ast
            .list(stmts)
            .iter()
            .any(|&s| !matches!(ast.node(s), Node::FunctionDecl { .. }) && rec(s)),
        Node::For { body, .. }
        | Node::ForIn { body, .. }
        | Node::ForOf { body, .. }
        | Node::While { body, .. }
        | Node::DoWhile { body, .. }
        | Node::Labeled { body, .. }
        | Node::With { body, .. } => rec(body),
        Node::If { cons, alt, .. } => rec(cons) || alt.is_some_and(rec),
        Node::Return { arg: None } => false,
        Node::Return { arg: Some(a) } => {
            let a = skip_parens(ast, a);
            match promises {
                None => true,
                Some(any_promise) => {
                    !is_new_promise(ast, a) || promise_resolves(ast, a, any_promise, depth + 1)
                }
            }
        }
        Node::Switch { cases, .. } => ast.list(cases).iter().any(|&c| match *ast.node(c) {
            Node::SwitchCase { body, .. } => ast.list(body).iter().any(|&s| rec(s)),
            _ => false,
        }),
        Node::Try {
            block,
            handler,
            finalizer,
        } => {
            rec(block)
                || handler.is_some_and(|h| match *ast.node(h) {
                    Node::CatchClause { body, .. } => rec(body),
                    _ => false,
                })
                || finalizer.is_some_and(rec)
        }
        _ => false,
    }
}

fn promise_resolves(ast: &Ast, new_expr: NodeId, any_promise: bool, depth: u32) -> bool {
    if any_promise {
        return true;
    }
    if is_void_promise(ast, new_expr) {
        return false;
    }
    let Node::New { args, .. } = *ast.node(new_expr) else {
        return false;
    };
    let Some(&exec) = ast.list(args).first() else {
        return false;
    };
    let (params, body) = match *ast.node(exec) {
        Node::FunctionExpr {
            params,
            body: Some(b),
            ..
        } => (params, b),
        Node::Arrow { params, body, .. } => (params, body),
        _ => return false,
    };
    let Some(&p0) = ast.list(params).first() else {
        return false;
    };
    let resolver = simple_param_name(ast, p0);
    resolver_called(ast, body, resolver, depth + 1)
}

fn simple_param_name(ast: &Ast, p: NodeId) -> Option<&str> {
    match *ast.node(p) {
        Node::Param {
            rest: false,
            pattern,
            init: None,
            ..
        } => ident_name(ast, pattern),
        _ => None,
    }
}

/// `hasNonEmptyResolverCall`: some call `resolver(arg)` (or passing the resolver on).
fn resolver_called(ast: &Ast, id: NodeId, resolver: Option<&str>, depth: u32) -> bool {
    if depth > MAX_DEPTH {
        return false;
    }
    let rec = |n: NodeId| resolver_called(ast, n, resolver, depth + 1);
    let any = |l: ts_r_ast::NodeList| ast.list(l).iter().any(|&n| rec(n));
    match *ast.node(id) {
        Node::Paren { expr } => rec(expr),
        Node::ArrayLit { elements } | Node::ArrayPattern { elements } => any(elements),
        Node::FunctionDecl { params, body, .. } | Node::FunctionExpr { params, body, .. } => {
            let p0 = ast
                .list(params)
                .first()
                .and_then(|&p| simple_param_name(ast, p));
            if p0 == resolver {
                return false;
            }
            body.is_some_and(rec)
        }
        Node::Arrow { params, body, .. } => {
            let p0 = ast
                .list(params)
                .first()
                .and_then(|&p| simple_param_name(ast, p));
            if p0 == resolver {
                return false;
            }
            rec(body)
        }
        Node::Assign { left, right, .. } | Node::Binary { left, right, .. } => {
            rec(left) || rec(right)
        }
        Node::AssignPattern { right, .. } => rec(right),
        Node::Await { arg } | Node::Spread { arg } | Node::Unary { arg, .. } => rec(arg),
        Node::Yield { arg, .. } => arg.is_some_and(rec),
        Node::Block { stmts } => any(stmts),
        Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. } => any(members),
        Node::Call { callee, args, .. } => {
            let callee_name = ident_name(ast, skip_parens(ast, callee));
            (callee_name == resolver && !ast.list(args).is_empty())
                || ast
                    .list(args)
                    .iter()
                    .any(|&a| resolver.is_some() && ident_name(ast, a) == resolver)
                || any(args)
        }
        Node::ExprStmt { expr } | Node::Decorator { expr } => rec(expr),
        Node::MethodDef {
            decorators,
            key,
            value,
            ..
        } => {
            any(decorators)
                || matches!(ast.node(key), Node::ComputedKey { .. }) && rec(key)
                || rec(value)
        }
        Node::ComputedKey { expr } => rec(expr),
        Node::Property { key, value, .. } => {
            matches!(ast.node(key), Node::ComputedKey { .. }) && rec(key) || rec(value)
        }
        Node::PropertyDef { key, value, .. } => {
            matches!(ast.node(key), Node::ComputedKey { .. }) && rec(key) || value.is_some_and(rec)
        }
        Node::BindingProperty { key, value, init } => {
            matches!(ast.node(key), Node::ComputedKey { .. }) && rec(key)
                || value.is_some_and(rec)
                || init.is_some_and(rec)
        }
        Node::Conditional { test, cons, alt } => rec(test) || rec(cons) || rec(alt),
        Node::If { test, cons, alt } => rec(test) || rec(cons) || alt.is_some_and(rec),
        Node::For { body, .. }
        | Node::ForIn { body, .. }
        | Node::ForOf { body, .. }
        | Node::While { body, .. }
        | Node::DoWhile { body, .. }
        | Node::Labeled { body, .. }
        | Node::With { body, .. } => rec(body),
        Node::ImportCall { arg, .. } => rec(arg),
        Node::Member { obj, prop, .. } => rec(obj) || rec(prop),
        Node::Index { obj, index, .. } => rec(obj) || rec(index),
        Node::ObjectLit { props } | Node::ObjectPattern { props } => any(props),
        Node::Return { arg } => arg.is_some_and(rec),
        Node::Seq { exprs } => any(exprs),
        Node::TemplateLit { parts } => any(parts),
        Node::Switch { cases, .. } => ast.list(cases).iter().any(|&c| match *ast.node(c) {
            Node::SwitchCase { body, .. } => any(body),
            _ => false,
        }),
        Node::TaggedTemplate { quasi, .. } => rec(quasi),
        Node::Try {
            block,
            handler,
            finalizer,
        } => {
            rec(block)
                || handler.is_some_and(|h| match *ast.node(h) {
                    Node::CatchClause { body, .. } => rec(body),
                    _ => false,
                })
                || finalizer.is_some_and(rec)
        }
        Node::VarDecl { decls, .. } => any(decls),
        Node::VarDeclarator { name, init, .. } => rec(name) || init.is_some_and(rec),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_round_trips() {
        let text = "/**\n   * Foo.\n   * @param {string} foo The foo.\n   * @param [bar=1]\n   * @returns {number}\n   */";
        let b = Block::parse(text);
        assert_eq!(b.stringify(), text);
        assert_eq!(b.tags.len(), 3);
        assert_eq!(b.tags[0].tag, "param");
        assert_eq!(b.tags[0].name, "foo");
        assert_eq!(b.tags[0].typ, "string");
        assert_eq!(b.tags[0].description, "The foo.");
        assert_eq!(b.tags[1].name, "bar");
        assert_eq!(b.tags[1].description, "");
        assert_eq!(b.tags[2].tag, "returns");
        assert_eq!(b.tags[2].name, "");
    }

    #[test]
    fn single_line_block() {
        let b = Block::parse("/** @param foo */");
        assert_eq!(b.tags.len(), 1);
        assert_eq!(b.tags[0].name, "foo");
        assert_eq!(b.stringify(), "/** @param foo */");
    }

    #[test]
    fn unpaired_curlies_stop_parsing() {
        let b = Block::parse("/**\n * @param {string foo Desc\n */");
        assert_eq!(b.tags[0].name, "");
        assert_eq!(b.tags[0].description, "");
    }

    #[test]
    fn critical_problems_are_recorded() {
        let problem = |text: &str| Block::parse(text).tags[0].problems.clone();
        assert_eq!(problem("/** @param {string foo */"), ["unpaired curlies"]);
        assert_eq!(problem("/** @param [foo */"), ["unpaired brackets"]);
        assert_eq!(problem("/** @param [] */"), ["empty name"]);
        assert_eq!(problem("/** @param [foo=] */"), ["empty default value"]);
        assert_eq!(
            problem("/** @param [foo=a=b] */"),
            ["invalid default value syntax"]
        );
        assert_eq!(problem("/** @param =x */"), ["empty name"]);
        assert!(problem("/** @param [foo=1] */").is_empty());
    }

    #[test]
    fn crlf_lines() {
        let text = "/**\r\n * @param a desc\r\n */";
        let b = Block::parse(text);
        assert_eq!(b.stringify(), text);
        assert_eq!(b.tags[0].description, "desc");
    }

    #[test]
    fn jsdoc_text() {
        assert!(is_jsdoc_text("/** x */"));
        assert!(is_jsdoc_text("/**\n */"));
        assert!(!is_jsdoc_text("/**x */"));
        assert!(!is_jsdoc_text("/***/"));
        assert!(!is_jsdoc_text("/**/"));
    }

    #[test]
    fn odd_blocks_do_not_panic() {
        let cases = [
            "/**",
            "/** @",
            "/** @param {",
            "/** @param [",
            "/** @param [=] x */",
            "/** @param a= */",
            "/** @param \"q */",
            "/**\n * @template\n */",
            "/**\n * @template [T=é] desc\r\n */",
            "/** @param {é} é é */",
            "/**\n * ```\n * @param x\n * ```\n */",
            "/**\n * @param {{\n *   a: string\n * }} é desc\n */",
            "/** \u{feff}@returns\u{a0}é */",
        ];
        for text in cases {
            let b = Block::parse(text);
            if text.ends_with("*/") {
                assert_eq!(b.stringify(), text, "{text:?}");
            }
        }
    }
}
