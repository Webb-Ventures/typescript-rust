//! `jsdoc/sort-tags`: tags follow `tagSequence` order, with `linesBetween` blank lines
//! between groups and none inside a group.
//!
//! Order: a tag's rank is its position in the flattened `tagSequence` (unlisted tags rank at
//! `-other`, or after everything); with `alphabetizeExtras`, unlisted tags of equal rank sort
//! by name. The plugin sorts with the engine's `Array.prototype.sort` and remembers, among
//! the comparisons that moved a tag forward, the earliest original tag compared against; the
//! report goes on that tag's line. For the sort's comparisons to come out the same, the sort
//! here is the one V8 uses on short arrays: count the leading run (reversing it when strictly
//! descending), then binary insertion. The fix removes every tag from the reported one on and
//! adds them back in sorted order (their lines copied, closing marks dropped), at the
//! position where the first moved tag began.
//!
//! When sorted, each group's last tag (walking the tags in order through each group in turn)
//! must end in exactly `linesBetween` blank lines when another tag follows
//! (`reportTagGroupSpacing`); otherwise any other tag followed by a tag and ending in blank
//! lines reports intra-group spacing (unless `tagExceptions[tag]` allows that many). Fixes
//! accumulate over the reports, as the plugin edits one shared block.

use serde_json::Map;

use super::prelude::*;
use crate::jsdoc::{Block, Doc, Finder, Tokens};
use crate::jsdoc_util::{each_block, remove_tag};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "jsdoc/sort-tags",
        "Sorts tags by a specified sequence according to tag name, optionally adding line \
         breaks between tag groups",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<SortTags>,
};

const DEFAULT_ORDER: &[&str] = &[
    "summary",
    "typeSummary",
    "module",
    "exports",
    "file",
    "fileoverview",
    "overview",
    "import",
    "template",
    "typedef",
    "interface",
    "record",
    "name",
    "kind",
    "type",
    "alias",
    "external",
    "host",
    "callback",
    "func",
    "function",
    "method",
    "class",
    "constructor",
    "modifies",
    "mixes",
    "mixin",
    "mixinClass",
    "mixinFunction",
    "namespace",
    "borrows",
    "constructs",
    "lends",
    "implements",
    "requires",
    "desc",
    "description",
    "classdesc",
    "tutorial",
    "copyright",
    "license",
    "internal",
    "overload",
    "const",
    "constant",
    "final",
    "global",
    "readonly",
    "abstract",
    "virtual",
    "var",
    "member",
    "memberof",
    "memberof!",
    "inner",
    "instance",
    "inheritdoc",
    "inheritDoc",
    "override",
    "hideconstructor",
    "param",
    "arg",
    "argument",
    "prop",
    "property",
    "return",
    "returns",
    "async",
    "generator",
    "default",
    "defaultvalue",
    "enum",
    "augments",
    "extends",
    "throws",
    "exception",
    "yield",
    "yields",
    "event",
    "fires",
    "emits",
    "listens",
    "this",
    "satisfies",
    "static",
    "private",
    "protected",
    "public",
    "access",
    "package",
    "-other",
    "see",
    "example",
    "closurePrimitive",
    "customElement",
    "expose",
    "hidden",
    "idGenerator",
    "meaning",
    "ngInject",
    "owner",
    "wizaction",
    "define",
    "dict",
    "export",
    "externs",
    "implicitCast",
    "noalias",
    "nocollapse",
    "nocompile",
    "noinline",
    "nosideeffects",
    "polymer",
    "polymerBehavior",
    "preserve",
    "struct",
    "suppress",
    "unrestricted",
    "category",
    "ignore",
    "author",
    "version",
    "variation",
    "since",
    "deprecated",
    "todo",
];

#[derive(Debug)]
struct SortTags {
    alphabetize_extras: bool,
    lines_between: i64,
    report_intra: bool,
    report_groups: bool,
    exceptions: Map<String, Value>,
    groups: Vec<Vec<String>>,
}

impl FromOptions for SortTags {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        let groups = match o.get("tagSequence").and_then(Value::as_array) {
            Some(seq) => seq
                .iter()
                .map(|g| {
                    g.get("tags")
                        .and_then(Value::as_array)
                        .map(|t| {
                            t.iter()
                                .filter_map(Value::as_str)
                                .map(str::to_owned)
                                .collect()
                        })
                        .unwrap_or_default()
                })
                .collect(),
            None => vec![DEFAULT_ORDER.iter().map(|s| (*s).to_owned()).collect()],
        };
        Ok(SortTags {
            alphabetize_extras: bool_field(o, "alphabetizeExtras", false),
            lines_between: o.get("linesBetween").and_then(Value::as_i64).unwrap_or(1),
            report_intra: bool_field(o, "reportIntraTagGroupSpacing", true),
            report_groups: bool_field(o, "reportTagGroupSpacing", true),
            exceptions: o
                .get("tagExceptions")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default(),
            groups,
        })
    }
}

