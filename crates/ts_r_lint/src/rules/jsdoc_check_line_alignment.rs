//! `jsdoc/check-line-alignment`: the parts of tag lines are aligned (`"always"`) or
//! separated by single spaces (`"never"`, the default), and wrapped tag lines are indented
//! by `wrapIndent`.
//!
//! `"always"` (multi-line blocks only) rebuilds the block: every line starts at the
//! comment's indent plus one space, description lines before the tags get one space after
//! the `*` (or none when empty), and the lines of the `tags` option (and the untagged lines
//! that follow them) pad the tag, type and name columns to the widest in the block; blocks
//! without any types fold the type column into the name column. The block is reported when
//! the result differs from the source. `"never"` reports each first tag line of the `tags`
//! whose spacers are neither empty nor their `customSpacings` width, or whose description's
//! leading hyphen is not followed by exactly `postHyphen` spaces; its fix also clears the
//! content after a spacer left empty (the plugin's behaviour, kept). Except with
//! `disableWrapIndent`, the first continuation line of a `tags` tag whose padding (after
//! one space) is not `wrapIndent` is reported, unless it is deeper than `wrapIndent` after
//! a markdown list item.

use super::prelude::*;
use crate::jsdoc::{Block, Doc, Finder, Tokens};
use crate::jsdoc_tags::{SMode, Structure};
use crate::jsdoc_util::{each_block, is_js_ws};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/check-line-alignment",
        "Reports invalid alignment of JSDoc block lines.",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<CheckLineAlignment>,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Style {
    Always,
    Never,
    Any,
}

#[derive(Debug, Default, Clone, Copy)]
struct Spacings {
    post_delimiter: Option<i64>,
    post_hyphen: Option<i64>,
    post_name: Option<i64>,
    post_tag: Option<i64>,
    post_type: Option<i64>,
}

#[derive(Debug)]
struct CheckLineAlignment {
    style: Style,
    spacings: Spacings,
    disable_wrap_indent: bool,
    preserve_main_post_delimiter: bool,
    tags: Vec<String>,
    wrap_indent: String,
}

impl FromOptions for CheckLineAlignment {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let style = match opts.first().and_then(Value::as_str) {
            Some("always") => Style::Always,
            Some("any") => Style::Any,
            _ => Style::Never,
        };
        let o = opts.get(1).and_then(Value::as_object);
        let get = |k: &str| o.and_then(|o| o.get(k));
        let cs = get("customSpacings").and_then(Value::as_object);
        let sp = |k: &str| cs.and_then(|c| c.get(k)).and_then(Value::as_i64);
        Ok(CheckLineAlignment {
            style,
            spacings: Spacings {
                post_delimiter: sp("postDelimiter"),
                post_hyphen: sp("postHyphen"),
                post_name: sp("postName"),
                post_tag: sp("postTag"),
                post_type: sp("postType"),
            },
            disable_wrap_indent: get("disableWrapIndent")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            preserve_main_post_delimiter: get("preserveMainDescriptionPostDelimiter")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            tags: match get("tags").and_then(Value::as_array) {
                Some(a) => a
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect(),
                None => [
                    "param", "arg", "argument", "property", "prop", "returns", "return", "template",
                ]
                .map(str::to_owned)
                .to_vec(),
            },
            wrap_indent: get("wrapIndent")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
        })
    }
}

/// A spacing option, where `0` (or none) means one (`|| 1`).
fn or_one(v: Option<i64>) -> i64 {
    match v {
        Some(n) if n != 0 => n,
        _ => 1,
    }
}

/// `"".padStart(n, " ")`.
fn spaces(n: i64) -> String {
    " ".repeat(n.clamp(0, 10_000) as usize)
}

fn len(s: &str) -> i64 {
    s.encode_utf16().count() as i64
}

/// Stands in for `-Infinity` (`Math.max()` of nothing); sums with it stay negative.
const NEG_INF: i64 = -(1 << 40);

/// `^\s*-\s+` replaced by `-` and `n` spaces.
fn fix_hyphen(desc: &str, n: i64) -> Option<String> {
    let rest = desc.trim_start_matches(is_js_ws);
    let after = rest.strip_prefix('-')?;
    let tail = after.trim_start_matches(is_js_ws);
    (tail.len() < after.len()).then(|| format!("-{}{tail}", spaces(n)))
}

