//! Running a parsed [`crate::regex::Regex`] with capture groups, for rules that need what a
//! pattern matched (`String.prototype.matchAll`), not just whether it matches. A
//! backtracking matcher over the tree: captures, backreferences, lookahead and lookbehind,
//! and the `i`, `m`, `s`, `g` and `y` flags. Runaway backtracking gives up (no match).

use crate::regex::{AssertKind, Flags, RId, RKind, Regex, SetKind, parse};
use std::cell::{Cell, RefCell};

/// Step budget for one match attempt before it is abandoned.
const MAX_STEPS: u32 = 200_000;
/// Nesting of the matcher's continuations (it recurses once per element a group or
/// quantified group matches), kept well inside a 2 MB thread stack. Quantified single
/// characters (`.*`, `[^\n]+`) are matched in a loop and don't nest.
const MAX_DEPTH: u32 = 400;

type Span = (usize, usize);
/// Whether the input is code points, the input, and each match's group spans.
type Matches = (bool, Vec<u32>, Vec<Vec<Option<Span>>>);

struct Exec<'r, 'a> {
    r: &'r Regex<'a>,
    input: Vec<u32>,
    icase: bool,
    /// `i` with `u`/`v`: simple case folding, where `ſ` is `s` and the Kelvin sign is `k`
    /// (both then also count as word characters for `\b`).
    unicode_fold: bool,
    multiline: bool,
    dotall: bool,
    steps: Cell<u32>,
    depth: Cell<u32>,
    caps: RefCell<Vec<Option<Span>>>,
}

fn fold(c: u32) -> u32 {
    char::from_u32(c)
        .and_then(|ch| ch.to_lowercase().next())
        .map_or(c, |l| l as u32)
}