/// The comparator's view of one tag: its name, original index and the line count up to and
/// including it (`originalLine`).
struct Item<'a> {
    tag: &'a str,
    index: usize,
    line: usize,
}

struct Sorter<'a> {
    list: &'a [String],
    end: usize,
    alphabetize: bool,
    /// (`originalLine`, `originalIndex`) of the earliest tag something moved ahead of.
    first_changed: Option<(usize, usize)>,
}

impl Sorter<'_> {
    fn compare(&mut self, new: &Item<'_>, old: &Item<'_>) -> i32 {
        if new.tag == old.tag {
            return 0;
        }
        let pos = |t: &str| self.list.iter().position(|x| x == t);
        let new_pos = pos(new.tag);
        let pref_new = new_pos.unwrap_or(self.end);
        let pref_old = pos(old.tag).unwrap_or(self.end);
        let ahead = if pref_new != pref_old {
            pref_new < pref_old
        } else if !self.alphabetize || new_pos.is_some() {
            return 0;
        } else {
            // JavaScript string order compares UTF-16 code units.
            new.tag.encode_utf16().lt(old.tag.encode_utf16())
        };
        if !ahead {
            return 1;
        }
        if self.first_changed.is_none_or(|(l, _)| old.line < l) {
            self.first_changed = Some((old.line, old.index));
        }
        -1
    }

    /// V8's sort for short arrays: the leading run, then binary insertion.
    fn sort(&mut self, items: &mut [Item<'_>]) {
        let n = items.len();
        if n < 2 {
            return;
        }
        let mut run = 2;
        let descending = self.compare(&items[1], &items[0]) < 0;
        while run < n {
            let order = self.compare(&items[run], &items[run - 1]);
            if (descending && order >= 0) || (!descending && order < 0) {
                break;
            }
            run += 1;
        }
        if descending {
            items[..run].reverse();
        }
        for start in run..n {
            let (mut left, mut right) = (0, start);
            while left < right {
                let mid = left + (right - left) / 2;
                if self.compare(&items[start], &items[mid]) < 0 {
                    right = mid;
                } else {
                    left = mid + 1;
                }
            }
            items[left..=start].rotate_right(1);
        }
    }
}

fn is_blank(t: &Tokens) -> bool {
    t.tag.is_empty() && t.typ.is_empty() && t.name.is_empty() && t.description.is_empty()
}

/// The blank lines a tag ends with (a closing line doesn't count, content resets).
fn trailing_blank_lines(b: &Block, ti: usize) -> i64 {
    b.tags[ti].source.iter().fold(0, |acc, &id| {
        let t = &b.lines[id].tokens;
        if is_blank(t) {
            acc + i64::from(t.end.is_empty())
        } else {
            0
        }
    })
}

fn renumber_all(b: &mut Block) {
    for k in 0..b.source.len() {
        let id = b.source[k];
        b.lines[id].number = k as i64;
    }
}

/// `addTag(name, number, tokens)` / `addLine(index, tokens)`: inserts a line, then bumps the
/// numbers of the lines after `bump_from`.
fn insert_line(b: &mut Block, at: usize, number: i64, bump_from: usize, tokens: Tokens) {
    let at = at.min(b.source.len());
    let id = b.new_line(number, tokens);
    b.source.insert(at, id);
    for k in (bump_from + 1).min(b.source.len())..b.source.len() {
        let l = b.source[k];
        b.lines[l].number += 1;
    }
}

