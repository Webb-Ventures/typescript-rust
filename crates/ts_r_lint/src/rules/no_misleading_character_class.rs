//! `no-misleading-character-class`: disallow characters which are made with multiple code
//! points in character class syntax.

use super::prelude::*;
use crate::regex::{self, Flags, RId, RKind, Regex};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-misleading-character-class",
        "Disallow characters which are made with multiple code points in character class syntax",
        ESLINT_REC,
        Category::Correctness,
    )
    .suggestions(),
    create: create::<NoMisleadingCharacterClass>,
};

#[derive(Debug, Default)]
struct NoMisleadingCharacterClass {
    allow_escape: bool,
}

impl FromOptions for NoMisleadingCharacterClass {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(NoMisleadingCharacterClass {
            allow_escape: bool_field(obj_opt(opts, 0), "allowEscape", false),
        })
    }
}

/// Unicode general categories Mn, Mc and Me (Unicode 16).
static COMBINING_MARKS: &[(u32, u32)] = &[
    (0x300, 0x36F),
    (0x483, 0x489),
    (0x591, 0x5BD),
    (0x5BF, 0x5BF),
    (0x5C1, 0x5C2),
    (0x5C4, 0x5C5),
    (0x5C7, 0x5C7),
    (0x610, 0x61A),
    (0x64B, 0x65F),
    (0x670, 0x670),
    (0x6D6, 0x6DC),
    (0x6DF, 0x6E4),
    (0x6E7, 0x6E8),
    (0x6EA, 0x6ED),
    (0x711, 0x711),
    (0x730, 0x74A),
    (0x7A6, 0x7B0),
    (0x7EB, 0x7F3),
    (0x7FD, 0x7FD),
    (0x816, 0x819),
    (0x81B, 0x823),
    (0x825, 0x827),
    (0x829, 0x82D),
    (0x859, 0x85B),
    (0x897, 0x89F),
    (0x8CA, 0x8E1),
    (0x8E3, 0x903),
    (0x93A, 0x93C),
    (0x93E, 0x94F),
    (0x951, 0x957),
    (0x962, 0x963),
    (0x981, 0x983),
    (0x9BC, 0x9BC),
    (0x9BE, 0x9C4),
    (0x9C7, 0x9C8),
    (0x9CB, 0x9CD),
    (0x9D7, 0x9D7),
    (0x9E2, 0x9E3),
    (0x9FE, 0x9FE),
    (0xA01, 0xA03),
    (0xA3C, 0xA3C),
    (0xA3E, 0xA42),
    (0xA47, 0xA48),
    (0xA4B, 0xA4D),
    (0xA51, 0xA51),
    (0xA70, 0xA71),
    (0xA75, 0xA75),
    (0xA81, 0xA83),
    (0xABC, 0xABC),
    (0xABE, 0xAC5),
    (0xAC7, 0xAC9),
    (0xACB, 0xACD),
    (0xAE2, 0xAE3),
    (0xAFA, 0xAFF),
    (0xB01, 0xB03),
    (0xB3C, 0xB3C),
    (0xB3E, 0xB44),
    (0xB47, 0xB48),
    (0xB4B, 0xB4D),
    (0xB55, 0xB57),
    (0xB62, 0xB63),
    (0xB82, 0xB82),
    (0xBBE, 0xBC2),
    (0xBC6, 0xBC8),
    (0xBCA, 0xBCD),
    (0xBD7, 0xBD7),
    (0xC00, 0xC04),
    (0xC3C, 0xC3C),
    (0xC3E, 0xC44),
    (0xC46, 0xC48),
    (0xC4A, 0xC4D),
    (0xC55, 0xC56),
    (0xC62, 0xC63),
    (0xC81, 0xC83),
    (0xCBC, 0xCBC),
    (0xCBE, 0xCC4),
    (0xCC6, 0xCC8),
    (0xCCA, 0xCCD),
    (0xCD5, 0xCD6),
    (0xCE2, 0xCE3),
    (0xCF3, 0xCF3),
    (0xD00, 0xD03),
    (0xD3B, 0xD3C),
    (0xD3E, 0xD44),
    (0xD46, 0xD48),
    (0xD4A, 0xD4D),
    (0xD57, 0xD57),
    (0xD62, 0xD63),
    (0xD81, 0xD83),
    (0xDCA, 0xDCA),
    (0xDCF, 0xDD4),
    (0xDD6, 0xDD6),
    (0xDD8, 0xDDF),
    (0xDF2, 0xDF3),
    (0xE31, 0xE31),
    (0xE34, 0xE3A),
    (0xE47, 0xE4E),
    (0xEB1, 0xEB1),
    (0xEB4, 0xEBC),
    (0xEC8, 0xECE),
    (0xF18, 0xF19),
    (0xF35, 0xF35),
    (0xF37, 0xF37),
    (0xF39, 0xF39),
    (0xF3E, 0xF3F),
    (0xF71, 0xF84),
    (0xF86, 0xF87),
    (0xF8D, 0xF97),
    (0xF99, 0xFBC),
    (0xFC6, 0xFC6),
    (0x102B, 0x103E),
    (0x1056, 0x1059),
    (0x105E, 0x1060),
    (0x1062, 0x1064),
    (0x1067, 0x106D),
    (0x1071, 0x1074),
    (0x1082, 0x108D),
    (0x108F, 0x108F),
    (0x109A, 0x109D),
    (0x135D, 0x135F),
    (0x1712, 0x1715),
    (0x1732, 0x1734),
    (0x1752, 0x1753),
    (0x1772, 0x1773),
    (0x17B4, 0x17D3),
    (0x17DD, 0x17DD),
    (0x180B, 0x180D),
    (0x180F, 0x180F),
    (0x1885, 0x1886),
    (0x18A9, 0x18A9),
    (0x1920, 0x192B),
    (0x1930, 0x193B),
    (0x1A17, 0x1A1B),
    (0x1A55, 0x1A5E),
    (0x1A60, 0x1A7C),
    (0x1A7F, 0x1A7F),
    (0x1AB0, 0x1ACE),
    (0x1B00, 0x1B04),
    (0x1B34, 0x1B44),
    (0x1B6B, 0x1B73),
    (0x1B80, 0x1B82),
    (0x1BA1, 0x1BAD),
    (0x1BE6, 0x1BF3),
    (0x1C24, 0x1C37),
    (0x1CD0, 0x1CD2),
    (0x1CD4, 0x1CE8),
    (0x1CED, 0x1CED),
    (0x1CF4, 0x1CF4),
    (0x1CF7, 0x1CF9),
    (0x1DC0, 0x1DFF),
    (0x20D0, 0x20F0),
    (0x2CEF, 0x2CF1),
    (0x2D7F, 0x2D7F),
    (0x2DE0, 0x2DFF),
    (0x302A, 0x302F),
    (0x3099, 0x309A),
    (0xA66F, 0xA672),
    (0xA674, 0xA67D),
    (0xA69E, 0xA69F),
    (0xA6F0, 0xA6F1),
    (0xA802, 0xA802),
    (0xA806, 0xA806),
    (0xA80B, 0xA80B),
    (0xA823, 0xA827),
    (0xA82C, 0xA82C),
    (0xA880, 0xA881),
    (0xA8B4, 0xA8C5),
    (0xA8E0, 0xA8F1),
    (0xA8FF, 0xA8FF),
    (0xA926, 0xA92D),
    (0xA947, 0xA953),
    (0xA980, 0xA983),
    (0xA9B3, 0xA9C0),
    (0xA9E5, 0xA9E5),
    (0xAA29, 0xAA36),
    (0xAA43, 0xAA43),
    (0xAA4C, 0xAA4D),
    (0xAA7B, 0xAA7D),
    (0xAAB0, 0xAAB0),
    (0xAAB2, 0xAAB4),
    (0xAAB7, 0xAAB8),
    (0xAABE, 0xAABF),
    (0xAAC1, 0xAAC1),
    (0xAAEB, 0xAAEF),
    (0xAAF5, 0xAAF6),
    (0xABE3, 0xABEA),
    (0xABEC, 0xABED),
    (0xFB1E, 0xFB1E),
    (0xFE00, 0xFE0F),
    (0xFE20, 0xFE2F),
    (0x101FD, 0x101FD),
    (0x102E0, 0x102E0),
    (0x10376, 0x1037A),
    (0x10A01, 0x10A03),
    (0x10A05, 0x10A06),
    (0x10A0C, 0x10A0F),
    (0x10A38, 0x10A3A),
    (0x10A3F, 0x10A3F),
    (0x10AE5, 0x10AE6),
    (0x10D24, 0x10D27),
    (0x10D69, 0x10D6D),
    (0x10EAB, 0x10EAC),
    (0x10EFC, 0x10EFF),
    (0x10F46, 0x10F50),
    (0x10F82, 0x10F85),
    (0x11000, 0x11002),
    (0x11038, 0x11046),
    (0x11070, 0x11070),
    (0x11073, 0x11074),
    (0x1107F, 0x11082),
    (0x110B0, 0x110BA),
    (0x110C2, 0x110C2),
    (0x11100, 0x11102),
    (0x11127, 0x11134),
    (0x11145, 0x11146),
    (0x11173, 0x11173),
    (0x11180, 0x11182),
    (0x111B3, 0x111C0),
    (0x111C9, 0x111CC),
    (0x111CE, 0x111CF),
    (0x1122C, 0x11237),
    (0x1123E, 0x1123E),
    (0x11241, 0x11241),
    (0x112DF, 0x112EA),
    (0x11300, 0x11303),
    (0x1133B, 0x1133C),
    (0x1133E, 0x11344),
    (0x11347, 0x11348),
    (0x1134B, 0x1134D),
    (0x11357, 0x11357),
    (0x11362, 0x11363),
    (0x11366, 0x1136C),
    (0x11370, 0x11374),
    (0x113B8, 0x113C0),
    (0x113C2, 0x113C2),
    (0x113C5, 0x113C5),
    (0x113C7, 0x113CA),
    (0x113CC, 0x113D0),
    (0x113D2, 0x113D2),
    (0x113E1, 0x113E2),
    (0x11435, 0x11446),
    (0x1145E, 0x1145E),
    (0x114B0, 0x114C3),
    (0x115AF, 0x115B5),
    (0x115B8, 0x115C0),
    (0x115DC, 0x115DD),
    (0x11630, 0x11640),
    (0x116AB, 0x116B7),
    (0x1171D, 0x1172B),
    (0x1182C, 0x1183A),
    (0x11930, 0x11935),
    (0x11937, 0x11938),
    (0x1193B, 0x1193E),
    (0x11940, 0x11940),
    (0x11942, 0x11943),
    (0x119D1, 0x119D7),
    (0x119DA, 0x119E0),
    (0x119E4, 0x119E4),
    (0x11A01, 0x11A0A),
    (0x11A33, 0x11A39),
    (0x11A3B, 0x11A3E),
    (0x11A47, 0x11A47),
    (0x11A51, 0x11A5B),
    (0x11A8A, 0x11A99),
    (0x11C2F, 0x11C36),
    (0x11C38, 0x11C3F),
    (0x11C92, 0x11CA7),
    (0x11CA9, 0x11CB6),
    (0x11D31, 0x11D36),
    (0x11D3A, 0x11D3A),
    (0x11D3C, 0x11D3D),
    (0x11D3F, 0x11D45),
    (0x11D47, 0x11D47),
    (0x11D8A, 0x11D8E),
    (0x11D90, 0x11D91),
    (0x11D93, 0x11D97),
    (0x11EF3, 0x11EF6),
    (0x11F00, 0x11F01),
    (0x11F03, 0x11F03),
    (0x11F34, 0x11F3A),
    (0x11F3E, 0x11F42),
    (0x11F5A, 0x11F5A),
    (0x13440, 0x13440),
    (0x13447, 0x13455),
    (0x1611E, 0x1612F),
    (0x16AF0, 0x16AF4),
    (0x16B30, 0x16B36),
    (0x16F4F, 0x16F4F),
    (0x16F51, 0x16F87),
    (0x16F8F, 0x16F92),
    (0x16FE4, 0x16FE4),
    (0x16FF0, 0x16FF1),
    (0x1BC9D, 0x1BC9E),
    (0x1CF00, 0x1CF2D),
    (0x1CF30, 0x1CF46),
    (0x1D165, 0x1D169),
    (0x1D16D, 0x1D172),
    (0x1D17B, 0x1D182),
    (0x1D185, 0x1D18B),
    (0x1D1AA, 0x1D1AD),
    (0x1D242, 0x1D244),
    (0x1DA00, 0x1DA36),
    (0x1DA3B, 0x1DA6C),
    (0x1DA75, 0x1DA75),
    (0x1DA84, 0x1DA84),
    (0x1DA9B, 0x1DA9F),
    (0x1DAA1, 0x1DAAF),
    (0x1E000, 0x1E006),
    (0x1E008, 0x1E018),
    (0x1E01B, 0x1E021),
    (0x1E023, 0x1E024),
    (0x1E026, 0x1E02A),
    (0x1E08F, 0x1E08F),
    (0x1E130, 0x1E136),
    (0x1E2AE, 0x1E2AE),
    (0x1E2EC, 0x1E2EF),
    (0x1E4EC, 0x1E4EF),
    (0x1E5EE, 0x1E5EF),
    (0x1E8D0, 0x1E8D6),
    (0x1E944, 0x1E94A),
    (0xE0100, 0xE01EF),
];