fn is_word(c: u32) -> bool {
    char::from_u32(c).is_some_and(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

fn is_line_terminator(c: u32) -> bool {
    matches!(c, 0x0A | 0x0D | 0x2028 | 0x2029)
}

impl Exec<'_, '_> {
    fn exhausted(&self) -> bool {
        let n = self.steps.get() + 1;
        self.steps.set(n);
        n > MAX_STEPS
    }

    fn same(&self, a: u32, b: u32) -> bool {
        a == b || (self.icase && fold(self.unfold(a)) == fold(self.unfold(b)))
    }

    fn unfold(&self, c: u32) -> u32 {
        match c {
            0x17F if self.unicode_fold => 's' as u32,
            0x212A if self.unicode_fold => 'k' as u32,
            _ => c,
        }
    }

    fn word(&self, c: u32) -> bool {
        is_word(self.unfold(c))
    }

    fn in_set(&self, kind: SetKind, negate: bool, c: u32) -> bool {
        let ch = char::from_u32(c);
        let hit = match kind {
            SetKind::Digit => ch.is_some_and(|x| x.is_ascii_digit()),
            SetKind::Space => ch.is_some_and(|x| x.is_whitespace() || x == '\u{FEFF}'),
            SetKind::Word => is_word(c),
            SetKind::Property | SetKind::StringProperty => ch.is_some_and(char::is_alphabetic),
        };
        hit != negate
    }

    fn in_class(&self, id: RId, c: u32) -> bool {
        let r = self.r;
        match r.kind(id) {
            RKind::Char { value } => self.same(*value, c),
            RKind::Range => {
                let ch = &r.node(id).children;
                let lo = r.char_value(ch[0]).unwrap_or(0);
                let hi = r.char_value(ch[1]).unwrap_or(0);
                (lo..=hi).contains(&c) || (self.icase && (lo..=hi).contains(&fold(c)))
            }
            RKind::CharSet { kind, negate } => self.in_set(*kind, *negate, c),
            RKind::Class { negate } => {
                r.node(id).children.iter().any(|&k| self.in_class(k, c)) != *negate
            }
            RKind::ClassStrings => r.node(id).children.iter().any(|&s| {
                let chars = &r.node(s).children;
                chars.len() == 1 && self.in_class(chars[0], c)
            }),
            RKind::SetOp { intersection } => {
                let ch = &r.node(id).children;
                let (a, b) = (self.in_class(ch[0], c), self.in_class(ch[1], c));
                if *intersection { a && b } else { a && !b }
            }
            _ => false,
        }
    }

    fn alts(&self, id: RId, pos: usize, k: &mut dyn FnMut(usize) -> bool) -> bool {
        let r = self.r;
        r.node(id)
            .children
            .iter()
            .any(|&a| self.seq(&r.node(a).children, pos, k))
    }

    fn seq(&self, items: &[RId], pos: usize, k: &mut dyn FnMut(usize) -> bool) -> bool {
        match items.split_first() {
            None => k(pos),
            Some((&first, rest)) => self.node(first, pos, &mut |p| self.seq(rest, p, k)),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn rep(
        &self,
        child: RId,
        min: u32,
        max: Option<u32>,
        greedy: bool,
        count: u32,
        pos: usize,
        k: &mut dyn FnMut(usize) -> bool,
    ) -> bool {
        if self.exhausted() {
            return false;
        }
        if count < min {
            return self.node(child, pos, &mut |p| {
                self.rep(child, min, max, greedy, count + 1, p, k)
            });
        }
        let more = max.is_none_or(|m| count < m);
        let again = |p: usize, k: &mut dyn FnMut(usize) -> bool| {
            p != pos && self.rep(child, min, max, greedy, count + 1, p, k)
        };
        if greedy {
            (more && self.node(child, pos, &mut |p| again(p, k))) || k(pos)
        } else {
            k(pos) || (more && self.node(child, pos, &mut |p| again(p, k)))
        }
    }

    /// Whether the one-character element `id` matches the input at `pos`.
    fn single(&self, id: RId, pos: usize) -> bool {
        let Some(c) = self.input.get(pos).copied() else {
            return false;
        };
        match self.r.kind(id) {
            RKind::Char { value } => self.same(*value, c),
            RKind::Dot => self.dotall || !is_line_terminator(c),
            RKind::CharSet { kind, negate } => self.in_set(*kind, *negate, c),
            RKind::Class { .. } => self.in_class(id, c),
            _ => false,
        }
    }

    /// A quantified one-character element, without recursing per character.
    fn rep_single(
        &self,
        child: RId,
        min: u32,
        max: Option<u32>,
        greedy: bool,
        pos: usize,
        k: &mut dyn FnMut(usize) -> bool,
    ) -> bool {
        let limit = max.map_or(usize::MAX, |m| m as usize);
        let mut n = 0usize;
        while n < limit && self.single(child, pos + n) {
            n += 1;
        }
        let min = min as usize;
        if n < min {
            return false;
        }
        if greedy {
            (min..=n).rev().any(|i| !self.exhausted() && k(pos + i))
        } else {
            (min..=n).any(|i| !self.exhausted() && k(pos + i))
        }
    }

    fn node(&self, id: RId, pos: usize, k: &mut dyn FnMut(usize) -> bool) -> bool {
        if self.exhausted() || self.depth.get() >= MAX_DEPTH {
            return false;
        }
        self.depth.set(self.depth.get() + 1);
        let hit = self.step(id, pos, k);
        self.depth.set(self.depth.get() - 1);
        hit
    }

    fn step(&self, id: RId, pos: usize, k: &mut dyn FnMut(usize) -> bool) -> bool {
        let r = self.r;
        let cur = self.input.get(pos).copied();
        match r.kind(id) {
            RKind::Pattern | RKind::Group => self.alts(id, pos, k),
            RKind::Capture { index, .. } => {
                let slot = *index as usize;
                self.alts(id, pos, &mut |p| {
                    let old = self.caps.borrow().get(slot).copied().flatten();
                    if let Some(c) = self.caps.borrow_mut().get_mut(slot) {
                        *c = Some((pos, p));
                    }
                    if k(p) {
                        return true;
                    }
                    if let Some(c) = self.caps.borrow_mut().get_mut(slot) {
                        *c = old;
                    }
                    false
                })
            }
            RKind::Alternative => self.seq(&r.node(id).children, pos, k),
            RKind::Lookaround { behind, negate } => {
                let found = if *behind {
                    (0..=pos).rev().any(|s| self.alts(id, s, &mut |e| e == pos))
                } else {
                    self.alts(id, pos, &mut |_| true)
                };
                found != *negate && k(pos)
            }
            RKind::Quantifier { min, max, greedy } => {
                let child = r.node(id).children[0];
                if matches!(
                    r.kind(child),
                    RKind::Char { .. } | RKind::Dot | RKind::CharSet { .. } | RKind::Class { .. }
                ) {
                    return self.rep_single(child, *min, *max, *greedy, pos, k);
                }
                self.rep(child, *min, *max, *greedy, 0, pos, k)
            }
            RKind::Char { value } => cur.is_some_and(|c| self.same(*value, c)) && k(pos + 1),
            RKind::Dot => cur.is_some_and(|c| self.dotall || !is_line_terminator(c)) && k(pos + 1),
            RKind::CharSet { kind, negate } => {
                cur.is_some_and(|c| self.in_set(*kind, *negate, c)) && k(pos + 1)
            }
            RKind::Class { .. } => cur.is_some_and(|c| self.in_class(id, c)) && k(pos + 1),
            RKind::Assertion(a) => {
                let before = pos.checked_sub(1).and_then(|p| self.input.get(p).copied());
                let ok = match a {
                    AssertKind::Start => {
                        pos == 0 || (self.multiline && before.is_some_and(is_line_terminator))
                    }
                    AssertKind::End => {
                        pos == self.input.len()
                            || (self.multiline && cur.is_some_and(is_line_terminator))
                    }
                    AssertKind::WordBoundary | AssertKind::NotWordBoundary => {
                        let b = before.is_some_and(|c| self.word(c))
                            != cur.is_some_and(|c| self.word(c));
                        b == (*a == AssertKind::WordBoundary)
                    }
                };
                ok && k(pos)
            }
            RKind::Backref { .. } => {
                let target = r
                    .backref_targets(id)
                    .into_iter()
                    .find_map(|c| match r.kind(c) {
                        RKind::Capture { index, .. } => {
                            self.caps.borrow().get(*index as usize).copied().flatten()
                        }
                        _ => None,
                    });
                let Some((s, e)) = target else {
                    return k(pos);
                };
                let len = e - s;
                if pos + len > self.input.len() {
                    return false;
                }
                (0..len).all(|i| self.same(self.input[s + i], self.input[pos + i])) && k(pos + len)
            }
            _ => false,
        }
    }
}

/// Every match of `pattern` in `text`: its groups as spans of the matcher's input units
/// (code points with `u`/`v`, else UTF-16 units), and the input. `None` when the pattern
/// does not parse.
fn exec_all(pattern: &str, flags: &str, text: &str) -> Option<Matches> {
    let r = parse(pattern, Flags::new(flags)).ok()?;
    let unicode = r.flags.unicode || r.flags.sets;
    let input: Vec<u32> = if unicode {
        text.chars().map(|c| c as u32).collect()
    } else {
        text.encode_utf16().map(u32::from).collect()
    };
    let ex = Exec {
        r: &r,
        input,
        icase: flags.contains('i'),
        unicode_fold: unicode && flags.contains('i'),
        multiline: flags.contains('m'),
        dotall: flags.contains('s'),
        steps: Cell::new(0),
        depth: Cell::new(0),
        caps: RefCell::new(vec![None; r.captures.len() + 1]),
    };
    let global = flags.contains('g');
    let sticky = flags.contains('y');
    let len = ex.input.len();
    let mut out = Vec::new();
    let mut last = 0usize;
    while last <= len {
        let mut found: Option<(usize, Vec<Option<Span>>)> = None;
        let starts = if sticky {
            last..last + 1
        } else {
            last..len + 1
        };
        for s in starts {
            ex.steps.set(0);
            ex.caps.borrow_mut().iter_mut().for_each(|c| *c = None);
            let mut end = None;
            if ex.node(r.root, s, &mut |e| {
                end = Some(e);
                true
            }) && let Some(e) = end
            {
                let mut caps = ex.caps.borrow().clone();
                caps[0] = Some((s, e));
                found = Some((e, caps));
                break;
            }
        }
        let Some((end, caps)) = found else {
            break;
        };
        let start = caps[0].map_or(end, |c| c.0);
        out.push(caps);
        if !global {
            break;
        }
        last = if end == start { end + 1 } else { end };
    }
    Some((unicode, ex.input, out))
}

/// `text.matchAll(new RegExp(pattern, flags))`: every match's groups (index 0 is the whole
/// match; `None` for groups that took no part). `None` when the pattern does not parse.
/// Without the `g` flag only the first match is given.
pub fn match_all(pattern: &str, flags: &str, text: &str) -> Option<Vec<Vec<Option<String>>>> {
    let (unicode, input, matches) = exec_all(pattern, flags, text)?;
    let to_string = |s: &[u32]| -> String {
        if unicode {
            s.iter().filter_map(|&c| char::from_u32(c)).collect()
        } else {
            let units: Vec<u16> = s.iter().map(|&c| c as u16).collect();
            String::from_utf16_lossy(&units)
        }
    };
    Some(
        matches
            .iter()
            .map(|caps| {
                caps.iter()
                    .map(|c| c.map(|(a, b)| to_string(&input[a..b.max(a)])))
                    .collect()
            })
            .collect(),
    )
}

/// One match's group byte ranges (index 0 is the whole match; `None` for groups that did not
/// take part).
pub type Groups = Vec<Option<(usize, usize)>>;

/// The group spans (index 0 is the whole match) of each match of
/// `text.matchAll(new RegExp(pattern, flags))` as byte offsets into `text`. An offset between
/// the two UTF-16 halves of a character (patterns without `u`) is rounded down to the
/// character's start. `None` when the pattern does not parse.
pub fn match_groups(pattern: &str, flags: &str, text: &str) -> Option<Vec<Groups>> {
    let (unicode, input, matches) = exec_all(pattern, flags, text)?;
    // Input unit index -> byte offset.
    let mut offsets = Vec::with_capacity(input.len() + 1);
    for (i, c) in text.char_indices() {
        offsets.push(i);
        if !unicode && c.len_utf16() == 2 {
            offsets.push(i);
        }
    }
    offsets.push(text.len());
    let at = |u: usize| offsets.get(u).copied().unwrap_or(text.len());
    Some(
        matches
            .iter()
            .map(|caps| {
                caps.iter()
                    .map(|c| c.map(|(a, b)| (at(a), at(b.max(a)))))
                    .collect()
            })
            .collect(),
    )
}

/// The whole-match spans of `text.matchAll(new RegExp(pattern, flags))` as byte offsets
/// into `text` (see [`match_groups`]). `None` when the pattern does not parse.
pub fn match_spans(pattern: &str, flags: &str, text: &str) -> Option<Vec<(usize, usize)>> {
    let groups = match_groups(pattern, flags, text)?;
    Some(groups.iter().filter_map(|caps| caps[0]).collect())
}

/// Whether `new RegExp(pattern, flags).test(text)`; a pattern that does not parse never
/// matches.
pub fn test(pattern: &str, flags: &str, text: &str) -> bool {
    let flags: String = flags.chars().filter(|&c| c != 'g').collect();
    match_spans(pattern, &flags, text).is_some_and(|m| !m.is_empty())
}

/// `text.replace(new RegExp(pattern, flags), f)`: every match with `g`, else the first,
/// replaced by what `f` makes of its groups (index 0 is the whole match). `None` when the
/// pattern does not parse.
pub fn replace_with(
    pattern: &str,
    flags: &str,
    text: &str,
    mut f: impl FnMut(&[Option<&str>]) -> String,
) -> Option<String> {
    let groups = match_groups(pattern, flags, text)?;
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for caps in &groups {
        let Some((start, end)) = caps[0] else {
            continue;
        };
        if start < last {
            continue;
        }
        let strs: Vec<Option<&str>> = caps.iter().map(|c| c.map(|(a, b)| &text[a..b])).collect();
        out.push_str(&text[last..start]);
        out.push_str(&f(&strs));
        last = end;
    }
    out.push_str(&text[last..]);
    Some(out)
}

/// `text.replace(new RegExp(pattern, flags), replacement)` with the replacement's `$$`,
/// `$&`, `` $` ``, `$'` and `$1`..`$99` patterns.
pub fn replace(pattern: &str, flags: &str, text: &str, replacement: &str) -> Option<String> {
    let groups = match_groups(pattern, flags, text)?;
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for caps in &groups {
        let Some((start, end)) = caps[0] else {
            continue;
        };
        if start < last {
            continue;
        }
        out.push_str(&text[last..start]);
        expand(replacement, text, caps, &mut out);
        last = end;
    }
    out.push_str(&text[last..]);
    Some(out)
}

/// Appends `rep` with its `$` patterns filled in from one match.
fn expand(rep: &str, text: &str, caps: &[Option<(usize, usize)>], out: &mut String) {
    let (start, end) = caps[0].unwrap_or((0, 0));
    let group = |n: usize| caps.get(n).copied().flatten().map(|(a, b)| &text[a..b]);
    let b = rep.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'$' || i + 1 >= b.len() {
            let ch = rep[i..].chars().next().unwrap_or('$');
            out.push(ch);
            i += ch.len_utf8();
            continue;
        }
        match b[i + 1] {
            b'$' => {
                out.push('$');
                i += 2;
            }
            b'&' => {
                out.push_str(&text[start..end]);
                i += 2;
            }
            b'`' => {
                out.push_str(&text[..start]);
                i += 2;
            }
            b'\'' => {
                out.push_str(&text[end..]);
                i += 2;
            }
            d if d.is_ascii_digit() => {
                let one = usize::from(d - b'0');
                let two = b
                    .get(i + 2)
                    .filter(|c| c.is_ascii_digit())
                    .map(|&c| one * 10 + usize::from(c - b'0'));
                let groups = caps.len() - 1;
                match two {
                    Some(n) if n >= 1 && n <= groups => {
                        out.push_str(group(n).unwrap_or(""));
                        i += 3;
                    }
                    _ if one >= 1 && one <= groups => {
                        out.push_str(group(one).unwrap_or(""));
                        i += 2;
                    }
                    _ => {
                        out.push('$');
                        i += 1;
                    }
                }
            }
            _ => {
                out.push('$');
                i += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(p: &str, f: &str, t: &str) -> Vec<Vec<Option<String>>> {
        match_all(p, f, t).unwrap()
    }

    #[test]
    fn unicode_ignore_case_folds_long_s_and_kelvin() {
        assert!(test(r"\bsql\b", "iu", "the \u{17f}ql query"));
        assert!(!test(r"\bsql\b", "i", "the \u{17f}ql query"));
        assert!(test("k", "iu", "\u{212a}"));
    }

    #[test]
    fn replacing() {
        assert_eq!(
            replace("(a)(b)?", "g", "xaby a", "[$1$2$&$$]").as_deref(),
            Some("x[abab$]y [aa$]")
        );
        assert_eq!(replace("a", "", "aaa", "$'").as_deref(), Some("aaaa"));
        assert_eq!(
            replace_with("(o)", "gu", "foo", |g| g[1].unwrap_or("").to_uppercase()).as_deref(),
            Some("fOO")
        );
        assert!(test("^b$", "mg", "a\nb"));
        assert!(!test("^b$", "", "a\nb"));
    }

    #[test]
    fn spans() {
        assert_eq!(
            match_spans("o", "gu", "f\u{1F984}oo"),
            Some(vec![(5, 6), (6, 7)])
        );
        assert_eq!(
            match_spans("", "gu", "ab"),
            Some(vec![(0, 0), (1, 1), (2, 2)])
        );
        assert_eq!(match_spans("x", "g", "\u{1F984}x"), Some(vec![(4, 5)]));
        assert_eq!(match_spans("(", "gu", "x"), None);
    }

    #[test]
    fn groups_and_empty_matches() {
        let m = all("([^\n\r]*)", "gv", "MIT\nmore");
        let firsts: Vec<_> = m.iter().map(|g| g[1].clone().unwrap()).collect();
        assert_eq!(firsts, ["MIT", "", "more", ""]);
        assert_eq!(all("a", "g", "").len(), 0);
    }

    #[test]
    fn lookaround() {
        let m = all("(?<=prefix )MIT|GPL", "gv", "prefix MIT");
        assert_eq!(m[0][0].as_deref(), Some("MIT"));
        assert!(all("(?<=prefix )MIT", "gv", "MIT").is_empty());
        assert!(all("^([^\n]+)(?!\nCopyright)", "gv", "\nCopyright").is_empty());
        let m = all("^([^\n]+)\nCopyright", "gv", "MIT\nCopyright 2022");
        assert_eq!(m[0][1].as_deref(), Some("MIT"));
    }

    #[test]
    fn flags_and_backrefs() {
        assert_eq!(all("^b", "gm", "a\nb").len(), 1);
        assert!(all("^b", "g", "a\nb").is_empty());
        assert_eq!(all("a.b", "s", "a\nb").len(), 1);
        assert_eq!(all("(a)\\1", "", "xaa")[0][0].as_deref(), Some("aa"));
        assert_eq!(all("A", "gi", "aA").len(), 2);
    }

    #[test]
    fn runaway_patterns_give_up() {
        let text = "a".repeat(20);
        assert!(all("(a*)*b", "", &text).is_empty());
        // Quantified characters loop; groups deeper than the matcher nests give no match
        // rather than a stack overflow.
        let long = "x".repeat(100_000);
        assert_eq!(all("^[\\s\\S]*$", "", &long).len(), 1);
        let long = "x".repeat(3_000);
        assert!(all("^(?:x)*$", "", &long).is_empty());
    }
}