impl SortTags {
    fn check(&self, cx: &mut Ctx<'_, '_>, finder: &Finder<'_>, doc: &mut Doc) {
        let list: Vec<String> = self.groups.iter().flatten().cloned().collect();
        let end = list
            .iter()
            .position(|t| t == "-other")
            .unwrap_or(list.len());
        let b = &doc.block;
        let mut count = 0;
        let mut items: Vec<Item<'_>> = b
            .tags
            .iter()
            .enumerate()
            .map(|(index, t)| {
                count += t.source.len();
                Item {
                    tag: &t.tag,
                    index,
                    line: count,
                }
            })
            .collect();
        let mut sorter = Sorter {
            list: &list,
            end,
            alphabetize: self.alphabetize_extras,
            first_changed: None,
        };
        if items.len() < 64 {
            sorter.sort(&mut items);
        } else {
            // Longer arrays merge runs; keep the result stable and the report approximate.
            items.sort_by(|a, b| sorter.compare(a, b).cmp(&0));
        }
        let order: Vec<usize> = items.iter().map(|i| i.index).collect();
        match sorter.first_changed {
            Some((_, first)) => self.report_order(cx, finder, doc, &list, &order, first),
            None => self.check_spacing(cx, finder, doc),
        }
    }

    fn report_order(
        &self,
        cx: &mut Ctx<'_, '_>,
        finder: &Finder<'_>,
        doc: &mut Doc,
        list: &[String],
        order: &[usize],
        first: usize,
    ) {
        let b = &mut doc.block;
        let line = b.tag_line(first);
        let first_line = b
            .source
            .iter()
            .find(|&&id| !b.lines[id].tokens.tag.is_empty())
            .map_or(0, |&id| b.lines[id].number);
        let prior: usize = b.tags[..first]
            .iter()
            .map(|t| t.source.len().saturating_sub(1))
            .sum();
        let initial = first_line as usize + first + prior;
        // The sorted tags' lines, as they were before the fix touches them.
        let moved: Vec<Vec<Tokens>> = order[first..]
            .iter()
            .map(|&ti| {
                b.tags[ti]
                    .source
                    .iter()
                    .map(|&id| b.lines[id].tokens.clone())
                    .collect()
            })
            .collect();
        for ti in first..b.tags.len() {
            remove_tag(b, ti, 0);
        }
        let mut extra = 0;
        for (idx, lines) in moved.into_iter().enumerate() {
            let mut lines = lines.into_iter();
            let Some(mut head) = lines.next() else {
                continue;
            };
            head.end.clear();
            let number = extra + initial + idx;
            insert_line(b, number, number as i64, number, head);
            for t in lines {
                if !t.end.is_empty() {
                    continue;
                }
                let at = extra + initial + idx + 1;
                let number = at
                    .checked_sub(1)
                    .and_then(|p| b.source.get(p))
                    .map_or(0, |&id| b.lines[id].number)
                    + 1;
                insert_line(b, at, number, number.max(0) as usize, t);
                extra += 1;
            }
        }
        let shown = if list.is_empty() {
            "(alphabetical)".to_owned()
        } else {
            list.join(", ")
        };
        let span = doc.line_span(finder, line);
        cx.report(
            span,
            format!("Tags are not in the prescribed order: {shown}"),
        )
        .fix(doc.fix());
    }

    fn check_spacing(&self, cx: &mut Ctx<'_, '_>, finder: &Finder<'_>, doc: &mut Doc) {
        let n = doc.block.tags.len();
        let mut last_of_group: Vec<usize> = Vec::new();
        let mut bad: Vec<usize> = Vec::new();
        let mut idx = 0;
        for group in &self.groups {
            let mut last = None;
            while let Some(t) = doc.block.tags.get(idx) {
                let listed = |name: &str| group.iter().any(|g| g == name);
                if !listed(&t.tag)
                    && (!listed("-other") || self.groups.iter().any(|g| g.contains(&t.tag)))
                {
                    break;
                }
                last = Some(idx);
                idx += 1;
            }
            if let Some(l) = last {
                last_of_group.push(l);
                let ct = trailing_blank_lines(&doc.block, l);
                if ct != self.lines_between && idx < n {
                    bad.push(l);
                }
            }
        }
        if self.report_groups && !bad.is_empty() {
            for ti in bad {
                let line = doc.block.tag_line(ti);
                let name = format!("@{}", doc.block.tags[ti].tag);
                self.fix_group(&mut doc.block, &name);
                let span = doc.line_span(finder, line);
                cx.report(span, "Tag groups do not have the expected whitespace")
                    .fix(doc.fix());
            }
            return;
        }
        if !self.report_intra {
            return;
        }
        let last_names: Vec<String> = last_of_group
            .iter()
            .map(|&i| format!("@{}", doc.block.tags[i].tag))
            .collect();
        for ti in 0..n {
            if ti + 1 >= n || last_of_group.contains(&ti) {
                continue;
            }
            let ct = trailing_blank_lines(&doc.block, ti);
            let allowed = self
                .exceptions
                .get(&doc.block.tags[ti].tag)
                .and_then(Value::as_f64)
                .filter(|&e| e != 0.0);
            if ct == 0 || allowed.is_some_and(|e| e >= ct as f64) {
                continue;
            }
            let line = doc.block.tag_line(ti);
            fix_intra(&mut doc.block, &last_names);
            let span = doc.line_span(finder, line);
            cx.report(span, "Intra-group tags have unexpected whitespace")
                .fix(doc.fix());
        }
    }

