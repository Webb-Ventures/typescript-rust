//! A small ECMAScript regular expression parser (ECMA-262 2025 syntax, with the Annex B
//! extensions in non-unicode mode). Written from the specification; it builds a tree with
//! source positions for the regex rules and reports the first syntax error.
//!
//! Positions are indexes into [`Regex::units`]: UTF-16 code units without the `u`/`v` flag
//! (so an astral character is two units, as the regex engine sees it) and code points with
//! it. [`Regex::byte_range`] maps them back to byte offsets in the pattern text.

use ts_r_lexer::{is_id_part, is_id_start};

/// The flags that change how a pattern is parsed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Flags {
    /// `u`
    pub unicode: bool,
    /// `v`
    pub sets: bool,
}

impl Flags {
    /// Reads the parse-relevant flags from a flags string.
    pub fn new(flags: &str) -> Flags {
        Flags {
            unicode: flags.contains('u'),
            sets: flags.contains('v'),
        }
    }
}

/// A syntax error, with the message wording of the JavaScript engines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegexError {
    pub message: &'static str,
    /// Unit index where the error was found.
    pub pos: u32,
}

pub type RId = u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssertKind {
    Start,
    End,
    WordBoundary,
    NotWordBoundary,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetKind {
    Digit,
    Space,
    Word,
    Property,
    /// A property of strings such as `\p{RGI_Emoji}` (`v` flag only).
    StringProperty,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RKind {
    /// The whole pattern; children are alternatives.
    Pattern,
    /// Children are terms.
    Alternative,
    /// `(?:...)` or a modifiers group; children are alternatives.
    Group,
    /// Children are alternatives. `index` is 1-based.
    Capture {
        index: u32,
        name: Option<String>,
    },
    Lookaround {
        behind: bool,
        negate: bool,
    },
    /// The single child is the quantified element.
    Quantifier {
        min: u32,
        max: Option<u32>,
        greedy: bool,
    },
    Char {
        value: u32,
    },
    Dot,
    Assertion(AssertKind),
    CharSet {
        kind: SetKind,
        negate: bool,
    },
    /// Children are class elements (chars, ranges, sets, nested classes, `\q{}`, set ops).
    Class {
        negate: bool,
    },
    /// Children are `[min, max]` chars.
    Range,
    /// `\q{a|bc}`; children are `ClassString`s.
    ClassStrings,
    /// Children are chars.
    ClassString,
    /// `a&&b` / `a--b` in a `v` class; children are the two operands.
    SetOp {
        intersection: bool,
    },
    Backref {
        num: Option<u32>,
        name: Option<String>,
    },
}

#[derive(Clone, Debug)]
pub struct RNode {
    pub kind: RKind,
    pub start: u32,
    pub end: u32,
    pub parent: Option<RId>,
    pub children: Vec<RId>,
}

/// A parsed pattern.
#[derive(Debug)]
pub struct Regex<'a> {
    pub src: &'a str,
    pub flags: Flags,
    pub units: Vec<u32>,
    /// Byte offset of each unit, plus the pattern length at the end. The high half of an
    /// astral character split into surrogates gets an empty byte range.
    offs: Vec<u32>,
    pub nodes: Vec<RNode>,
    pub root: RId,
    /// Capture groups in order (`captures[0]` is group 1).
    pub captures: Vec<RId>,
}

impl<'a> Regex<'a> {
    pub fn node(&self, id: RId) -> &RNode {
        &self.nodes[id as usize]
    }

    pub fn kind(&self, id: RId) -> &RKind {
        &self.nodes[id as usize].kind
    }

    /// Byte range of a unit range in the pattern text.
    pub fn unit_bytes(&self, start: u32, end: u32) -> (u32, u32) {
        let s = self.offs[start as usize];
        let e = self.offs[end as usize];
        (s, e.max(s))
    }

    /// For a node that is one half of an astral character split into surrogates (no `u`
    /// flag): `Some(false)` for the high half, `Some(true)` for the low half.
    pub fn split_half(&self, id: RId) -> Option<bool> {
        let n = self.node(id);
        let (s, e) = (n.start as usize, n.end as usize);
        if e != s + 1 {
            return None;
        }
        if self.offs[s] == self.offs[e] {
            Some(false)
        } else if s > 0 && self.offs[s] == self.offs[s - 1] {
            Some(true)
        } else {
            None
        }
    }

    /// Byte range of a node in the pattern text.
    pub fn byte_range(&self, id: RId) -> (u32, u32) {
        let n = self.node(id);
        let s = if n.start > 0 && self.offs[n.start as usize] == self.offs[n.start as usize - 1] {
            // The low half of a split astral character starts where its high half does.
            self.offs[n.start as usize - 1]
        } else {
            self.offs[n.start as usize]
        };
        (s, self.offs[n.end as usize])
    }

    /// The source text of a node.
    pub fn text(&self, id: RId) -> &'a str {
        let (s, e) = self.byte_range(id);
        &self.src[s as usize..e as usize]
    }

    pub fn char_value(&self, id: RId) -> Option<u32> {
        match self.kind(id) {
            RKind::Char { value } => Some(*value),
            _ => None,
        }
    }

    /// The capture groups a backreference refers to.
    pub fn backref_targets(&self, id: RId) -> Vec<RId> {
        match self.kind(id) {
            RKind::Backref { num: Some(n), .. } => self
                .captures
                .get(*n as usize - 1)
                .copied()
                .into_iter()
                .collect(),
            RKind::Backref {
                name: Some(name), ..
            } => self
                .captures
                .iter()
                .copied()
                .filter(
                    |&c| matches!(self.kind(c), RKind::Capture { name: Some(n), .. } if n == name),
                )
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Ancestors of a node, nearest first (excluding the node).
    pub fn ancestors(&self, id: RId) -> impl Iterator<Item = RId> + '_ {
        std::iter::successors(self.node(id).parent, move |&p| self.node(p).parent)
    }
}

/// Parses `src` with `flags`.
pub fn parse(src: &str, flags: Flags) -> Result<Regex<'_>, RegexError> {
    let umode = flags.unicode || flags.sets;
    let mut units = Vec::with_capacity(src.len());
    let mut offs = Vec::with_capacity(src.len() + 1);
    for (i, c) in src.char_indices() {
        let cp = c as u32;
        if !umode && cp > 0xFFFF {
            let v = cp - 0x10000;
            units.push(0xD800 + (v >> 10));
            offs.push(i as u32);
            units.push(0xDC00 + (v & 0x3FF));
            offs.push(i as u32);
        } else {
            units.push(cp);
            offs.push(i as u32);
        }
    }
    offs.push(src.len() as u32);
    let mut p = Parser {
        r: Regex {
            src,
            flags,
            units,
            offs,
            nodes: Vec::new(),
            root: 0,
            captures: Vec::new(),
        },
        pos: 0,
        u: umode,
        v: flags.sets,
        named: umode,
        group_total: 0,
        next_disj: 0,
        path: Vec::new(),
        names: Vec::new(),
        named_refs: Vec::new(),
        depth: 0,
    };
    p.prescan();
    let alts = p.disjunction()?;
    if p.pos < p.r.units.len() {
        return p.err(if p.cur() == Some(')' as u32) {
            "Unmatched ')'"
        } else {
            "Unexpected character"
        });
    }
    let end = p.pos as u32;
    p.r.root = p.add(RKind::Pattern, 0, end, alts);
    // Named references must resolve.
    for &(id, pos) in &p.named_refs {
        let RKind::Backref {
            name: Some(name), ..
        } = p.r.kind(id)
        else {
            continue;
        };
        if !p.names.iter().any(|(n, _, _)| n == name) {
            return Err(RegexError {
                message: "Invalid named capture referenced",
                pos,
            });
        }
    }
    // Duplicate names must be in different alternatives.
    for i in 0..p.names.len() {
        for j in 0..i {
            let (a, _, pa) = &p.names[i];
            let (b, _, pb) = &p.names[j];
            if a == b && !disjoint(pa, pb) {
                let pos = p.r.node(p.names[i].1).start;
                return Err(RegexError {
                    message: "Duplicate capture group name",
                    pos,
                });
            }
        }
    }
    Ok(p.r)
}

