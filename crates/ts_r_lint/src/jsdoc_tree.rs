//! The ESTree-like view of one parsed JSDoc block that `comment` selectors in jsdoc
//! `contexts` match against: a `JsdocBlock` root with `JsdocDescriptionLine`, `JsdocTag`,
//! `JsdocTypeLine` and `JsdocInlineTag` nodes.
//!
//! The shape follows `@es-joy/jsdoccomment` (`src/commentParserToESTree.js`, compact
//! spacing; `src/parseInlineTags.js`). Tag types are parsed only for a small subset of
//! `jsdoc-type-pratt-parser`'s typescript mode ([`parse_type`]: names, numbers, strings,
//! `null`, `undefined`, `*`, `?` and unions of those); any other type gets `parsedType: null`,
//! as a type that fails to parse does.

use std::borrow::Cow;

use crate::esquery::{Tree, Val};
use crate::jsdoc::Block;

#[derive(Debug, Clone)]
enum A {
    Str(String),
    Num(f64),
    Bool(bool),
    Null,
}

#[derive(Debug, Default)]
struct JNode {
    ty: &'static str,
    parent: Option<u32>,
    key: &'static str,
    attrs: Vec<(&'static str, A)>,
    /// Array fields in traversal order.
    lists: Vec<(&'static str, Vec<u32>)>,
    /// Single-node fields (`parsedType`).
    fields: Vec<(&'static str, u32)>,
    kids: Vec<u32>,
}

impl JNode {
    fn new(ty: &'static str) -> JNode {
        JNode {
            ty,
            ..JNode::default()
        }
    }
    fn set(&mut self, k: &'static str, v: A) {
        match self.attrs.iter_mut().find(|(n, _)| *n == k) {
            Some(slot) => slot.1 = v,
            None => self.attrs.push((k, v)),
        }
    }
    fn s(&mut self, k: &'static str, v: &str) {
        self.set(k, A::Str(v.to_owned()));
    }
    #[cfg(test)]
    fn get_str(&self, k: &str) -> &str {
        match self.attrs.iter().find(|(n, _)| *n == k) {
            Some((_, A::Str(s))) => s,
            _ => "",
        }
    }
}

/// One line's description, before it becomes a node.
#[derive(Debug, Clone)]
struct DescLine {
    delimiter: String,
    description: String,
    post_delimiter: String,
    initial: String,
}

#[derive(Debug, Default)]
struct TagDraft {
    attrs: Vec<(&'static str, String)>,
    description: String,
    raw_type: String,
    desc_lines: Vec<DescLine>,
    type_lines: Vec<DescLine>,
    inline_source: String,
}

impl TagDraft {
    fn clean_up(&mut self) {
        let raw = self.raw_type.strip_prefix('{').unwrap_or(&self.raw_type);
        self.raw_type = raw.strip_suffix('}').unwrap_or(raw).to_owned();
        if let Some(first) = self.type_lines.first_mut() {
            first.description = first
                .description
                .strip_prefix('{')
                .unwrap_or(&first.description)
                .to_owned();
        }
        if let Some(last) = self.type_lines.last_mut() {
            last.description = last
                .description
                .strip_suffix('}')
                .unwrap_or(&last.description)
                .to_owned();
        }
        if self.desc_lines.len() == 1 && self.desc_lines[0].description.is_empty() {
            self.desc_lines.clear();
        }
    }
}

/// A JSDoc block as an esquery [`Tree`]; node 0 is the `JsdocBlock`.
#[derive(Debug)]
pub struct JsdocTree {
    nodes: Vec<JNode>,
}

struct Inline<'s> {
    tag: &'s str,
    namepath: &'s str,
    text: &'s str,
    format: &'static str,
}

fn is_space(c: char) -> bool {
    c.is_whitespace()
}

/// The tag names of a description's inline tags, in [`inline_tags`] order.
pub fn inline_tag_names(desc: &str) -> Vec<&str> {
    inline_tags(desc).into_iter().map(|t| t.tag).collect()
}

/// The `{@tag namepath text}` and `[text]{@tag namepath}` inline tags of a description, the
/// prefixed ones first (`parseInlineTags`).
fn inline_tags(desc: &str) -> Vec<Inline<'_>> {
    let mut prefixed = Vec::new();
    let mut suffixed = Vec::new();
    let b = desc.as_bytes();
    let mut i = 0;
    while let Some(off) = desc[i..].find("{@") {
        let at = i + off;
        i = at + 2;
        // Tag name: up to whitespace or `}`.
        let rest = &desc[at + 2..];
        let tag_len = rest
            .find(|c: char| c == '}' || is_space(c))
            .unwrap_or(rest.len());
        if tag_len == 0 {
            continue;
        }
        let tag = &rest[..tag_len];
        let mut p = at + 2 + tag_len;
        if desc[p..].starts_with(|c: char| is_space(c)) {
            p += desc[p..].chars().next().map_or(1, char::len_utf8);
        }
        let np_len = desc[p..]
            .find(|c: char| c == '}' || c == '|' || is_space(c))
            .unwrap_or(desc.len() - p);
        let namepath = &desc[p..p + np_len];
        p += np_len;
        let prefixed_text = (at > 0 && b[at - 1] == b']')
            .then(|| desc[..at - 1].rfind('[').map(|o| &desc[o + 1..at - 1]))
            .flatten();
        if let Some(text) = prefixed_text {
            if desc[p..].starts_with('}') && !text.is_empty() {
                prefixed.push(Inline {
                    tag,
                    namepath,
                    text,
                    format: "prefix",
                });
            }
            continue;
        }
        let ws = desc[p..].len() - desc[p..].trim_start_matches(is_space).len();
        let after_ws = p + ws;
        let mut separator = None;
        let mut q = after_ws;
        if desc[q..].starts_with('|') {
            separator = Some('|');
            q += 1;
        } else if ws > 0 {
            separator = Some(' ');
        }
        q += desc[q..].len() - desc[q..].trim_start_matches(is_space).len();
        // Text: up to an unescaped `}`.
        let mut end = None;
        let mut j = q;
        while j < b.len() {
            match b[j] {
                b'\\' => j += 2,
                b'}' => {
                    end = Some(j);
                    break;
                }
                _ => j += 1,
            }
        }
        let Some(end) = end else {
            break;
        };
        let text = &desc[q..end.min(desc.len())];
        let format = if text.is_empty() {
            "plain"
        } else if separator == Some('|') {
            "pipe"
        } else {
            "space"
        };
        suffixed.push(Inline {
            tag,
            namepath,
            text,
            format,
        });
        i = end + 1;
    }
    prefixed.extend(suffixed);
    prefixed
}

impl JsdocTree {
    pub fn build(block: &Block) -> JsdocTree {
        let mut tree = JsdocTree { nodes: Vec::new() };
        let src: Vec<_> = block
            .source
            .iter()
            .map(|&i| &block.lines[i].tokens)
            .collect();
        let mut root = JNode::new("JsdocBlock");
        let first = src.first().copied().cloned().unwrap_or_default();
        let end_line = src.len().saturating_sub(1);
        root.s("delimiter", &first.delimiter);
        root.s("delimiterLineBreak", "\n");
        root.s("initial", &first.start);
        root.s("terminal", &first.end);
        root.s("preterminalLineBreak", "\n");
        root.set("hasPreterminalDescription", A::Num(0.0));
        root.set("endLine", A::Num(end_line as f64));
        root.s("postDelimiter", &first.post_delimiter);
        root.s("lineEnd", &first.line_end);

        let mut description = String::new();
        let mut desc_lines: Vec<DescLine> = Vec::new();
        let mut tags: Vec<TagDraft> = Vec::new();
        let mut last_desc_line: Option<usize> = None;
        let mut desc_start: Option<usize> = None;
        let mut desc_end: Option<usize> = None;
        let mut tag_desc_seen = false;
        let mut desc_open = true;

        let push_desc = |holder: &mut String, d: &str| {
            if !holder.is_empty() {
                holder.push('\n');
            }
            holder.push_str(d);
        };

        for (idx, t) in src.iter().enumerate() {
            let tag = t.tag.as_str();
            let desc = t.description.as_str();
            let end = t.end.as_str();
            let raw_type = t.typ.as_str();
            if tag.is_empty() && !desc.is_empty() && desc_open {
                desc_start.get_or_insert(idx);
                desc_end = Some(idx);
            }
            if !tag.is_empty() || !end.is_empty() {
                desc_open = false;
                last_desc_line.get_or_insert(idx);
                if let Some(last) = tags.last_mut() {
                    last.clean_up();
                }
                if !end.is_empty() && tag.is_empty() {
                    root.s("terminal", end);
                    let delimiter_line = desc_lines.is_empty() && t.delimiter == "/**";
                    if delimiter_line {
                        root.s("delimiterLineBreak", "");
                    }
                    if !desc.is_empty() {
                        if end == "*/" {
                            root.s("preterminalLineBreak", "");
                        }
                        let line = DescLine {
                            delimiter: if delimiter_line {
                                String::new()
                            } else {
                                t.delimiter.clone()
                            },
                            description: desc.to_owned(),
                            post_delimiter: if delimiter_line {
                                String::new()
                            } else {
                                t.post_delimiter.clone()
                            },
                            initial: t.start.clone(),
                        };
                        match tags.last_mut() {
                            Some(last) => {
                                root.set("hasPreterminalTagDescription", A::Num(1.0));
                                push_desc(&mut last.description, desc);
                                last.desc_lines.push(line);
                            }
                            None => {
                                root.set("hasPreterminalDescription", A::Num(1.0));
                                push_desc(&mut description, desc);
                                desc_lines.push(line);
                            }
                        }
                    }
                    continue;
                }
                // A new tag. The name may sit on a later line of the same tag.
                let (mut name, mut post_name, mut post_type) =
                    (t.name.clone(), t.post_name.clone(), t.post_type.clone());
                if name.is_empty() {
                    for later in &src[idx + 1..] {
                        if !later.tag.is_empty() {
                            break;
                        }
                        if !later.name.is_empty() {
                            post_type = later.post_type.clone();
                            name = later.name.clone();
                            post_name = later.post_name.clone();
                            break;
                        }
                    }
                }
                let first_line = last_desc_line.is_some_and(|l| l != 0);
                let mut d = TagDraft {
                    description: desc.to_owned(),
                    ..TagDraft::default()
                };
                d.attrs = vec![
                    ("tag", tag.strip_prefix('@').unwrap_or(tag).to_owned()),
                    ("postTag", t.post_tag.clone()),
                    ("name", name),
                    ("postName", post_name),
                    ("type", t.typ.clone()),
                    ("postType", post_type),
                    ("lineEnd", t.line_end.clone()),
                    (
                        "initial",
                        if end_line > 0 {
                            t.start.clone()
                        } else {
                            String::new()
                        },
                    ),
                    (
                        "postDelimiter",
                        if first_line {
                            t.post_delimiter.clone()
                        } else {
                            String::new()
                        },
                    ),
                    (
                        "delimiter",
                        if first_line {
                            t.delimiter.clone()
                        } else {
                            String::new()
                        },
                    ),
                ];
                if let Some(bt) = block.tags.get(tags.len()) {
                    d.inline_source = bt.description.clone();
                }
                tags.push(d);
                tag_desc_seen = false;
            }

            if !raw_type.is_empty()
                && let Some(last) = tags.last_mut()
            {
                let line = if last.type_lines.is_empty() {
                    DescLine {
                        delimiter: String::new(),
                        description: raw_type.to_owned(),
                        post_delimiter: String::new(),
                        initial: String::new(),
                    }
                } else {
                    DescLine {
                        delimiter: t.delimiter.clone(),
                        description: raw_type.to_owned(),
                        post_delimiter: t.post_delimiter.clone(),
                        initial: t.start.clone(),
                    }
                };
                last.type_lines.push(line);
                push_desc(&mut last.raw_type, raw_type);
            }

            if !desc.is_empty() || !tags.is_empty() {
                let holder_lines = match tags.last() {
                    Some(l) => l.desc_lines.len(),
                    None => desc_lines.len(),
                };
                let delimiter_line = holder_lines == 0 && t.delimiter == "/**";
                if delimiter_line {
                    root.s("delimiterLineBreak", "");
                }
                tag_desc_seen |=
                    !tags.is_empty() && (raw_type.is_empty() || raw_type.ends_with('}'));
                match tags.last_mut() {
                    Some(last) => {
                        if tag_desc_seen {
                            let first = last.desc_lines.is_empty();
                            if !desc.is_empty() || first {
                                let pick =
                                    |s: &String| if first { String::new() } else { s.clone() };
                                last.desc_lines.push(DescLine {
                                    delimiter: pick(&t.delimiter),
                                    description: desc.to_owned(),
                                    post_delimiter: pick(&t.post_delimiter),
                                    initial: pick(&t.start),
                                });
                            }
                        }
                        if tag.is_empty()
                            && tag_desc_seen
                            && (last.description.is_empty() || !desc.is_empty())
                        {
                            push_desc(&mut last.description, desc);
                        }
                    }
                    None => {
                        let pick = |s: &String| {
                            if delimiter_line {
                                String::new()
                            } else {
                                s.clone()
                            }
                        };
                        desc_lines.push(DescLine {
                            delimiter: pick(&t.delimiter),
                            description: desc.to_owned(),
                            post_delimiter: pick(&t.post_delimiter),
                            initial: pick(&t.start),
                        });
                        if tag.is_empty() {
                            push_desc(&mut description, desc);
                        }
                    }
                }
            }

            if !end.is_empty() && !tag.is_empty() {
                root.s("terminal", end);
                root.set("hasPreterminalTagDescription", A::Num(1.0));
                if end == "*/" {
                    root.s("preterminalLineBreak", "");
                }
                if let Some(last) = tags.last_mut() {
                    last.clean_up();
                }
            }
        }

        root.s("description", &description);
        if let Some(l) = desc_start {
            root.set("descriptionStartLine", A::Num(l as f64));
        }
        if let Some(l) = desc_end {
            root.set("descriptionEndLine", A::Num(l as f64));
        }
        if let Some(l) = last_desc_line {
            root.set("lastDescriptionLine", A::Num(l as f64));
        }

        // The block's own inline tags come from its compact (space-joined) description.
        let block_desc: Vec<&str> = src
            .iter()
            .take_while(|t| t.tag.is_empty())
            .map(|t| t.description.trim())
            .filter(|d| !d.is_empty())
            .collect();
        let block_desc = block_desc.join(" ");

        tree.nodes.push(root);
        let lines: Vec<u32> = desc_lines
            .iter()
            .map(|l| tree.push_line(0, "descriptionLines", "JsdocDescriptionLine", l))
            .collect();
        let mut tag_ids = Vec::new();
        for d in &tags {
            let id = tree.nodes.len() as u32;
            let mut n = JNode::new("JsdocTag");
            n.parent = Some(0);
            n.key = "tags";
            for (k, v) in &d.attrs {
                n.s(k, v);
            }
            n.s("description", &d.description);
            n.s("rawType", &d.raw_type);
            let ty = parse_type(&d.raw_type);
            if ty.is_none() {
                n.set("parsedType", A::Null);
            }
            tree.nodes.push(n);
            let parsed = ty.map(|t| tree.push_type(id, "parsedType", &t));
            let tl: Vec<u32> = d
                .type_lines
                .iter()
                .map(|l| tree.push_line(id, "typeLines", "JsdocTypeLine", l))
                .collect();
            let dl: Vec<u32> = d
                .desc_lines
                .iter()
                .map(|l| tree.push_line(id, "descriptionLines", "JsdocDescriptionLine", l))
                .collect();
            let il = tree.push_inline(id, &d.inline_source);
            let n = &mut tree.nodes[id as usize];
            n.kids = parsed
                .iter()
                .chain(&tl)
                .chain(&dl)
                .chain(&il)
                .copied()
                .collect();
            n.fields = parsed.map(|p| ("parsedType", p)).into_iter().collect();
            n.lists = vec![
                ("typeLines", tl),
                ("descriptionLines", dl),
                ("inlineTags", il),
            ];
            tag_ids.push(id);
        }
        let il = tree.push_inline(0, &block_desc);
        let r = &mut tree.nodes[0];
        r.kids = lines.iter().chain(&tag_ids).chain(&il).copied().collect();
        r.lists = vec![
            ("descriptionLines", lines),
            ("tags", tag_ids),
            ("inlineTags", il),
        ];
        tree
    }

    fn push_line(&mut self, parent: u32, key: &'static str, ty: &'static str, l: &DescLine) -> u32 {
        let mut n = JNode::new(ty);
        n.parent = Some(parent);
        n.key = key;
        n.s("delimiter", &l.delimiter);
        n.s(
            if ty == "JsdocTypeLine" {
                "rawType"
            } else {
                "description"
            },
            &l.description,
        );
        n.s("postDelimiter", &l.post_delimiter);
        n.s("initial", &l.initial);
        self.nodes.push(n);
        self.nodes.len() as u32 - 1
    }

    /// Adds a parsed type node (and its children) under `parent`'s field `key`, in the
    /// type parser's visitor-key order.
    fn push_type(&mut self, parent: u32, key: &'static str, t: &TypeNode) -> u32 {
        let id = self.leaf(parent, key, t.ty());
        let n = &mut self.nodes[id as usize];
        match t {
            TypeNode::Name(v) => n.s("value", v),
            TypeNode::Number(v) => n.set("value", A::Num(*v)),
            TypeNode::Str(v, quote) => {
                n.s("value", v);
                n.s("meta.quote", quote);
            }
            TypeNode::NamePath(..) => n.s("pathType", "property"),
            TypeNode::Generic(_, _, brackets, dot) => {
                n.s("meta.brackets", brackets);
                n.set("meta.dot", A::Bool(*dot));
            }
            TypeNode::Object(_, sep) => n.s("meta.separator", sep),
            TypeNode::Null
            | TypeNode::Undefined
            | TypeNode::Any
            | TypeNode::Unknown
            | TypeNode::Union(_)
            | TypeNode::Paren(_) => {}
        }
        let mut fields = Vec::new();
        let mut lists: Vec<(&'static str, Vec<u32>)> = Vec::new();
        match t {
            TypeNode::Union(elems) => {
                let ids = elems
                    .iter()
                    .map(|e| self.push_type(id, "elements", e))
                    .collect();
                lists.push(("elements", ids));
            }
            TypeNode::NamePath(left, prop, quote) => {
                fields.push(("left", self.push_type(id, "left", left)));
                let r = self.leaf(id, "right", "JsdocTypeProperty");
                let rn = &mut self.nodes[r as usize];
                rn.s("value", prop);
                if let Some(q) = quote {
                    rn.s("meta.quote", q);
                }
                fields.push(("right", r));
            }
            TypeNode::Generic(left, elems, ..) => {
                fields.push(("left", self.push_type(id, "left", left)));
                let ids = elems
                    .iter()
                    .map(|e| self.push_type(id, "elements", e))
                    .collect();
                lists.push(("elements", ids));
            }
            TypeNode::Object(list, _) => {
                let ids = list.iter().map(|f| self.push_field(id, f)).collect();
                lists.push(("elements", ids));
            }
            TypeNode::Paren(inner) => {
                fields.push(("element", self.push_type(id, "element", inner)));
            }
            _ => {}
        }
        let n = &mut self.nodes[id as usize];
        n.kids = fields.iter().map(|&(_, c)| c).collect();
        for (_, l) in &lists {
            n.kids.extend_from_slice(l);
        }
        n.fields = fields;
        n.lists = lists;
        id
    }

    /// A `JsdocTypeObjectField` (its `key` is a plain string, not a node).
    fn push_field(&mut self, parent: u32, f: &Field) -> u32 {
        let id = self.leaf(parent, "elements", "JsdocTypeObjectField");
        let n = &mut self.nodes[id as usize];
        n.s("key", &f.key);
        n.set("optional", A::Bool(f.optional));
        n.set("readonly", A::Bool(f.readonly));
        if let Some(q) = f.quote {
            n.s("meta.quote", q);
        }
        if let Some(r) = &f.right {
            let c = self.push_type(id, "right", r);
            let n = &mut self.nodes[id as usize];
            n.kids = vec![c];
            n.fields = vec![("right", c)];
        }
        id
    }

    /// Pushes a childless node of type `ty` under `parent`'s field `key`.
    fn leaf(&mut self, parent: u32, key: &'static str, ty: &'static str) -> u32 {
        let mut n = JNode::new(ty);
        n.parent = Some(parent);
        n.key = key;
        self.nodes.push(n);
        self.nodes.len() as u32 - 1
    }

    fn push_inline(&mut self, parent: u32, desc: &str) -> Vec<u32> {
        let mut out = Vec::new();
        for t in inline_tags(desc) {
            let mut n = JNode::new("JsdocInlineTag");
            n.parent = Some(parent);
            n.key = "inlineTags";
            n.s("text", t.text);
            n.s("tag", t.tag);
            n.s("format", t.format);
            n.s("namepathOrURL", t.namepath);
            out.push(self.nodes.len() as u32);
            self.nodes.push(n);
        }
        out
    }

    fn list_of(&self, n: u32) -> Option<&[u32]> {
        let node = &self.nodes[n as usize];
        let p = &self.nodes[node.parent? as usize];
        p.lists
            .iter()
            .find(|(k, _)| *k == node.key)
            .map(|(_, l)| l.as_slice())
    }
}

/// A parsed tag type (the subset [`parse_type`] understands).
#[derive(Debug, Clone, PartialEq)]
enum TypeNode {
    Name(String),
    Number(f64),
    /// Value and quote kind (`single` or `double`).
    Str(String, &'static str),
    Null,
    Undefined,
    Any,
    Unknown,
    Union(Vec<TypeNode>),
    /// `left.prop` (a property path); the quote kind when the property is quoted.
    NamePath(Box<TypeNode>, String, Option<&'static str>),
    /// `left<elements>`, `left.<elements>` or `elements[]` (left `Array`): brackets and dot.
    Generic(Box<TypeNode>, Vec<TypeNode>, &'static str, bool),
    /// `{fields}` and the separator (`comma` unless they are split by `;`).
    Object(Vec<Field>, &'static str),
    /// `(element)`.
    Paren(Box<TypeNode>),
}

/// One `JsdocTypeObjectField`.
#[derive(Debug, Clone, PartialEq)]
struct Field {
    key: String,
    quote: Option<&'static str>,
    right: Option<TypeNode>,
    optional: bool,
    readonly: bool,
}

impl TypeNode {
    fn ty(&self) -> &'static str {
        match self {
            TypeNode::Name(_) => "JsdocTypeName",
            TypeNode::Number(_) => "JsdocTypeNumber",
            TypeNode::Str(..) => "JsdocTypeStringValue",
            TypeNode::Null => "JsdocTypeNull",
            TypeNode::Undefined => "JsdocTypeUndefined",
            TypeNode::Any => "JsdocTypeAny",
            TypeNode::Unknown => "JsdocTypeUnknown",
            TypeNode::Union(_) => "JsdocTypeUnion",
            TypeNode::NamePath(..) => "JsdocTypeNamePath",
            TypeNode::Generic(..) => "JsdocTypeGeneric",
            TypeNode::Object(..) => "JsdocTypeObject",
            TypeNode::Paren(_) => "JsdocTypeParenthesis",
        }
    }
}

/// Words the type grammar treats as keywords: a bare one is not a plain name.
const TYPE_KEYWORDS: &[&str] = &[
    "this", "typeof", "keyof", "import", "new", "function", "is", "asserts", "infer", "extends",
    "readonly", "in", "module", "event", "external", "unique", "const", "out",
];

/// Nesting limit for [`parse_type`]; deeper types get `parsedType: null`.
const MAX_TYPE_DEPTH: u32 = 64;

/// Parses `raw` (a tag type without its braces) into the subset of `jsdoc-type-pratt-parser`'s
/// typescript-mode shapes we model: names, numbers, quoted strings, `null`, `undefined`, `*`,
/// `?`, property paths, generics (`A<B>`, `A.<B>`, `B[]`), object types and unions. `None`
/// for anything else.
fn parse_type(raw: &str) -> Option<TypeNode> {
    let mut p = TypeParser { s: raw, pos: 0 };
    let t = p.union(0)?;
    p.ws();
    (p.pos == raw.len()).then_some(t)
}

struct TypeParser<'s> {
    s: &'s str,
    pos: usize,
}

impl<'s> TypeParser<'s> {
    fn rest(&self) -> &'s str {
        &self.s[self.pos..]
    }
    fn ws(&mut self) {
        let r = self.rest();
        self.pos += r.len() - r.trim_start().len();
    }
    fn eat(&mut self, tok: &str) -> bool {
        self.ws();
        if self.rest().starts_with(tok) {
            self.pos += tok.len();
            true
        } else {
            false
        }
    }
    fn peek(&mut self) -> Option<char> {
        self.ws();
        self.rest().chars().next()
    }

    fn union(&mut self, depth: u32) -> Option<TypeNode> {
        if depth > MAX_TYPE_DEPTH {
            return None;
        }
        let first = self.postfix(depth)?;
        if self.peek() != Some('|') {
            return Some(first);
        }
        let mut elems = vec![first];
        while self.eat("|") {
            elems.push(self.postfix(depth)?);
        }
        Some(TypeNode::Union(elems))
    }

    fn postfix(&mut self, depth: u32) -> Option<TypeNode> {
        let mut t = self.primary(depth)?;
        loop {
            if self.eat("[]") {
                let left = TypeNode::Name("Array".into());
                t = TypeNode::Generic(Box::new(left), vec![t], "square", false);
            } else if self.eat(".<") {
                let args = self.args(depth)?;
                t = TypeNode::Generic(Box::new(t), args, "angle", true);
            } else if self.eat("<") {
                let args = self.args(depth)?;
                t = TypeNode::Generic(Box::new(t), args, "angle", false);
            } else if self.eat(".") {
                if !matches!(
                    t,
                    TypeNode::Name(_) | TypeNode::NamePath(..) | TypeNode::Generic(..)
                ) {
                    return None;
                }
                self.ws();
                let (prop, quote) = match self.quoted() {
                    Some((v, q)) => (v, Some(q)),
                    None => (self.ident()?.to_owned(), None),
                };
                t = TypeNode::NamePath(Box::new(t), prop, quote);
            } else {
                return Some(t);
            }
        }
    }

    /// Generic arguments after `<`, through the closing `>`.
    fn args(&mut self, depth: u32) -> Option<Vec<TypeNode>> {
        let mut out = vec![self.union(depth + 1)?];
        while self.eat(",") {
            out.push(self.union(depth + 1)?);
        }
        self.eat(">").then_some(out)
    }

    fn ident(&mut self) -> Option<&'s str> {
        let r = self.rest();
        let first = r.chars().next()?;
        if !(first.is_ascii_alphabetic() || first == '_' || first == '$') {
            return None;
        }
        let len = r
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '$'))
            .unwrap_or(r.len());
        self.pos += len;
        Some(&r[..len])
    }

    /// A quoted string without escapes: its value and quote kind.
    fn quoted(&mut self) -> Option<(String, &'static str)> {
        let r = self.rest();
        let q = r.chars().next().filter(|c| matches!(c, '"' | '\''))?;
        let end = r[1..].find(q)? + 1;
        let inner = &r[1..end];
        if inner.contains('\\') {
            return None;
        }
        self.pos += end + 1;
        Some((inner.to_owned(), if q == '"' { "double" } else { "single" }))
    }

    fn primary(&mut self, depth: u32) -> Option<TypeNode> {
        let c = self.peek()?;
        match c {
            '*' => {
                self.pos += 1;
                Some(TypeNode::Any)
            }
            '?' => {
                self.pos += 1;
                Some(TypeNode::Unknown)
            }
            '"' | '\'' => self.quoted().map(|(v, q)| TypeNode::Str(v, q)),
            '{' => self.object(depth),
            '(' => {
                self.pos += 1;
                let inner = self.union(depth + 1)?;
                self.eat(")").then(|| TypeNode::Paren(Box::new(inner)))
            }
            '-' | '0'..='9' => self.number(),
            _ => {
                let id = self.ident()?;
                Some(match id {
                    "null" => TypeNode::Null,
                    "undefined" => TypeNode::Undefined,
                    _ if TYPE_KEYWORDS.contains(&id) => return None,
                    _ => TypeNode::Name(id.to_owned()),
                })
            }
        }
    }

    fn number(&mut self) -> Option<TypeNode> {
        let r = self.rest();
        let len = r
            .char_indices()
            .find(|&(i, c)| {
                !(c.is_ascii_digit()
                    || c == '.' && !r[i + 1..].starts_with('<')
                    || matches!(c, 'e' | 'E')
                    || matches!(c, '+' | '-') && (i == 0 || r[..i].ends_with(['e', 'E'])))
            })
            .map_or(r.len(), |(i, _)| i);
        let s = &r[..len];
        let digits = s.strip_prefix('-').unwrap_or(s);
        let (mant, exp) = match digits.find(['e', 'E']) {
            Some(i) => (&digits[..i], Some(&digits[i + 1..])),
            None => (digits, None),
        };
        let mut halves = mant.splitn(2, '.');
        let int = halves.next().unwrap_or("");
        let frac = halves.next();
        let all_digits = |p: &str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit());
        let exp_ok = exp.is_none_or(|e| all_digits(e.strip_prefix(['+', '-']).unwrap_or(e)));
        // A number running into a name (`0x10`, `1a`) is not a type.
        let next = r[len..].chars().next();
        if !(all_digits(int) && frac.is_none_or(all_digits) && exp_ok)
            || next.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
        {
            return None;
        }
        let v = s.parse::<f64>().ok()?;
        self.pos += len;
        Some(TypeNode::Number(v))
    }