    /// After the last line of the run of `name` tags that starts at its first use, makes the
    /// blank lines before the next tag number `linesBetween`.
    fn fix_group(&self, b: &mut Block, name: &str) {
        let Some(start) = b
            .source
            .iter()
            .position(|&id| b.lines[id].tokens.tag == name)
        else {
            return;
        };
        let mut at = start;
        let mut blank = 0i64;
        loop {
            at += 1;
            let Some(&id) = b.source.get(at) else {
                return;
            };
            let t = &b.lines[id].tokens;
            if !t.tag.is_empty() {
                if t.tag == name {
                    blank = 0;
                    continue;
                }
                let diff = self.lines_between - blank;
                if diff > 0 {
                    let start_tok = b.lines[b.source[at - 1]].tokens.start.clone();
                    for _ in 0..diff {
                        let id = b.new_line(
                            0,
                            Tokens {
                                start: start_tok.clone(),
                                delimiter: "*".to_owned(),
                                ..Tokens::default()
                            },
                        );
                        b.source.insert(at, id);
                    }
                } else if diff < 0 {
                    let from = (at as i64 + diff).max(0) as usize;
                    b.source.drain(from..at);
                }
                break;
            }
            if t.typ.is_empty() && t.name.is_empty() && t.description.is_empty() {
                blank += 1;
            } else {
                blank = 0;
            }
        }
        renumber_all(b);
    }
}

/// Drops blank lines after a tag that more tags follow: after a group's last tag name, only
/// when that name appears again later. Like the plugin's loop over a list it splices, the
/// line after a dropped one is skipped.
fn fix_intra(b: &mut Block, last_names: &[String]) {
    let mut current: Option<String> = None;
    let mut k = 0;
    while k < b.source.len() {
        let t = b.lines[b.source[k]].tokens.clone();
        k += 1;
        if !t.tag.is_empty() {
            current = Some(t.tag.clone());
        }
        let Some(cur) = &current else {
            continue;
        };
        if !(is_blank(&t) && t.end.is_empty()) {
            continue;
        }
        let at = k - 1;
        let rest = &b.source[at + 1..];
        let keep = if last_names.iter().any(|n| n == cur) {
            !rest.iter().any(|&id| b.lines[id].tokens.tag == *cur)
        } else {
            let mut found = false;
            for &id in rest {
                let n = &b.lines[id].tokens;
                if !n.end.is_empty() {
                    break;
                }
                if !n.tag.is_empty() {
                    found = true;
                    break;
                }
            }
            !found
        };
        if !keep {
            b.source.remove(at);
            renumber_all(b);
        }
    }
}

impl Rule for SortTags {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        each_block(cx, |cx, finder, doc| self.check(cx, finder, doc));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sorted(tags: &[&str], list: &[&str], alphabetize: bool) -> (Vec<usize>, Option<usize>) {
        let list: Vec<String> = list.iter().map(|s| (*s).to_owned()).collect();
        let end = list
            .iter()
            .position(|t| t == "-other")
            .unwrap_or(list.len());
        let mut items: Vec<Item<'_>> = tags
            .iter()
            .enumerate()
            .map(|(index, t)| Item {
                tag: t,
                index,
                line: index + 1,
            })
            .collect();
        let mut s = Sorter {
            list: &list,
            end,
            alphabetize,
            first_changed: None,
        };
        s.sort(&mut items);
        (
            items.iter().map(|i| i.index).collect(),
            s.first_changed.map(|f| f.1),
        )
    }

    #[test]
    fn sort_and_first_changed() {
        assert_eq!(
            sorted(&["returns", "param", "param"], &["param", "returns"], false),
            (vec![1, 2, 0], Some(0))
        );
        assert_eq!(
            sorted(&["param", "returns"], &["param", "returns"], false),
            (vec![0, 1], None)
        );
        assert_eq!(
            sorted(&["xyz", "abc", "abc", "def", "xyz"], &[], true),
            (vec![1, 2, 3, 0, 4], Some(0))
        );
    }
}