/// Are two alternative paths in different alternatives of some disjunction?
fn disjoint(a: &[(u32, u32)], b: &[(u32, u32)]) -> bool {
    for (x, y) in a.iter().zip(b) {
        if x.0 != y.0 {
            return false;
        }
        if x.1 != y.1 {
            return true;
        }
    }
    false
}

/// Formats an error the way `new RegExp` reports it (without the final period).
pub fn error_message(pattern: &str, flags: &str, err: &RegexError) -> String {
    format!(
        "Invalid regular expression: /{pattern}/{flags}: {}",
        err.message
    )
}

/// A group name, its capture node and the alternative path it was declared on.
type NameEntry = (String, RId, Vec<(u32, u32)>);

struct Parser<'a> {
    r: Regex<'a>,
    pos: usize,
    u: bool,
    v: bool,
    /// `\k` is a named reference (unicode mode, or the pattern has a named group).
    named: bool,
    group_total: u32,
    next_disj: u32,
    /// (disjunction id, alternative index) of the alternatives enclosing the current term.
    path: Vec<(u32, u32)>,
    names: Vec<NameEntry>,
    named_refs: Vec<(RId, u32)>,
    /// Groups, lookarounds and `v`-mode classes currently open.
    depth: u32,
}

/// Nesting limit for groups and `v`-mode classes; the parser recurses once per level.
const MAX_DEPTH: u32 = 256;

const fn ch(c: char) -> u32 {
    c as u32
}