    fn object(&mut self, depth: u32) -> Option<TypeNode> {
        if depth > MAX_TYPE_DEPTH {
            return None;
        }
        self.pos += 1; // `{`
        let mut fields = Vec::new();
        let mut sep: Option<&'static str> = None;
        loop {
            if self.eat("}") {
                return Some(TypeNode::Object(fields, sep.unwrap_or("comma")));
            }
            if !fields.is_empty() {
                let this = if self.eat(",") {
                    "comma"
                } else if self.eat(";") {
                    "semicolon"
                } else {
                    return None;
                };
                if sep.is_some_and(|s| s != this) {
                    return None;
                }
                sep = Some(this);
                if self.eat("}") {
                    return Some(TypeNode::Object(fields, this));
                }
            }
            fields.push(self.field(depth)?);
        }
    }

    fn field(&mut self, depth: u32) -> Option<Field> {
        self.ws();
        let save = self.pos;
        let mut readonly = false;
        if self.ident() == Some("readonly")
            && !matches!(self.peek(), Some(':' | '?' | ',' | ';' | '}'))
        {
            readonly = true;
        } else {
            self.pos = save;
        }
        self.ws();
        let (key, quote) = match self.quoted() {
            Some((v, q)) => (v, Some(q)),
            None => (self.ident()?.to_owned(), None),
        };
        let optional = self.eat("?");
        let right = if self.eat(":") {
            Some(self.union(depth + 1)?)
        } else {
            None
        };
        Some(Field {
            key,
            quote,
            right,
            optional,
            readonly,
        })
    }
}

