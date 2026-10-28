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

struct Exec<'r, 'a> {
    r: &'r Regex<'a>,
    input: Vec<u32>,
    icase: bool,
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
        a == b || (self.icase && fold(a) == fold(b))
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
                        let b = before.is_some_and(is_word) != cur.is_some_and(is_word);
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

/// `text.matchAll(new RegExp(pattern, flags))`: every match's groups (index 0 is the whole
/// match; `None` for groups that took no part). `None` when the pattern does not parse.
/// Without the `g` flag only the first match is given.
pub fn match_all(pattern: &str, flags: &str, text: &str) -> Option<Vec<Vec<Option<String>>>> {
    let r = parse(pattern, Flags::new(flags)).ok()?;
    let unicode = r.flags.unicode || r.flags.sets;
    let input: Vec<u32> = if unicode {
        text.chars().map(|c| c as u32).collect()
    } else {
        text.encode_utf16().map(u32::from).collect()
    };
    let to_string = |s: &[u32]| -> String {
        if unicode {
            s.iter().filter_map(|&c| char::from_u32(c)).collect()
        } else {
            let units: Vec<u16> = s.iter().map(|&c| c as u16).collect();
            String::from_utf16_lossy(&units)
        }
    };
    let ex = Exec {
        r: &r,
        input,
        icase: flags.contains('i'),
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
        out.push(
            caps.iter()
                .map(|c| c.map(|(a, b)| to_string(&ex.input[a..b.max(a)])))
                .collect(),
        );
        if !global {
            break;
        }
        last = if end == start { end + 1 } else { end };
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(p: &str, f: &str, t: &str) -> Vec<Vec<Option<String>>> {
        match_all(p, f, t).unwrap()
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