fn is_syntax_char(c: u32) -> bool {
    matches!(
        char::from_u32(c),
        Some('^' | '$' | '\\' | '.' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|')
    )
}

fn hex_val(c: u32) -> Option<u32> {
    char::from_u32(c)?.to_digit(16)
}

fn is_lead(c: u32) -> bool {
    (0xD800..=0xDBFF).contains(&c)
}

fn is_trail(c: u32) -> bool {
    (0xDC00..=0xDFFF).contains(&c)
}

fn combine(h: u32, l: u32) -> u32 {
    0x10000 + ((h - 0xD800) << 10) + (l - 0xDC00)
}

const STRING_PROPERTIES: &[&str] = &[
    "Basic_Emoji",
    "Emoji_Keycap_Sequence",
    "RGI_Emoji_Modifier_Sequence",
    "RGI_Emoji_Flag_Sequence",
    "RGI_Emoji_Tag_Sequence",
    "RGI_Emoji_ZWJ_Sequence",
    "RGI_Emoji",
];

impl<'a> Parser<'a> {
    fn cur(&self) -> Option<u32> {
        self.r.units.get(self.pos).copied()
    }

    fn peek(&self, k: usize) -> Option<u32> {
        self.r.units.get(self.pos + k).copied()
    }

    fn at(&self, c: char) -> bool {
        self.cur() == Some(ch(c))
    }

    fn eat(&mut self, c: char) -> bool {
        if self.at(c) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn err<T>(&self, message: &'static str) -> Result<T, RegexError> {
        Err(RegexError {
            message,
            pos: self.pos as u32,
        })
    }

    /// Runs `f` one nesting level deeper, failing past [`MAX_DEPTH`] instead of overflowing
    /// the stack.
    fn nested<T>(&mut self, f: fn(&mut Self) -> Result<T, RegexError>) -> Result<T, RegexError> {
        if self.depth == MAX_DEPTH {
            return self.err("Regular expression too deeply nested");
        }
        self.depth += 1;
        let r = f(self);
        self.depth -= 1;
        r
    }

    fn add(&mut self, kind: RKind, start: u32, end: u32, children: Vec<RId>) -> RId {
        let id = self.r.nodes.len() as RId;
        for &c in &children {
            self.r.nodes[c as usize].parent = Some(id);
        }
        self.r.nodes.push(RNode {
            kind,
            start,
            end,
            parent: None,
            children,
        });
        id
    }

    fn leaf(&mut self, kind: RKind, start: usize) -> RId {
        let end = self.pos as u32;
        self.add(kind, start as u32, end, Vec::new())
    }

    /// Counts capture groups and detects named groups.
    fn prescan(&mut self) {
        let u = &self.r.units;
        let mut i = 0;
        let mut class_depth = 0u32;
        while i < u.len() {
            let c = u[i];
            if c == ch('\\') {
                i += 2;
                continue;
            }
            if class_depth > 0 {
                if c == ch(']') {
                    class_depth -= 1;
                } else if c == ch('[') && self.v {
                    class_depth += 1;
                }
            } else if c == ch('[') {
                class_depth = 1;
            } else if c == ch('(') {
                if u.get(i + 1) != Some(&ch('?')) {
                    self.group_total += 1;
                } else if u.get(i + 2) == Some(&ch('<'))
                    && !matches!(u.get(i + 3), Some(&x) if x == ch('=') || x == ch('!'))
                {
                    self.group_total += 1;
                    self.named = true;
                }
            }
            i += 1;
        }
    }

    fn disjunction(&mut self) -> Result<Vec<RId>, RegexError> {
        let did = self.next_disj;
        self.next_disj += 1;
        let mut alts = Vec::new();
        let mut i = 0;
        loop {
            self.path.push((did, i));
            let a = self.alternative();
            self.path.pop();
            alts.push(a?);
            if self.eat('|') {
                i += 1;
                continue;
            }
            return Ok(alts);
        }
    }

    fn alternative(&mut self) -> Result<RId, RegexError> {
        let start = self.pos;
        let mut terms = Vec::new();
        while let Some(c) = self.cur() {
            if c == ch('|') || c == ch(')') {
                break;
            }
            terms.push(self.term()?);
        }
        let end = self.pos as u32;
        Ok(self.add(RKind::Alternative, start as u32, end, terms))
    }

    /// Reads `{n}`, `{n,}` or `{n,m}` at the cursor without consuming it.
    fn braced(&self) -> Option<(u32, Option<u32>, usize)> {
        let u = &self.r.units;
        let mut i = self.pos;
        if u.get(i) != Some(&ch('{')) {
            return None;
        }
        i += 1;
        let num = |i: &mut usize| -> Option<u32> {
            let s = *i;
            let mut v: u64 = 0;
            while let Some(d) = u.get(*i).and_then(|&c| char::from_u32(c)?.to_digit(10)) {
                v = (v * 10 + u64::from(d)).min(u64::from(u32::MAX));
                *i += 1;
            }
            (*i > s).then_some(v as u32)
        };
        let min = num(&mut i)?;
        let max = if u.get(i) == Some(&ch(',')) {
            i += 1;
            if u.get(i) == Some(&ch('}')) {
                None
            } else {
                Some(num(&mut i)?)
            }
        } else {
            Some(min)
        };
        if u.get(i) != Some(&ch('}')) {
            return None;
        }
        Some((min, max, i + 1))
    }

    fn at_quantifier(&self) -> bool {
        matches!(self.cur(), Some(c) if c == ch('*') || c == ch('+') || c == ch('?'))
            || self.braced().is_some()
    }

    fn no_quantifier(&self) -> Result<(), RegexError> {
        if self.at_quantifier() {
            return self.err("Nothing to repeat");
        }
        Ok(())
    }

    fn term(&mut self) -> Result<RId, RegexError> {
        let start = self.pos;
        let c = self.cur().unwrap_or(0);
        if c == ch('^') || c == ch('$') {
            self.pos += 1;
            let k = if c == ch('^') {
                AssertKind::Start
            } else {
                AssertKind::End
            };
            let n = self.leaf(RKind::Assertion(k), start);
            self.no_quantifier()?;
            return Ok(n);
        }
        if c == ch('\\') && matches!(self.peek(1), Some(x) if x == ch('b') || x == ch('B')) {
            let k = if self.peek(1) == Some(ch('b')) {
                AssertKind::WordBoundary
            } else {
                AssertKind::NotWordBoundary
            };
            self.pos += 2;
            let n = self.leaf(RKind::Assertion(k), start);
            self.no_quantifier()?;
            return Ok(n);
        }
        if c == ch('(') && self.peek(1) == Some(ch('?')) {
            let p2 = self.peek(2);
            let p3 = self.peek(3);
            let look = if p2 == Some(ch('=')) || p2 == Some(ch('!')) {
                Some((false, p2 == Some(ch('!')), 3))
            } else if p2 == Some(ch('<')) && (p3 == Some(ch('=')) || p3 == Some(ch('!'))) {
                Some((true, p3 == Some(ch('!')), 4))
            } else {
                None
            };
            if let Some((behind, negate, len)) = look {
                self.pos += len;
                let alts = self.nested(Self::disjunction)?;
                if !self.eat(')') {
                    return self.err("Unterminated group");
                }
                let end = self.pos as u32;
                let n = self.add(
                    RKind::Lookaround { behind, negate },
                    start as u32,
                    end,
                    alts,
                );
                if !behind && !self.u {
                    return self.quantifier(n, start);
                }
                self.no_quantifier()?;
                return Ok(n);
            }
        }
        let atom = self.atom()?;
        self.quantifier(atom, start)
    }

    fn quantifier(&mut self, atom: RId, start: usize) -> Result<RId, RegexError> {
        let (min, max) = match self.cur().and_then(char::from_u32) {
            Some('*') => {
                self.pos += 1;
                (0, None)
            }
            Some('+') => {
                self.pos += 1;
                (1, None)
            }
            Some('?') => {
                self.pos += 1;
                (0, Some(1))
            }
            Some('{') => match self.braced() {
                Some((min, max, end)) => {
                    if max.is_some_and(|m| m < min) {
                        return self.err("numbers out of order in {} quantifier");
                    }
                    self.pos = end;
                    (min, max)
                }
                None => {
                    if self.u {
                        return self.err("Incomplete quantifier");
                    }
                    return Ok(atom);
                }
            },
            _ => return Ok(atom),
        };
        let greedy = !self.eat('?');
        let end = self.pos as u32;
        Ok(self.add(
            RKind::Quantifier { min, max, greedy },
            start as u32,
            end,
            vec![atom],
        ))
    }

    fn atom(&mut self) -> Result<RId, RegexError> {
        let start = self.pos;
        let c = self.cur().unwrap_or(0);
        match char::from_u32(c) {
            Some('.') => {
                self.pos += 1;
                Ok(self.leaf(RKind::Dot, start))
            }
            Some('(') => self.group(),
            Some('[') => {
                if self.v {
                    self.nested(Self::class_v)
                } else {
                    self.class()
                }
            }
            Some('\\') => self.atom_escape(),
            Some('*' | '+' | '?') => self.err("Nothing to repeat"),
            Some('{') => {
                if self.braced().is_some() {
                    self.err("Nothing to repeat")
                } else if self.u {
                    self.err("Lone quantifier brackets")
                } else {
                    self.pos += 1;
                    Ok(self.leaf(RKind::Char { value: c }, start))
                }
            }
            Some('}' | ']') if self.u => self.err("Lone quantifier brackets"),
            _ => {
                self.pos += 1;
                Ok(self.leaf(RKind::Char { value: c }, start))
            }
        }
    }

    fn group(&mut self) -> Result<RId, RegexError> {
        let start = self.pos;
        self.pos += 1;
        let mut kind = RKind::Group;
        let mut name_path = None;
        if self.eat('?') {
            if self.eat(':') {
            } else if self.eat('<') {
                let name = self.group_name("Invalid capture group name")?;
                kind = RKind::Capture {
                    index: 0,
                    name: Some(name),
                };
                name_path = Some(self.path.clone());
            } else {
                self.modifiers()?;
            }
        } else {
            kind = RKind::Capture {
                index: 0,
                name: None,
            };
        }
        // Number the capture before its nested groups.
        let index = if let RKind::Capture { index, .. } = &mut kind {
            *index = self.r.captures.len() as u32 + 1;
            self.r.captures.push(RId::MAX);
            Some(*index)
        } else {
            None
        };
        let alts = self.nested(Self::disjunction)?;
        if !self.eat(')') {
            return self.err("Unterminated group");
        }
        let end = self.pos as u32;
        let id = self.add(kind.clone(), start as u32, end, alts);
        if let Some(i) = index {
            self.r.captures[i as usize - 1] = id;
        }
        if let (RKind::Capture { name: Some(n), .. }, Some(path)) = (kind, name_path) {
            self.names.push((n, id, path));
        }
        Ok(id)
    }

    /// `(?ims-ims:` after the `?`.
    fn modifiers(&mut self) -> Result<(), RegexError> {
        let mut seen: Vec<u32> = Vec::new();
        let mut dash = false;
        let mut any = false;
        loop {
            let Some(c) = self.cur() else {
                return self.err("Invalid group");
            };
            match char::from_u32(c) {
                Some('i' | 'm' | 's') => {
                    if seen.contains(&c) {
                        return self.err(match char::from_u32(c) {
                            Some('i') => "Duplicated flag 'i'",
                            Some('m') => "Duplicated flag 'm'",
                            _ => "Duplicated flag 's'",
                        });
                    }
                    seen.push(c);
                    any = true;
                }
                Some('-') if !dash => dash = true,
                Some(':') => {
                    if dash && !any {
                        return self.err("Invalid empty flags");
                    }
                    if !dash && !any {
                        return self.err("Invalid group");
                    }
                    self.pos += 1;
                    return Ok(());
                }
                _ => return self.err("Invalid group"),
            }
            self.pos += 1;
        }
    }

    /// A `RegExpIdentifierName` followed by `>` (after the `<`).
    fn group_name(&mut self, msg: &'static str) -> Result<String, RegexError> {
        let mut s = String::new();
        loop {
            let Some(c) = self.cur() else {
                return self.err(msg);
            };
            if c == ch('>') {
                self.pos += 1;
                break;
            }
            let cp = if c == ch('\\') {
                self.pos += 1;
                if !self.eat('u') {
                    return self.err(msg);
                }
                match self.unicode_escape(true) {
                    Some(v) => v,
                    None => return self.err(msg),
                }
            } else {
                self.pos += 1;
                if is_lead(c) && self.cur().is_some_and(is_trail) {
                    let l = self.cur().unwrap_or(0);
                    self.pos += 1;
                    combine(c, l)
                } else {
                    c
                }
            };
            let Some(chr) = char::from_u32(cp) else {
                return self.err(msg);
            };
            let ok = if s.is_empty() {
                is_id_start(chr) || chr == '$' || chr == '_'
            } else {
                is_id_part(chr) || chr == '$'
            };
            if !ok {
                return self.err(msg);
            }
            s.push(chr);
        }
        if s.is_empty() {
            return self.err(msg);
        }
        Ok(s)
    }

    /// `\u` escape body with the cursor after the `u`. `braces` allows `\u{...}` and joins
    /// surrogate-pair escapes (unicode mode and group names).
    fn unicode_escape(&mut self, braces: bool) -> Option<u32> {
        let u = &self.r.units;
        let hex4 = |i: usize| -> Option<u32> {
            let mut v = 0;
            for k in 0..4 {
                v = v * 16 + hex_val(*u.get(i + k)?)?;
            }
            Some(v)
        };
        if braces && self.at('{') {
            let mut i = self.pos + 1;
            let mut v: u32 = 0;
            let s = i;
            while let Some(d) = u.get(i).and_then(|&c| hex_val(c)) {
                v = v.saturating_mul(16).saturating_add(d);
                i += 1;
            }
            if i == s || u.get(i) != Some(&ch('}')) || v > 0x10FFFF {
                return None;
            }
            self.pos = i + 1;
            return Some(v);
        }
        let v = hex4(self.pos)?;
        self.pos += 4;
        if braces
            && is_lead(v)
            && u.get(self.pos) == Some(&ch('\\'))
            && u.get(self.pos + 1) == Some(&ch('u'))
            && let Some(l) = hex4(self.pos + 2)
            && is_trail(l)
        {
            self.pos += 6;
            return Some(combine(v, l));
        }
        Some(v)
    }

    /// Legacy octal escape (non-unicode mode) with the cursor on the first digit.
    fn legacy_octal(&mut self) -> u32 {
        let u = &self.r.units;
        let oct = |i: usize| u.get(i).and_then(|&c| char::from_u32(c)?.to_digit(8));
        let Some(d1) = oct(self.pos) else {
            // `\8` / `\9`: identity escapes.
            let c = self.cur().unwrap_or(0);
            self.pos += 1;
            return c;
        };
        self.pos += 1;
        let mut v = d1;
        if let Some(d2) = oct(self.pos) {
            v = v * 8 + d2;
            self.pos += 1;
            if d1 <= 3
                && let Some(d3) = oct(self.pos)
            {
                v = v * 8 + d3;
                self.pos += 1;
            }
        }
        v
    }

    fn property(&mut self, start: usize) -> Result<RId, RegexError> {
        let negate = self.at('P');
        self.pos += 1;
        if !self.eat('{') {
            return self.err("Invalid property name");
        }
        let s = self.pos;
        while let Some(c) = self.cur() {
            if c == ch('}') {
                break;
            }
            if !(char::from_u32(c)
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_' || c == '='))
            {
                return self.err("Invalid property name");
            }
            self.pos += 1;
        }
        if !self.at('}') || self.pos == s {
            return self.err("Invalid property name");
        }
        let text: String = self.r.units[s..self.pos]
            .iter()
            .filter_map(|&c| char::from_u32(c))
            .collect();
        self.pos += 1;
        let mut kind = SetKind::Property;
        if let Some((name, value)) = text.split_once('=') {
            if !matches!(
                name,
                "General_Category" | "gc" | "Script" | "sc" | "Script_Extensions" | "scx"
            ) || value.is_empty()
                || value.contains('=')
            {
                return self.err("Invalid property name");
            }
        } else if STRING_PROPERTIES.contains(&text.as_str()) {
            if !self.v || negate {
                return self.err("Invalid property name");
            }
            kind = SetKind::StringProperty;
        }
        Ok(self.leaf(RKind::CharSet { kind, negate }, start))
    }

    /// `\d` and friends with the cursor on the letter; `None` if it isn't one.
    fn class_escape_set(&mut self, start: usize) -> Option<Result<RId, RegexError>> {
        let c = char::from_u32(self.cur()?)?;
        let kind = match c.to_ascii_lowercase() {
            'd' => SetKind::Digit,
            's' => SetKind::Space,
            'w' => SetKind::Word,
            'p' if self.u => return Some(self.property(start)),
            _ => return None,
        };
        self.pos += 1;
        Some(Ok(self.leaf(
            RKind::CharSet {
                kind,
                negate: c.is_ascii_uppercase(),
            },
            start,
        )))
    }

    fn atom_escape(&mut self) -> Result<RId, RegexError> {
        let start = self.pos;
        self.pos += 1;
        let Some(c) = self.cur() else {
            return self.err("\\ at end of pattern");
        };
        if (ch('1')..=ch('9')).contains(&c) {
            let save = self.pos;
            let mut n: u64 = 0;
            while let Some(d) = self.cur().and_then(|c| char::from_u32(c)?.to_digit(10)) {
                n = (n * 10 + u64::from(d)).min(u64::from(u32::MAX));
                self.pos += 1;
            }
            if n <= u64::from(self.group_total) {
                return Ok(self.leaf(
                    RKind::Backref {
                        num: Some(n as u32),
                        name: None,
                    },
                    start,
                ));
            }
            if self.u {
                self.pos = save;
                return self.err("Invalid escape");
            }
            self.pos = save;
            let value = self.legacy_octal();
            return Ok(self.leaf(RKind::Char { value }, start));
        }
        if c == ch('0') {
            if self
                .peek(1)
                .is_some_and(|d| (ch('0')..=ch('9')).contains(&d))
            {
                if self.u {
                    return self.err("Invalid decimal escape");
                }
                let value = self.legacy_octal();
                return Ok(self.leaf(RKind::Char { value }, start));
            }
            self.pos += 1;
            return Ok(self.leaf(RKind::Char { value: 0 }, start));
        }
        if c == ch('k') {
            self.pos += 1;
            if self.named {
                if !self.eat('<') {
                    return self.err("Invalid named reference");
                }
                let name = self.group_name("Invalid named reference")?;
                let id = self.leaf(
                    RKind::Backref {
                        num: None,
                        name: Some(name),
                    },
                    start,
                );
                self.named_refs.push((id, start as u32));
                return Ok(id);
            }
            return Ok(self.leaf(RKind::Char { value: c }, start));
        }
        if let Some(r) = self.class_escape_set(start) {
            return r;
        }
        let value = self.char_escape(false)?;
        Ok(self.leaf(RKind::Char { value }, start))
    }

    /// A CharacterEscape with the cursor after the backslash.
    fn char_escape(&mut self, in_class: bool) -> Result<u32, RegexError> {
        let Some(c) = self.cur() else {
            return self.err("\\ at end of pattern");
        };
        let simple = match char::from_u32(c) {
            Some('f') => Some(0x0C),
            Some('n') => Some(0x0A),
            Some('r') => Some(0x0D),
            Some('t') => Some(0x09),
            Some('v') => Some(0x0B),
            _ => None,
        };
        if let Some(v) = simple {
            self.pos += 1;
            return Ok(v);
        }
        match char::from_u32(c) {
            Some('c') => {
                let next = self.peek(1).and_then(char::from_u32);
                if let Some(l) = next.filter(char::is_ascii_alphabetic) {
                    self.pos += 2;
                    return Ok(l as u32 % 32);
                }
                if in_class
                    && !self.u
                    && let Some(l) = next.filter(|l| l.is_ascii_digit() || *l == '_')
                {
                    self.pos += 2;
                    return Ok(l as u32 % 32);
                }
                if self.u {
                    return self.err("Invalid unicode escape");
                }
                // `\c` is a literal backslash; the `c` is read next.
                Ok(ch('\\'))
            }
            Some('x') => {
                if let (Some(a), Some(b)) = (
                    self.peek(1).and_then(hex_val),
                    self.peek(2).and_then(hex_val),
                ) {
                    self.pos += 3;
                    return Ok(a * 16 + b);
                }
                if self.u {
                    return self.err("Invalid escape");
                }
                self.pos += 1;
                Ok(c)
            }
            Some('u') => {
                self.pos += 1;
                let save = self.pos;
                if let Some(v) = self.unicode_escape(self.u) {
                    return Ok(v);
                }
                self.pos = save;
                if self.u {
                    return self.err("Invalid Unicode escape");
                }
                Ok(c)
            }
            _ => {
                if self.u && !(is_syntax_char(c) || c == ch('/') || (in_class && c == ch('-'))) {
                    return self.err("Invalid escape");
                }
                self.pos += 1;
                Ok(c)
            }
        }
    }

    fn class(&mut self) -> Result<RId, RegexError> {
        let start = self.pos;
        self.pos += 1;
        let negate = self.eat('^');
        let mut elems = Vec::new();
        loop {
            let Some(c) = self.cur() else {
                return self.err("Unterminated character class");
            };
            if c == ch(']') {
                self.pos += 1;
                break;
            }
            let a = self.class_atom()?;
            if self.at('-') && self.peek(1).is_some_and(|n| n != ch(']')) {
                let dash_start = self.pos;
                self.pos += 1;
                let b = self.class_atom()?;
                match (self.r.char_value(a), self.r.char_value(b)) {
                    (Some(x), Some(y)) => {
                        if x > y {
                            return self.err("Range out of order in character class");
                        }
                        let (s, e) = (self.r.node(a).start, self.r.node(b).end);
                        elems.push(self.add(RKind::Range, s, e, vec![a, b]));
                    }
                    _ => {
                        if self.u {
                            return self.err("Invalid character class");
                        }
                        elems.push(a);
                        let save = self.pos;
                        self.pos = dash_start + 1;
                        let d = self.leaf(RKind::Char { value: ch('-') }, dash_start);
                        self.pos = save;
                        elems.push(d);
                        elems.push(b);
                    }
                }
            } else {
                elems.push(a);
            }
        }
        let end = self.pos as u32;
        Ok(self.add(RKind::Class { negate }, start as u32, end, elems))
    }

    fn class_atom(&mut self) -> Result<RId, RegexError> {
        let start = self.pos;
        let c = self.cur().unwrap_or(0);
        if c != ch('\\') {
            self.pos += 1;
            return Ok(self.leaf(RKind::Char { value: c }, start));
        }
        self.pos += 1;
        let Some(c2) = self.cur() else {
            return self.err("\\ at end of pattern");
        };
        if c2 == ch('b') {
            self.pos += 1;
            return Ok(self.leaf(RKind::Char { value: 8 }, start));
        }
        if self.u && c2 == ch('-') {
            self.pos += 1;
            return Ok(self.leaf(RKind::Char { value: c2 }, start));
        }
        if let Some(r) = self.class_escape_set(start) {
            return r;
        }
        if (ch('0')..=ch('9')).contains(&c2) {
            if self.u {
                if c2 == ch('0')
                    && !self
                        .peek(1)
                        .is_some_and(|d| (ch('0')..=ch('9')).contains(&d))
                {
                    self.pos += 1;
                    return Ok(self.leaf(RKind::Char { value: 0 }, start));
                }
                return self.err("Invalid class escape");
            }
            let value = self.legacy_octal();
            return Ok(self.leaf(RKind::Char { value }, start));
        }
        if c2 == ch('k') && !self.u {
            self.pos += 1;
            return Ok(self.leaf(RKind::Char { value: c2 }, start));
        }
        let value = self.char_escape(true)?;
        Ok(self.leaf(RKind::Char { value }, start))
    }

    fn starts_with2(&self, a: char) -> bool {
        self.at(a) && self.peek(1) == Some(ch(a))
    }

    /// A `v`-mode class (`ClassSetExpression`).
    fn class_v(&mut self) -> Result<RId, RegexError> {
        let start = self.pos;
        self.pos += 1;
        let negate = self.eat('^');
        let mut elems = Vec::new();
        if self.eat(']') {
            let end = self.pos as u32;
            return Ok(self.add(RKind::Class { negate }, start as u32, end, elems));
        }
        let (first, first_range) = self.v_operand()?;
        if self.starts_with2('&') || self.starts_with2('-') {
            if first_range {
                return self.err("Invalid set operation in character class");
            }
            let intersection = self.at('&');
            let op = if intersection { '&' } else { '-' };
            let mut acc = first;
            loop {
                self.pos += 2;
                let (rhs, range) = self.v_operand()?;
                if range {
                    return self.err("Invalid set operation in character class");
                }
                let (s, e) = (self.r.node(acc).start, self.r.node(rhs).end);
                acc = self.add(RKind::SetOp { intersection }, s, e, vec![acc, rhs]);
                if self.eat(']') {
                    break;
                }
                if self.cur().is_none() {
                    return self.err("Unterminated character class");
                }
                if !self.starts_with2(op) {
                    return self.err("Invalid set operation in character class");
                }
            }
            elems.push(acc);
        } else {
            elems.push(first);
            loop {
                if self.cur().is_none() {
                    return self.err("Unterminated character class");
                }
                if self.eat(']') {
                    break;
                }
                if self.starts_with2('&') || self.starts_with2('-') {
                    return self.err("Invalid set operation in character class");
                }
                elems.push(self.v_operand()?.0);
            }
        }
        let end = self.pos as u32;
        Ok(self.add(RKind::Class { negate }, start as u32, end, elems))
    }

    /// An operand of a `v` class, possibly a range (`true` in the result).
    fn v_operand(&mut self) -> Result<(RId, bool), RegexError> {
        let a = self.v_atom()?;
        if self.at('-')
            && self.peek(1) != Some(ch('-'))
            && let Some(x) = self.r.char_value(a)
        {
            self.pos += 1;
            let b = self.v_atom()?;
            let Some(y) = self.r.char_value(b) else {
                return self.err("Invalid character class");
            };
            if x > y {
                return self.err("Range out of order in character class");
            }
            let (s, e) = (self.r.node(a).start, self.r.node(b).end);
            return Ok((self.add(RKind::Range, s, e, vec![a, b]), true));
        }
        Ok((a, false))
    }

    fn v_atom(&mut self) -> Result<RId, RegexError> {
        let start = self.pos;
        let Some(c) = self.cur() else {
            return self.err("Unterminated character class");
        };
        if c == ch('[') {
            return self.nested(Self::class_v);
        }
        if c == ch('\\') {
            self.pos += 1;
            let Some(c2) = self.cur() else {
                return self.err("\\ at end of pattern");
            };
            if c2 == ch('q') && self.peek(1) == Some(ch('{')) {
                self.pos += 2;
                return self.class_strings(start);
            }
            if let Some(r) = self.class_escape_set(start) {
                return r;
            }
            if c2 == ch('b') {
                self.pos += 1;
                return Ok(self.leaf(RKind::Char { value: 8 }, start));
            }
            if char::from_u32(c2).is_some_and(|x| "&-!#%,:;<=>@`~".contains(x)) {
                self.pos += 1;
                return Ok(self.leaf(RKind::Char { value: c2 }, start));
            }
            if c2 == ch('0')
                && !self
                    .peek(1)
                    .is_some_and(|d| (ch('0')..=ch('9')).contains(&d))
            {
                self.pos += 1;
                return Ok(self.leaf(RKind::Char { value: 0 }, start));
            }
            if (ch('0')..=ch('9')).contains(&c2) {
                return self.err("Invalid class escape");
            }
            let value = self.char_escape(true)?;
            return Ok(self.leaf(RKind::Char { value }, start));
        }
        if char::from_u32(c).is_some_and(|x| "()]{}/-|".contains(x)) {
            return self.err("Invalid character in character class");
        }
        if char::from_u32(c).is_some_and(|x| "&!#$%*+,.:;<=>?@^`~".contains(x))
            && self.peek(1) == Some(c)
        {
            return self.err("Invalid set operation in character class");
        }
        self.pos += 1;
        Ok(self.leaf(RKind::Char { value: c }, start))
    }

    /// `\q{...}` with the cursor after the `{`.
    fn class_strings(&mut self, start: usize) -> Result<RId, RegexError> {
        let mut strings = Vec::new();
        loop {
            let s = self.pos;
            let mut chars = Vec::new();
            loop {
                let Some(c) = self.cur() else {
                    return self.err("Unterminated character class");
                };
                if c == ch('|') || c == ch('}') {
                    break;
                }
                let cs = self.pos;
                if c == ch('\\') {
                    self.pos += 1;
                    let value = if self.eat('b') {
                        8
                    } else {
                        self.char_escape(true)?
                    };
                    chars.push(self.leaf(RKind::Char { value }, cs));
                } else {
                    self.pos += 1;
                    chars.push(self.leaf(RKind::Char { value: c }, cs));
                }
            }
            let e = self.pos as u32;
            strings.push(self.add(RKind::ClassString, s as u32, e, chars));
            if self.eat('}') {
                break;
            }
            self.pos += 1;
        }
        let end = self.pos as u32;
        Ok(self.add(RKind::ClassStrings, start as u32, end, strings))
    }
}

/// Compiles a pattern for matching (`u` mode first, then without it).
pub fn compile(pattern: &str) -> Option<Regex<'_>> {
    parse(pattern, Flags::new("u"))
        .or_else(|_| parse(pattern, Flags::default()))
        .ok()
}

impl Regex<'_> {
    /// Whether the pattern matches somewhere in `text` (a backtracking matcher meant for
    /// the small patterns in rule options; backreferences match the empty string and
    /// lookbehinds always succeed).
    pub fn is_match(&self, text: &str, ignore_case: bool) -> bool {
        self.is_match_flags(text, if ignore_case { "i" } else { "" })
    }

    /// [`Regex::is_match`] with the matching flags of `flags`: `i`, `s` (`.` matches line
    /// terminators) and `m` (`^` and `$` match at line terminators).
    pub fn is_match_flags(&self, text: &str, flags: &str) -> bool {
        let umode = self.flags.unicode || self.flags.sets;
        let input: Vec<u32> = if umode {
            text.chars().map(|c| c as u32).collect()
        } else {
            text.encode_utf16().map(u32::from).collect()
        };
        let m = Matcher {
            r: self,
            input,
            icase: flags.contains('i'),
            dot_all: flags.contains('s'),
            multiline: flags.contains('m'),
            steps: std::cell::Cell::new(0),
        };
        (0..=m.input.len()).any(|start| m.node(self.root, start, &mut |_| true))
    }
}

struct Matcher<'r, 'a> {
    r: &'r Regex<'a>,
    input: Vec<u32>,
    icase: bool,
    dot_all: bool,
    multiline: bool,
    steps: std::cell::Cell<u32>,
}

fn fold(c: u32) -> u32 {
    char::from_u32(c)
        .and_then(|ch| ch.to_lowercase().next())
        .map_or(c, |l| l as u32)
}

fn is_word(c: u32) -> bool {
    char::from_u32(c).is_some_and(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

/// The emoji properties of strings, approximated by one code point: the pictograph and
/// symbol blocks, the regional indicators and the code points that end a sequence
/// (variation selector 16, the keycap mark, a skin tone, a tag terminator). A sequence
/// therefore matches at its last code point, which is what a pattern ending in
/// `[...\p{RGI_Emoji}]` needs.
fn is_emoji_like(c: u32) -> bool {
    matches!(
        c,
        0x203C | 0x2049 | 0x2122 | 0x2139 | 0x2194..=0x21AA | 0x231A..=0x23FF | 0x24C2
            | 0x25AA..=0x25FE | 0x2600..=0x27BF | 0x2934 | 0x2935 | 0x2B05..=0x2B55
            | 0x3030 | 0x303D | 0x3297 | 0x3299 | 0xFE0F | 0x20E3 | 0xE007F
            | 0x1F000..=0x1FAFF
    )
}

fn is_line_terminator(c: u32) -> bool {
    matches!(c, 0x0A | 0x0D | 0x2028 | 0x2029)
}

impl Matcher<'_, '_> {
    /// Gives up on runaway backtracking.
    fn tick(&self) -> bool {
        let n = self.steps.get() + 1;
        self.steps.set(n);
        n > 200_000
    }

    fn char_eq(&self, a: u32, b: u32) -> bool {
        a == b || (self.icase && fold(a) == fold(b))
    }

    fn set_matches(&self, kind: SetKind, negate: bool, c: u32) -> bool {
        let ch = char::from_u32(c);
        let m = match kind {
            SetKind::Digit => ch.is_some_and(|x| x.is_ascii_digit()),
            SetKind::Space => ch.is_some_and(|x| x.is_whitespace() || x == '\u{FEFF}'),
            SetKind::Word => is_word(c),
            SetKind::Property => ch.is_some_and(char::is_alphabetic),
            SetKind::StringProperty => is_emoji_like(c),
        };
        m != negate
    }

    fn class_item(&self, id: RId, c: u32) -> bool {
        let r = self.r;
        match r.kind(id) {
            RKind::Char { value } => self.char_eq(*value, c),
            RKind::Range => {
                let ch = &r.node(id).children;
                let lo = r.char_value(ch[0]).unwrap_or(0);
                let hi = r.char_value(ch[1]).unwrap_or(0);
                (lo..=hi).contains(&c) || (self.icase && (lo..=hi).contains(&fold(c)))
            }
            RKind::CharSet { kind, negate } => self.set_matches(*kind, *negate, c),
            RKind::Class { negate } => {
                r.node(id).children.iter().any(|&k| self.class_item(k, c)) != *negate
            }
            RKind::ClassStrings => r.node(id).children.iter().any(|&s| {
                let chars = &r.node(s).children;
                chars.len() == 1 && self.class_item(chars[0], c)
            }),
            RKind::SetOp { intersection } => {
                let ch = &r.node(id).children;
                let (a, b) = (self.class_item(ch[0], c), self.class_item(ch[1], c));
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
        if self.tick() {
            return false;
        }
        if count < min {
            return self.node(child, pos, &mut |p| {
                self.rep(child, min, max, greedy, count + 1, p, k)
            });
        }
        let more = max.is_none_or(|m| count < m);
        if greedy {
            if more
                && self.node(child, pos, &mut |p| {
                    p != pos && self.rep(child, min, max, greedy, count + 1, p, k)
                })
            {
                return true;
            }
            k(pos)
        } else {
            if k(pos) {
                return true;
            }
            more && self.node(child, pos, &mut |p| {
                p != pos && self.rep(child, min, max, greedy, count + 1, p, k)
            })
        }
    }

    fn node(&self, id: RId, pos: usize, k: &mut dyn FnMut(usize) -> bool) -> bool {
        if self.tick() {
            return false;
        }
        let r = self.r;
        let cur = self.input.get(pos).copied();
        match r.kind(id) {
            RKind::Pattern | RKind::Group | RKind::Capture { .. } => self.alts(id, pos, k),
            RKind::Alternative => self.seq(&r.node(id).children, pos, k),
            RKind::Lookaround { behind, negate } => {
                if *behind {
                    return k(pos);
                }
                let found = self.alts(id, pos, &mut |_| true);
                found != *negate && k(pos)
            }
            RKind::Quantifier { min, max, greedy } => {
                let child = r.node(id).children[0];
                self.rep(child, *min, *max, *greedy, 0, pos, k)
            }
            RKind::Char { value } => cur.is_some_and(|c| self.char_eq(*value, c)) && k(pos + 1),
            RKind::Dot => cur.is_some_and(|c| self.dot_all || !is_line_terminator(c)) && k(pos + 1),
            RKind::CharSet { kind, negate } => {
                cur.is_some_and(|c| self.set_matches(*kind, *negate, c)) && k(pos + 1)
            }
            RKind::Class { .. } => cur.is_some_and(|c| self.class_item(id, c)) && k(pos + 1),
            RKind::Assertion(a) => {
                let before = pos.checked_sub(1).and_then(|p| self.input.get(p).copied());
                let ok = match a {
                    AssertKind::Start => {
                        pos == 0
                            || (self.multiline
                                && self
                                    .input
                                    .get(pos - 1)
                                    .is_some_and(|&c| is_line_terminator(c)))
                    }
                    AssertKind::End => {
                        pos == self.input.len()
                            || (self.multiline
                                && self.input.get(pos).is_some_and(|&c| is_line_terminator(c)))
                    }
                    AssertKind::WordBoundary | AssertKind::NotWordBoundary => {
                        let b = before.is_some_and(is_word) != cur.is_some_and(is_word);
                        b == (*a == AssertKind::WordBoundary)
                    }
                };
                ok && k(pos)
            }
            RKind::Backref { .. } => k(pos),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(p: &str, f: &str) -> Option<&'static str> {
        parse(p, Flags::new(f)).err().map(|e| e.message)
    }

    #[test]
    fn errors() {
        assert_eq!(err("[", ""), Some("Unterminated character class"));
        assert_eq!(err(")", ""), Some("Unmatched ')'"));
        assert_eq!(err("\\a", "u"), Some("Invalid escape"));
        assert_eq!(err("\\u{0}*", ""), Some("Nothing to repeat"));
        assert_eq!(err("\\", ""), Some("\\ at end of pattern"));
        assert_eq!(
            err("[A--B]", "u"),
            Some("Range out of order in character class")
        );
        assert_eq!(
            err("(?<k>a)(?<k>b)", ""),
            Some("Duplicate capture group name")
        );
        assert_eq!(err("((?<k>a)|(?<k>b))", ""), None);
        assert_eq!(err("(?ii:foo)", ""), Some("Duplicated flag 'i'"));
        assert_eq!(err("(?-:foo)", ""), Some("Invalid empty flags"));
        assert_eq!(err("(?g:foo)", ""), Some("Invalid group"));
        assert_eq!(err("[[]", "v"), Some("Unterminated character class"));
        assert_eq!(err("[A--[0-9]]", "v"), None);
        assert_eq!(err("[\\p{Basic_Emoji}--\\q{a|bc|def}]", "v"), None);
        assert_eq!(err("(?<\\ud835\\udc9c>.)", ""), None);
        assert_eq!(err("{", ""), None);
        assert_eq!(err("{", "u"), Some("Lone quantifier brackets"));
        assert_eq!(err("\\1(a)", ""), None);
    }

    #[test]
    fn deep_nesting_is_an_error() {
        let deep = |open: &str, close: &str, n: usize| open.repeat(n) + &close.repeat(n);
        let too_deep = Some("Regular expression too deeply nested");
        assert_eq!(err(&deep("(", ")", 100_000), ""), too_deep);
        assert_eq!(err(&deep("(?=", ")", 100_000), ""), too_deep);
        assert_eq!(err(&deep("[", "]", 100_000), "v"), too_deep);
        assert_eq!(err(&deep("(", ")", MAX_DEPTH as usize), ""), None);
        assert_eq!(err(&deep("[", "]", MAX_DEPTH as usize), "v"), None);
    }

    #[test]
    fn matching() {
        let m = |p: &str, t: &str| compile(p).unwrap().is_match(t, false);
        assert!(m("^_", "_foo"));
        assert!(!m("^_", "foo"));
        assert!(m("falls?\\s?through", "it falls through"));
        assert!(m("^(?:ignored|unused)$", "unused"));
        assert!(!m("^(?:ignored|unused)$", "unused2"));
        assert!(m("[a-c]+x", "zzbbbx"));
        assert!(compile("FALL").unwrap().is_match("fall", true));
    }

    #[test]
    fn matching_flags() {
        let m = |p: &str, f: &str, t: &str| parse(p, Flags::new(f)).unwrap().is_match_flags(t, f);
        assert!(!m("^a.b$", "v", "a\nb"));
        assert!(m("^a.b$", "vs", "a\nb"));
        assert!(!m("^b$", "v", "a\nb"));
        assert!(m("^b$", "vm", "a\nb"));
        assert!(m("[.\\p{RGI_Emoji}]$", "v", "Text \u{1F642}"));
        assert!(!m("[.\\p{RGI_Emoji}]$", "v", "Foo"));
    }

    #[test]
    fn surrogates() {
        let r = parse("[👍]", Flags::default()).unwrap();
        assert_eq!(r.units.len(), 4);
        let r = parse("[👍]", Flags::new("u")).unwrap();
        assert_eq!(r.units.len(), 3);
    }
}