impl Tree for JsdocTree {
    fn ty(&self, n: u32) -> &str {
        self.nodes[n as usize].ty
    }
    fn parent(&self, n: u32) -> Option<u32> {
        self.nodes[n as usize].parent
    }
    fn key(&self, n: u32) -> &str {
        self.nodes[n as usize].key
    }
    fn list_pos(&self, n: u32) -> Option<(u32, u32)> {
        let l = self.list_of(n)?;
        let i = l.iter().position(|&x| x == n)?;
        Some((i as u32, l.len() as u32))
    }
    fn list_item(&self, n: u32, index: u32) -> Option<u32> {
        self.list_of(n)?.get(index as usize).copied()
    }
    fn children(&self, n: u32) -> &[u32] {
        &self.nodes[n as usize].kids
    }
    fn attr(&self, n: u32, path: &[&str]) -> Val<'_> {
        let node = &self.nodes[n as usize];
        let Some((&head, rest)) = path.split_first() else {
            return Val::Node;
        };
        if head == "type" && rest.is_empty() {
            return Val::Str(Cow::Borrowed(node.ty));
        }
        if let Some((_, l)) = node.lists.iter().find(|(k, _)| *k == head) {
            return match rest {
                [] => Val::List(l.len() as u32),
                ["length"] => Val::Num(l.len() as f64),
                [i, more @ ..] => match i.parse::<usize>().ok().and_then(|i| l.get(i)) {
                    Some(&c) => self.attr(c, more),
                    None => Val::Undef,
                },
            };
        }
        if let Some(&(_, c)) = node.fields.iter().find(|(k, _)| *k == head) {
            return self.attr(c, rest);
        }
        let found = if rest.is_empty() || rest == ["length"] {
            node.attrs.iter().find(|(k, _)| *k == head)
        } else {
            // A nested plain object (`meta.quote`) is stored under its dotted path.
            let dotted = path.join(".");
            return match node.attrs.iter().find(|(k, _)| *k == dotted) {
                Some((_, A::Str(s))) => Val::Str(Cow::Borrowed(s)),
                Some((_, A::Num(x))) => Val::Num(*x),
                Some((_, A::Bool(b))) => Val::Bool(*b),
                Some((_, A::Null)) => Val::Null,
                None => Val::Undef,
            };
        };
        let Some((_, v)) = found else {
            return Val::Undef;
        };
        match (v, rest) {
            (A::Str(s), []) => Val::Str(Cow::Borrowed(s)),
            (A::Str(s), ["length"]) => Val::Num(s.encode_utf16().count() as f64),
            (A::Num(x), []) => Val::Num(*x),
            (A::Bool(b), []) => Val::Bool(*b),
            (A::Null, []) => Val::Null,
            _ => Val::Undef,
        }
    }
}