/// `^\s*(?:[\-*]|\d+(?:\.|\)))\s+`.
fn starts_with_list_marker(text: &str) -> bool {
    let rest = text.trim_start_matches(is_js_ws);
    let after = if let Some(a) = rest.strip_prefix(['-', '*']) {
        a
    } else {
        let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        if digits == 0 {
            return false;
        }
        match rest[digits..].strip_prefix(['.', ')']) {
            Some(a) => a,
            None => return false,
        }
    };
    after.starts_with(is_js_ws)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Prop {
    Tag,
    Type,
    Name,
    Description,
    PostDelimiter,
    PostTag,
    PostType,
    PostName,
}

fn prop(t: &mut Tokens, p: Prop) -> &mut String {
    match p {
        Prop::Tag => &mut t.tag,
        Prop::Type => &mut t.typ,
        Prop::Name => &mut t.name,
        Prop::Description => &mut t.description,
        Prop::PostDelimiter => &mut t.post_delimiter,
        Prop::PostTag => &mut t.post_tag,
        Prop::PostType => &mut t.post_type,
        Prop::PostName => &mut t.post_name,
    }
}

impl CheckLineAlignment {
    fn spacing(&self, p: Prop) -> i64 {
        or_one(match p {
            Prop::PostDelimiter => self.spacings.post_delimiter,
            Prop::PostTag => self.spacings.post_tag,
            Prop::PostType => self.spacings.post_type,
            Prop::PostName => self.spacings.post_name,
            _ => None,
        })
    }

    fn post_hyphen(&self) -> i64 {
        self.spacings.post_hyphen.unwrap_or(1)
    }

    /// `shouldAlign`: the line is a `tags` tag line, or an untagged line whose nearest tag
    /// line above is one (or that has no tag line above).
    fn should_align(&self, b: &Block, index: usize) -> bool {
        let tag_of = |i: usize| b.lines[b.source[i]].tokens.tag.replacen('@', "", 1);
        let tag = tag_of(index);
        if self.tags.contains(&tag) {
            return true;
        }
        if !tag.is_empty() {
            return false;
        }
        for i in (0..=index).rev() {
            let t = tag_of(i);
            if !t.is_empty() {
                return self.tags.contains(&t);
            }
        }
        true
    }

    /// The `"always"` rebuild of the block.
    fn aligned(&self, doc: &Doc) -> Block {
        let mut b = doc.block.clone();
        let indent = doc.indent.as_str();
        let n = b.source.len();
        let (mut w_name, mut w_tag, mut w_type) = (0i64, 0i64, 0i64);
        for i in 0..n {
            if self.should_align(&doc.block, i) {
                let t = &doc.block.lines[doc.block.source[i]].tokens;
                w_name = w_name.max(len(&t.name));
                w_tag = w_tag.max(len(&t.tag));
                w_type = w_type.max(len(&t.typ));
            }
        }
        let tags = &doc.block.tags;
        let has_no_types = tags.iter().all(|t| t.typ.is_empty());
        let max_of = |named: bool| {
            tags.iter()
                .filter(|t| t.name.is_empty() != named)
                .map(|t| len(&t.tag))
                .max()
                .map_or(NEG_INF, |m| m + 1)
        };
        let (max_named, max_unnamed) = (max_of(true), max_of(false));
        let mut into_tags = false;
        let mut tag_indent_mode = false;
        for i in 0..n {
            let orig = doc.block.lines[doc.block.source[i]].tokens.clone();
            let indent_tag = !self.disable_wrap_indent
                && tag_indent_mode
                && orig.tag.is_empty()
                && !orig.description.is_empty();
            let mut t = orig.clone();
            self.update(
                &mut t,
                &doc.block,
                i,
                indent,
                &mut into_tags,
                indent_tag,
                (w_name, w_tag, w_type),
                (has_no_types, max_named, max_unnamed),
            );
            b.lines[b.source[i]].tokens = t;
            if !self.disable_wrap_indent && !orig.tag.is_empty() {
                tag_indent_mode = true;
            }
        }
        b
    }

    #[allow(clippy::too_many_arguments)]
    fn update(
        &self,
        t: &mut Tokens,
        b: &Block,
        index: usize,
        indent: &str,
        into_tags: &mut bool,
        indent_tag: bool,
        width: (i64, i64, i64),
        typeless: (bool, i64, i64),
    ) {
        if !t.tag.is_empty() {
            *into_tags = true;
        }
        let empty =
            t.tag.is_empty() && t.name.is_empty() && t.typ.is_empty() && t.description.is_empty();
        if t.end == "*/" && empty {
            t.start = format!("{indent} ");
            return;
        }
        match t.delimiter.as_str() {
            "*" => t.start = format!("{indent} "),
            "/**" => t.start = indent.to_owned(),
            _ => {
                t.delimiter.clear();
                t.start = format!("{indent}  ");
            }
        }
        if !*into_tags {
            if t.description.is_empty() {
                t.post_delimiter.clear();
            } else if !self.preserve_main_post_delimiter {
                t.post_delimiter = " ".to_owned();
            }
            return;
        }
        if let Some(d) = fix_hyphen(&t.description, self.post_hyphen()) {
            t.description = d;
        }
        if !self.should_align(b, index) {
            return;
        }
        self.align_tokens(t, width, typeless);
        if !self.disable_wrap_indent && indent_tag {
            let (has_list, tag_start) = list_markers(b, index);
            if has_list && index > tag_start {
                let extra = list_extra_indent(b, index, tag_start);
                t.post_delimiter.push_str(&self.wrap_indent);
                t.post_delimiter.push_str(&extra);
            } else {
                t.post_delimiter.push_str(&self.wrap_indent);
            }
        }
    }

    fn align_tokens(&self, t: &mut Tokens, width: (i64, i64, i64), typeless: (bool, i64, i64)) {
        let (w_name, w_tag, w_type) = width;
        let (has_no_types, max_named, max_unnamed) = typeless;
        let (mut after_delim, mut after_name, mut after_tag, mut after_type) =
            (false, false, false, false);
        if t.description.is_empty() {
            after_name = true;
            t.post_name.clear();
            if t.name.is_empty() {
                after_type = true;
                t.post_type.clear();
                if t.typ.is_empty() {
                    after_tag = true;
                    t.post_tag.clear();
                    if t.tag.is_empty() {
                        after_delim = true;
                    }
                }
            }
        }
        let mut name_adjust = 0;
        let mut type_adjust = 0;
        if has_no_types {
            after_tag = true;
            t.post_tag.clear();
            let tag_len = len(&t.tag);
            if t.name.is_empty() {
                name_adjust = max_named - tag_len;
            } else {
                name_adjust = if max_named > max_unnamed {
                    0
                } else {
                    0.max(max_unnamed - (tag_len + len(&t.name) + 1))
                };
                type_adjust = max_named - tag_len;
            }
        }
        if t.tag.is_empty() && !t.typ.is_empty() {
            return;
        }
        t.post_delimiter = if after_delim {
            String::new()
        } else {
            spaces(or_one(self.spacings.post_delimiter))
        };
        if !after_tag {
            t.post_tag = spaces(w_tag - len(&t.tag) + or_one(self.spacings.post_tag));
        }
        if !after_type {
            t.post_type =
                spaces(w_type - len(&t.typ) + or_one(self.spacings.post_type) + type_adjust);
        }
        if !after_name {
            t.post_name = if w_name == 0 {
                String::new()
            } else {
                spaces(w_name - len(&t.name) + or_one(self.spacings.post_name) + name_adjust)
            };
        }
    }

    /// `"never"`: one tag's first line; true when it reported (the plugin's fix then
    /// leaves the tag with only its first line, so its wrap indent is not checked).
    fn check_not_aligned(
        &self,
        cx: &mut Ctx<'_, '_>,
        f: &Finder<'_>,
        doc: &mut Doc,
        structure: &Structure,
        tag: usize,
    ) -> bool {
        let named = structure.might_name_or_namepath(&doc.block.tags[tag].tag);
        let spacers: &[Prop] = if named {
            &[
                Prop::PostDelimiter,
                Prop::PostTag,
                Prop::PostType,
                Prop::PostName,
            ]
        } else {
            &[Prop::PostDelimiter, Prop::PostTag, Prop::PostType]
        };
        let contents: &[Prop] = if named {
            &[Prop::Tag, Prop::Type, Prop::Name, Prop::Description]
        } else {
            &[Prop::Tag, Prop::Type, Prop::Description]
        };
        let line = doc.block.tags[tag].source[0];
        let mut t = doc.block.lines[line].tokens.clone();
        let ph = self.post_hyphen();
        let desc = &t.description;
        let lead = desc.trim_start_matches(is_js_ws);
        let has_no_hyphen = !lead
            .strip_prefix('-')
            .is_some_and(|a| a.starts_with(is_js_ws));
        let has_exact_hyphen = lead.strip_prefix('-').is_some_and(|a| {
            let tail = a.trim_start_matches(is_js_ws);
            let n = a[..a.len() - tail.len()].chars().count() as i64;
            n == ph && ph >= 0
        });
        let bad_spacer = spacers.iter().any(|&sp| {
            let l = len(prop(&mut t, sp));
            l != self.spacing(sp) && l != 0
        });
        if !bad_spacer && (has_no_hyphen || has_exact_hyphen) {
            return false;
        }
        for (idx, &sp) in spacers.iter().enumerate() {
            if !prop(&mut t, contents[idx]).is_empty() {
                *prop(&mut t, sp) = spaces(self.spacing(sp));
                for j in idx + 1..spacers.len() {
                    let empty = prop(&mut t, spacers[j]).is_empty();
                    if empty {
                        prop(&mut t, contents[j]).clear();
                    } else {
                        break;
                    }
                }
            } else {
                prop(&mut t, sp).clear();
            }
        }
        if !has_exact_hyphen && let Some(d) = fix_hyphen(&t.description, ph) {
            t.description = d;
        }
        doc.block.lines[line].tokens = t;
        let rel = doc.block.tag_line(tag);
        let (span, loc) = doc.report_at(f, rel, None);
        cx.report(span, "Expected JSDoc block lines to not be aligned.")
            .loc(loc)
            .fix(doc.fix());
        true
    }

    /// The wrap indent of continuation lines; true when it reported (one per block).
    fn check_wrap(&self, cx: &mut Ctx<'_, '_>, f: &Finder<'_>, doc: &mut Doc, tag: usize) -> bool {
        let source = doc.block.tags[tag].source.clone();
        for idx in 1..source.len() {
            let t = &doc.block.lines[source[idx]].tokens;
            if t.description.is_empty() || !t.typ.is_empty() || !t.name.is_empty() {
                continue;
            }
            let mut pd = t.post_delimiter.chars();
            let first = pd.next();
            let actual = pd.as_str();
            let wrap = self.wrap_indent.as_str();
            let correct = actual == wrap;
            let extra = actual.len() > wrap.len() && actual.starts_with(wrap);
            let in_list = (0..=idx).any(|i| {
                let d = &doc.block.lines[source[i]].tokens.description;
                i > 0 && !d.is_empty() && starts_with_list_marker(d)
            });
            if self.disable_wrap_indent || correct || (extra && in_list) {
                continue;
            }
            let new_pd = format!("{}{wrap}", first.map(String::from).unwrap_or_default());
            doc.block.lines[source[idx]].tokens.post_delimiter = new_pd;
            let rel = doc.block.tag_line(tag) + idx as i64;
            let (span, loc) = doc.report_at(f, rel, None);
            cx.report(span, "Expected wrap indent")
                .loc(loc)
                .fix(doc.fix());
            return true;
        }
        false
    }
}

/// `checkForListMarkers`: whether the tag holding line `index` has a list item up to it,
/// and where the tag starts.
fn list_markers(b: &Block, index: usize) -> (bool, usize) {
    let tok = |i: usize| &b.lines[b.source[i]].tokens;
    let mut start = index;
    while start > 0 && tok(start).tag.is_empty() {
        start -= 1;
    }
    let has = (start..=index).any(|i| {
        let d = &tok(i).description;
        i != start && !d.is_empty() && starts_with_list_marker(d)
    });
    (has, start)
}

/// `calculateListExtraIndent`: the padding of line `index` beyond the tag's first
/// continuation line.
fn list_extra_indent(b: &Block, index: usize, tag_start: usize) -> String {
    let tok = |i: usize| &b.lines[b.source[i]].tokens;
    let first = (tag_start + 1..b.source.len())
        .find(|&i| !tok(i).description.is_empty() && tok(i).tag.is_empty())
        .map(|i| len(&tok(i).post_delimiter));
    let cur = len(&tok(index).post_delimiter);
    match first {
        Some(f) if cur > f => spaces(cur - f),
        _ => String::new(),
    }
}

impl Rule for CheckLineAlignment {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let settings_value = cx
            .env()
            .settings
            .get("jsdoc")
            .cloned()
            .unwrap_or(Value::Null);
        let src = cx.file.src;
        each_block(cx, |cx, f, doc| {
            let text = doc.comment.text(src);
            if self.style == Style::Always {
                if !text.contains('\n') {
                    return;
                }
                let formatted = self.aligned(doc).stringify();
                let formatted = formatted.trim_start_matches(is_js_ws);
                if text != formatted {
                    cx.report(doc.comment, "Expected JSDoc block lines to be aligned.")
                        .fix(crate::diagnostic::Fix::replace(
                            doc.comment,
                            formatted.to_owned(),
                        ));
                }
                return;
            }
            let structure = Structure::new(
                SMode::from_setting(f.settings.mode.as_str()),
                &settings_value,
            );
            let found: Vec<usize> = (0..doc.block.tags.len())
                .filter(|&i| self.tags.contains(&doc.block.tags[i].tag))
                .collect();
            let mut cut = vec![false; doc.block.tags.len()];
            if self.style != Style::Any {
                for &i in &found {
                    cut[i] = self.check_not_aligned(cx, f, doc, &structure, i);
                }
            }
            for &i in &found {
                if !cut[i] && doc.block.tags[i].source.len() > 1 && self.check_wrap(cx, f, doc, i) {
                    return;
                }
            }
        });
    }
}
