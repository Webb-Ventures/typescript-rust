//! The ESTree-like view of one parsed JSDoc block that `comment` selectors in jsdoc
//! `contexts` match against: a `JsdocBlock` root with `JsdocDescriptionLine`, `JsdocTag`,
//! `JsdocTypeLine` and `JsdocInlineTag` nodes.
//!
//! The shape follows `@es-joy/jsdoccomment` (`src/commentParserToESTree.js`, compact
//! spacing; `src/parseInlineTags.js`). Types are not parsed: `parsedType` is always `null`,
//! so selectors reaching into type nodes never match.

use std::borrow::Cow;

use crate::esquery::{Tree, Val};
use crate::jsdoc::Block;

#[derive(Debug, Clone)]
enum A {
    Str(String),
    Num(f64),
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
            n.set("parsedType", A::Null);
            tree.nodes.push(n);
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
            n.kids = tl.iter().chain(&dl).chain(&il).copied().collect();
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
        let Some((_, v)) = node.attrs.iter().find(|(k, _)| *k == head) else {
            return Val::Undef;
        };
        match (v, rest) {
            (A::Str(s), []) => Val::Str(Cow::Borrowed(s)),
            (A::Str(s), ["length"]) => Val::Num(s.encode_utf16().count() as f64),
            (A::Num(x), []) => Val::Num(*x),
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
}