impl JsdocTree {
    /// The tag name of a `JsdocTag` node (for tests).
    #[cfg(test)]
    fn tag_name(&self, n: u32) -> &str {
        self.nodes[n as usize].get_str("tag")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::esquery;

    fn matches(comment: &str, sel: &str) -> bool {
        let tree = JsdocTree::build(&Block::parse(comment));
        esquery::parse(sel).unwrap().matches(&tree, 0, 0)
    }

    #[test]
    fn block_shape() {
        let c = "/**\n * Desc {@link Foo}.\n * @param {string} props The props.\n * @returns {void}\n */";
        let tree = JsdocTree::build(&Block::parse(c));
        assert_eq!(tree.ty(0), "JsdocBlock");
        let tags = &tree.nodes[0].lists[1].1;
        assert_eq!(tags.len(), 2);
        assert_eq!(tree.tag_name(tags[0]), "param");
        assert!(matches(c, "JsdocBlock:has(JsdocTag[name=props])"));
        assert!(!matches(
            c,
            "JsdocBlock:has(JsdocTag:not([name=props]):not([tag=returns]))"
        ));
        assert!(matches(c, "JsdocBlock:has(JsdocTag[rawType=string])"));
        assert!(matches(
            c,
            "JsdocBlock:has(JsdocInlineTag[tag=link][namepathOrURL=Foo])"
        ));
        assert!(matches(c, "JsdocBlock[endLine=4][descriptionStartLine=1]"));
        assert!(matches(c, "JsdocBlock:has(JsdocTag[tag=/returns/])"));
        assert!(matches(
            "/** One line */",
            "JsdocBlock[endLine=0][hasPreterminalDescription=1]"
        ));
        assert!(matches(
            "/** @type {Foo} */",
            "JsdocBlock[hasPreterminalTagDescription=1]:has(JsdocTag[tag=type])"
        ));
    }

    #[test]
    fn odd_input_does_not_panic() {
        for c in [
            "/***/",
            "/** {@",
            "/** {@link",
            "/** [x]{@a} {@b c|d} {@}",
            "/**\n * @\n */",
        ] {
            let _ = JsdocTree::build(&Block::parse(c));
        }
    }

    /// Shapes observed from `jsdoc-type-pratt-parser` (typescript mode).
    #[test]
    fn tag_types() {
        use TypeNode as T;
        let name = |s: &str| T::Name(s.into());
        assert_eq!(
            parse_type("Bar | Foo"),
            Some(T::Union(vec![name("Bar"), name("Foo")]))
        );
        assert_eq!(parse_type("-1e3"), Some(T::Number(-1000.0)));
        assert_eq!(parse_type("'x'"), Some(T::Str("x".into(), "single")));
        assert_eq!(
            parse_type("string[]"),
            Some(T::Generic(
                Box::new(name("Array")),
                vec![name("string")],
                "square",
                false
            ))
        );
        assert_eq!(
            parse_type("Array.<A>"),
            Some(T::Generic(
                Box::new(name("Array")),
                vec![name("A")],
                "angle",
                true
            ))
        );
        assert_eq!(
            parse_type("a.\"b\""),
            Some(T::NamePath(Box::new(name("a")), "b".into(), Some("double")))
        );
        let Some(T::Object(fields, sep)) = parse_type("{readonly a: x; b?}") else {
            panic!("object type");
        };
        assert_eq!(sep, "semicolon");
        assert!(fields[0].readonly && fields[0].right == Some(name("x")));
        assert!(fields[1].optional && fields[1].right.is_none());
        assert_eq!(parse_type("{}"), Some(T::Object(vec![], "comma")));
        for bad in [
            "",
            "A<>",
            "0x10",
            "foo-bar",
            "this",
            "{a, b; c}",
            "a.",
            "(A",
            "A |",
        ] {
            assert_eq!(parse_type(bad), None, "{bad}");
        }
        let deep = "A<".repeat(500) + &">".repeat(500);
        assert_eq!(parse_type(&deep), None);
    }

    #[test]
    fn type_selectors() {
        let c = "/** @type {React.FunctionComponent<{ children: React.ReactNode }>} */";
        assert!(matches(
            c,
            "JsdocBlock:has(JsdocTag[tag=\"type\"]:has([value=/FunctionComponent/]))"
        ));
        assert!(matches(
            c,
            "JsdocBlock:has(JsdocTag[parsedType.type=JsdocTypeGeneric][parsedType.meta.dot=false])"
        ));
        assert!(matches(
            c,
            "JsdocBlock:has(JsdocTypeObjectField[key=children] > JsdocTypeNamePath > JsdocTypeProperty)"
        ));
        let u = "/** @param {Bar|Foo} x */";
        assert!(matches(
            u,
            "JsdocBlock:has(JsdocTypeUnion > JsdocTypeName[value=\"Foo\"]:nth-child(2))"
        ));
        assert!(matches(
            "/** @type {a.b} */",
            "JsdocBlock:has(JsdocTag[parsedType.right.value=b])"
        ));
        assert!(matches(
            "/** @type {} */",
            "JsdocBlock:has(JsdocTag[parsedType=null])"
        ));
    }
}