pub(crate) fn is_mark(c: u32) -> bool {
    COMBINING_MARKS
        .binary_search_by(|&(a, b)| {
            if b < c {
                std::cmp::Ordering::Less
            } else if a > c {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

fn is_modifier(c: u32) -> bool {
    (0x1F3FB..=0x1F3FF).contains(&c)
}

fn is_regional(c: u32) -> bool {
    (0x1F1E6..=0x1F1FF).contains(&c)
}

const ZWJ: u32 = 0x200D;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    SurrogatePairWithoutUFlag,
    SurrogatePair,
    Combining,
    EmojiModifier,
    RegionalIndicator,
    Zwj,
}

impl Kind {
    fn message(self) -> (&'static str, &'static str) {
        match self {
            Kind::SurrogatePairWithoutUFlag => (
                "surrogatePairWithoutUFlag",
                "Unexpected surrogate pair in character class. Use 'u' flag.",
            ),
            Kind::SurrogatePair => (
                "surrogatePair",
                "Unexpected surrogate pair in character class.",
            ),
            Kind::Combining => (
                "combiningClass",
                "Unexpected combined character in character class.",
            ),
            Kind::EmojiModifier => (
                "emojiModifier",
                "Unexpected modified Emoji in character class.",
            ),
            Kind::RegionalIndicator => (
                "regionalIndicatorSymbol",
                "Unexpected national flag in character class.",
            ),
            Kind::Zwj => (
                "zwj",
                "Unexpected joined character sequence in character class.",
            ),
        }
    }
}

/// Where the pattern text lives in the source.
enum Origin {
    /// A regex literal; the pattern starts at this offset.
    Literal(u32),
    /// A string or template literal: value byte offset -> source range of that character.
    Mapped(Vec<(u32, u32, u32)>),
    /// Not mappable; report the whole node.
    Node(NodeId),
}

impl Origin {
    /// Source range of the pattern bytes `s..e`; `None` reports the whole node.
    fn range(&self, s: u32, e: u32) -> Option<(u32, u32)> {
        match self {
            Origin::Literal(base) => Some((base + s, base + e)),
            Origin::Mapped(map) => {
                let first = map.iter().find(|m| m.0 >= s)?;
                let last = map.iter().rev().find(|m| m.0 < e)?;
                Some((first.1, last.2.max(first.1)))
            }
            Origin::Node(_) => None,
        }
    }

    /// Like [`Origin::range`], for a range that may start at the low half (`s_low`) or end
    /// at the high half (`e_high`; `e` is then the start of that character) of an astral
    /// character. The position between the halves is the character's start + 2 (the output
    /// maps it to the UTF-16 column of the low surrogate); characters written as escapes
    /// keep their whole range.
    fn range_split(&self, s: u32, s_low: bool, e: u32, e_high: bool) -> Option<(u32, u32)> {
        if !s_low && !e_high {
            return self.range(s, e);
        }
        match self {
            Origin::Literal(base) => Some((
                base + s + if s_low { 2 } else { 0 },
                base + e + if e_high { 2 } else { 0 },
            )),
            Origin::Mapped(map) => {
                let literal = |m: &(u32, u32, u32)| m.2 - m.1 == 4;
                let first = map.iter().find(|m| m.0 >= s)?;
                let a = if s_low && literal(first) {
                    first.1 + 2
                } else {
                    first.1
                };
                let b = if e_high {
                    let last = map.iter().find(|m| m.0 >= e)?;
                    if literal(last) { last.1 + 2 } else { last.2 }
                } else {
                    map.iter().rev().find(|m| m.0 < e)?.2
                };
                Some((a, b.max(a)))
            }
            Origin::Node(_) => None,
        }
    }
}

/// Cooks a string literal's (or template's) raw text, mapping each value character to its
/// source range. Returns `(value, [(value offset, source start, source end)])`.
/// A cooked string and its `(cooked offset, raw start, raw end)` map.
type CookMap = (String, Vec<(u32, u32, u32)>);

fn cook_map(raw: &str, base: u32, template: bool) -> Option<CookMap> {
    let b = raw.as_bytes();
    let mut out = String::new();
    let mut map = Vec::new();
    let mut i = 0;
    let hex = |s: &str| u32::from_str_radix(s, 16).ok();
    while i < b.len() {
        let c = raw[i..].chars().next()?;
        let (ch, end) = if c == '\\' {
            let e = raw[i + 1..].chars().next()?;
            let j = i + 1 + e.len_utf8();
            match e {
                '\n' | '\u{2028}' | '\u{2029}' => {
                    i = j;
                    continue;
                }
                '\r' => {
                    i = j + usize::from(b.get(j) == Some(&b'\n'));
                    continue;
                }
                'n' => ('\n', j),
                't' => ('\t', j),
                'r' => ('\r', j),
                'b' => ('\u{8}', j),
                'f' => ('\u{c}', j),
                'v' => ('\u{b}', j),
                'x' => {
                    let v = hex(raw.get(j..j + 2)?)?;
                    (char::from_u32(v)?, j + 2)
                }
                'u' => {
                    let (v, mut k) = if raw[j..].starts_with('{') {
                        let close = raw[j..].find('}')? + j;
                        (hex(&raw[j + 1..close])?, close + 1)
                    } else {
                        (hex(raw.get(j..j + 4)?)?, j + 4)
                    };
                    let mut v = v;
                    if (0xD800..=0xDBFF).contains(&v)
                        && raw[k..].starts_with("\\u")
                        && let Some(l) = raw.get(k + 2..k + 6).and_then(hex)
                        && (0xDC00..=0xDFFF).contains(&l)
                    {
                        v = 0x10000 + ((v - 0xD800) << 10) + (l - 0xDC00);
                        k += 6;
                    }
                    (char::from_u32(v).unwrap_or('\u{FFFD}'), k)
                }
                '0'..='7' => {
                    let mut k = i + 1;
                    let mut v = 0;
                    let max = if e <= '3' { 3 } else { 2 };
                    while k < b.len() && k - (i + 1) < max && (b'0'..=b'7').contains(&b[k]) {
                        v = v * 8 + u32::from(b[k] - b'0');
                        k += 1;
                    }
                    (char::from_u32(v)?, k)
                }
                _ => (e, j),
            }
        } else if template && c == '\r' {
            ('\n', i + 1 + usize::from(b.get(i + 1) == Some(&b'\n')))
        } else {
            (c, i + c.len_utf8())
        };
        map.push((out.len() as u32, base + i as u32, base + end as u32));
        out.push(ch);
        i = end;
    }
    Some((out, map))
}

struct Found {
    kind: Kind,
    first: RId,
    last: RId,
}

impl NoMisleadingCharacterClass {
    fn scan(&self, r: &Regex<'_>, escaped: &dyn Fn(RId) -> bool) -> Vec<Found> {
        let umode = r.flags.unicode || r.flags.sets;
        let mut out = Vec::new();
        for n in &r.nodes {
            if !matches!(n.kind, RKind::Class { .. }) {
                continue;
            }
            let mut seq: Vec<Option<RId>> = Vec::new();
            for &c in &n.children {
                match r.kind(c) {
                    RKind::Char { .. } => seq.push(Some(c)),
                    // A range ends the sequence at its minimum and starts a new one at
                    // its maximum (`[​-‍﻿]` has no joined sequence).
                    RKind::Range => {
                        let ends = &r.node(c).children;
                        if let (Some(&min), Some(&max)) = (ends.first(), ends.last()) {
                            seq.extend([Some(min), None, Some(max)]);
                        }
                    }
                    _ => seq.push(None),
                }
            }
            let val = |i: usize| seq.get(i).copied().flatten().and_then(|c| r.char_value(c));
            let id = |i: usize| seq[i].unwrap_or(0);
            let len = seq.len();
            let mut i = 0;
            while i + 1 < len {
                let (Some(a), Some(b)) = (val(i), val(i + 1)) else {
                    i += 1;
                    continue;
                };
                if (0xD800..=0xDBFF).contains(&a) && (0xDC00..=0xDFFF).contains(&b) {
                    if !(self.allow_escape && escaped(id(i + 1))) {
                        out.push(Found {
                            kind: if umode {
                                Kind::SurrogatePair
                            } else {
                                Kind::SurrogatePairWithoutUFlag
                            },
                            first: id(i),
                            last: id(i + 1),
                        });
                    }
                } else if is_regional(a) && is_regional(b) {
                    if !(self.allow_escape && escaped(id(i + 1))) {
                        out.push(Found {
                            kind: Kind::RegionalIndicator,
                            first: id(i),
                            last: id(i + 1),
                        });
                    }
                    i += 2;
                    continue;
                } else if is_modifier(b)
                    && !is_modifier(a)
                    && !(self.allow_escape && escaped(id(i + 1)))
                {
                    out.push(Found {
                        kind: Kind::EmojiModifier,
                        first: id(i),
                        last: id(i + 1),
                    });
                }
                i += 1;
            }
            // Combining marks after a base character.
            let mut i = 0;
            while i + 1 < len {
                if val(i).is_some_and(|a| !is_mark(a)) && val(i + 1).is_some_and(is_mark) {
                    let mut j = i + 1;
                    while val(j + 1).is_some_and(is_mark) {
                        j += 1;
                    }
                    if !(self.allow_escape && (i + 1..=j).all(|k| escaped(id(k)))) {
                        out.push(Found {
                            kind: Kind::Combining,
                            first: id(i),
                            last: id(j),
                        });
                    }
                    i = j + 1;
                } else {
                    i += 1;
                }
            }
            // Zero-width-joiner sequences.
            let mut i = 0;
            while i + 2 < len {
                if val(i).is_some_and(|a| a != ZWJ)
                    && val(i + 1) == Some(ZWJ)
                    && val(i + 2).is_some()
                {
                    let mut j = i + 2;
                    while val(j + 1) == Some(ZWJ) && val(j + 2).is_some() {
                        j += 2;
                    }
                    let joins = (i + 1..j).step_by(2);
                    if !(self.allow_escape && joins.clone().all(|k| escaped(id(k)))) {
                        out.push(Found {
                            kind: Kind::Zwj,
                            first: id(i),
                            last: id(j),
                        });
                    }
                    i = j;
                } else {
                    i += 1;
                }
            }
        }
        out
    }

    fn check(
        &self,
        cx: &mut Ctx<'_, '_>,
        pattern: &str,
        flags: &str,
        origin: &Origin,
        add_u: Option<Fix>,
    ) {
        let Ok(r) = regex::parse(pattern, Flags::new(flags)) else {
            return;
        };
        let src = cx.src();
        let escaped = |c: RId| -> bool {
            let (s, e) = r.byte_range(c);
            // A regex escape, or a string escape that spells the code unit out
            // (`"\\uD83D"`); `"\\👍"` in a string is just the character.
            r.text(c).starts_with('\\')
                || origin.range(s, e).is_some_and(|(a, _)| {
                    let t = &src[a as usize..];
                    t.starts_with("\\u") || t.starts_with("\\x")
                })
        };
        let found = self.scan(&r, &escaped);
        if found.is_empty() {
            return;
        }
        let add_u = add_u.filter(|_| {
            crate::globals::normalize_ecma_version(cx.env().ecma_version) >= 2015
                && !flags.contains('v')
                && regex::parse(pattern, Flags::new(&format!("{flags}u"))).is_ok()
        });
        let mut reports: Vec<(Span, Kind)> = Vec::new();
        for f in found {
            // Without `u`, a sequence can start at the low half or end at the high half of
            // an astral character: ESLint's columns count code units.
            let s_low = r.split_half(f.first) == Some(true);
            let e_high = r.split_half(f.last) == Some(false);
            let s = r.byte_range(f.first).0;
            let e = if e_high {
                r.byte_range(f.last).0
            } else {
                r.byte_range(f.last).1
            };
            let span = match (origin.range_split(s, s_low, e, e_high), origin) {
                (Some((a, b)), _) => Span::new(a, b),
                (None, Origin::Node(n)) => cx.span(*n),
                (None, _) => continue,
            };
            if !reports.contains(&(span, f.kind)) {
                reports.push((span, f.kind));
            }
        }
        reports.sort_by_key(|(s, _)| (s.start, s.end));
        for (span, kind) in reports {
            let (id, msg) = kind.message();
            let mut rep = cx.report(span, msg).id(id);
            if kind == Kind::SurrogatePairWithoutUFlag
                && let Some(f) = &add_u
            {
                rep = rep.suggest(
                    "suggestUnicodeFlag",
                    "Add unicode 'u' flag to regex.",
                    f.clone(),
                );
            }
            let _ = rep;
        }
    }
}

/// The end of `id` including any parentheses around it.
fn outer_end(cx: &Ctx<'_, '_>, id: NodeId) -> u32 {
    let ast = cx.ast();
    let mut cur = id;
    while let Some(p) = cx.parent(cur) {
        if matches!(ast.node(p), Node::Paren { .. }) {
            cur = p;
        } else {
            break;
        }
    }
    cx.span(cur).end
}

/// A string literal or expression-free template: its value and character map.
fn literal_text(cx: &Ctx<'_, '_>, id: NodeId) -> Option<(String, Origin)> {
    let ast = cx.ast();
    let raw = cx.text(id);
    let base = cx.span(id).start + 1;
    let (value, template) = match *ast.node(id) {
        Node::StringLit { value } => (ast.atom(value).to_owned(), false),
        Node::TemplateLit { parts } => match ast.list(parts) {
            [p] => match *ast.node(*p) {
                Node::TemplateElement { cooked, .. } => (ast.atom(cooked).to_owned(), true),
                _ => return None,
            },
            _ => return None,
        },
        _ => return None,
    };
    if raw.len() < 2 {
        return None;
    }
    match cook_map(&raw[1..raw.len() - 1], base, template) {
        Some((v, map)) if v == value => Some((value, Origin::Mapped(map))),
        _ => Some((value, Origin::Node(id))),
    }
}

impl Rule for NoMisleadingCharacterClass {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::RegexLit, NodeKind::Call, NodeKind::New]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        if let Some((pattern, flags)) = regex_parts(ast, node) {
            // A literal passed to `RegExp` is checked with the call's flags.
            if let Some(call) = parent_skip_parens(ast, cx.file.parents, node)
                && let Some(args) = regexp_call_args(cx, call, true, true)
                && args.first().is_some_and(|&a| skip_parens(ast, a) == node)
            {
                return;
            }
            let span = cx.span(node);
            let fix = Fix::insert(span.end, "u");
            self.check(
                cx,
                pattern,
                flags,
                &Origin::Literal(span.start + 1),
                Some(fix),
            );
            return;
        }
        let Some(args) = regexp_call_args(cx, node, true, true) else {
            return;
        };
        let Some(&arg0) = args.first() else { return };
        let p = skip_parens(ast, arg0);
        let mut lit_flags: Option<String> = None;
        let mut lit_end = None;
        let (pattern, origin) = match *ast.node(p) {
            Node::RegexLit { .. } => {
                let Some((pat, fl)) = regex_parts(ast, p) else {
                    return;
                };
                lit_flags = Some(fl.to_owned());
                lit_end = Some(cx.span(p).end);
                (pat.to_owned(), Origin::Literal(cx.span(p).start + 1))
            }
            Node::StringLit { .. } | Node::TemplateLit { .. } => match literal_text(cx, p) {
                Some(x) => x,
                None => match static_string_eval(cx, p) {
                    Some(v) => (v, Origin::Node(arg0)),
                    None => return,
                },
            },
            _ => match static_string_eval(cx, p) {
                Some(v) => (v, Origin::Node(arg0)),
                None => return,
            },
        };
        let (flags, fix) = match args.get(1) {
            None => (
                lit_flags.unwrap_or_default(),
                Some(match lit_end {
                    Some(end) => Fix::insert(end, "u"),
                    None => Fix::insert(outer_end(cx, arg0), ", \"u\""),
                }),
            ),
            Some(&f) => {
                let fp = skip_parens(ast, f);
                match literal_text(cx, fp) {
                    Some((v, _)) => (v, Some(Fix::insert(cx.span(fp).end - 1, "u"))),
                    None => match static_string_eval(cx, fp) {
                        Some(v) => (v, None),
                        None => return,
                    },
                }
            }
        };
        self.check(cx, &pattern, &flags, &origin, fix);
    }
}
