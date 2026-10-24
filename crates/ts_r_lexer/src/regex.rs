//! The regular expression pattern checker: the grammar errors tsc 7 reports for a regex
//! literal's pattern (TS1005, TS1125, TS1126, TS1198, TS1199, TS1487, TS1488 and
//! TS1503-TS1538). tsc reports them from its checker, so the parser keeps them only in a
//! file without syntax errors. The flags are checked by `Lexer::rescan_slash_as_regex`.
//!
//! Written from ECMA-262 (22.2 Patterns, with the Annex B extensions that tsc always
//! allows) and from the behaviour of tsc 7's regex scanner
//! (`tsc/internal/scanner/regexp.go`), observed through its test baselines:
//!
//! - Without `u` or `v` the Annex B grammar applies: a lone `{`, `}` or `]` is a literal,
//!   `\c` without a letter is `\` and `c`, `\k` without `<` is `k` unless the pattern has a
//!   named group, and an astral character is two UTF-16 units (so `[😀-😁]` is out of
//!   order). With `u` or `v` all of those are errors, and so is escaping a character that
//!   is not a syntax character (`\a`).
//! - Errors come in scan order. An error at the position of the error just before it is
//!   dropped (as the checker drops it). TS1503 (named groups before ES2018) and TS1501 (the
//!   `s` modifier before ES2018) are always produced, and the checker drops them for later
//!   targets, so they do not take part in that rule.
//! - tsc's spelling suggestions ("Did you mean ...?") come at the position of the error
//!   they follow, so they are always dropped; they are not produced here.
//! - Nesting (groups, `v`-mode classes) deeper than [`MAX_DEPTH`] is not checked: such a
//!   pattern gets no errors rather than a stack overflow.

use std::collections::HashSet;

use ts_r_diagnostics::{DiagnosticMessage, messages as m};

use crate::{chars, regex_props as props};

/// Deepest nesting of groups and `v`-mode classes that is checked.
const MAX_DEPTH: u32 = 256;

/// One error: byte offsets in the source, the message and its argument (empty if none).
pub(crate) struct RegexError {
    pub start: usize,
    pub end: usize,
    pub msg: DiagnosticMessage,
    pub arg: String,
}

/// Flag bits, as the lexer collects them and [`validate`] reads them.
pub(crate) const FLAG_D: u8 = 1;
pub(crate) const FLAG_G: u8 = 1 << 1;
pub(crate) const FLAG_I: u8 = 1 << 2;
pub(crate) const FLAG_M: u8 = 1 << 3;
pub(crate) const FLAG_S: u8 = 1 << 4;
pub(crate) const FLAG_U: u8 = 1 << 5;
pub(crate) const FLAG_V: u8 = 1 << 6;
pub(crate) const FLAG_Y: u8 = 1 << 7;

/// The bit of a regex flag character.
pub(crate) fn flag_bit(c: char) -> Option<u8> {
    Some(match c {
        'd' => FLAG_D,
        'g' => FLAG_G,
        'i' => FLAG_I,
        'm' => FLAG_M,
        's' => FLAG_S,
        'u' => FLAG_U,
        'v' => FLAG_V,
        'y' => FLAG_Y,
        _ => return None,
    })
}

/// The target a flag needs, the argument of TS1501 (`d`: ES2022, `s`: ES2018, `v`: ES2024).
pub(crate) fn flag_target(bit: u8) -> Option<&'static str> {
    match bit {
        FLAG_D => Some("es2022"),
        FLAG_S => Some("es2018"),
        FLAG_V => Some("es2024"),
        _ => None,
    }
}

/// Checks the pattern `src[start..end]` (between the slashes) under the accepted `flags`.
pub(crate) fn validate(src: &str, start: usize, end: usize, flags: u8) -> Vec<RegexError> {
    let b = src.as_bytes();
    let end = end.min(b.len());
    let start = start.min(end);
    let mut v = Validator {
        src,
        b,
        pos: start,
        end,
        unicode: flags & (FLAG_U | FLAG_V) != 0,
        sets: flags & FLAG_V != 0,
        named_groups: has_named_group(&b[start..end]),
        may_strings: false,
        groups: 0,
        specifiers: HashSet::new(),
        references: Vec::new(),
        backreferences: Vec::new(),
        scopes: Vec::new(),
        pending_low: 0,
        depth: 0,
        too_deep: false,
        last_start: None,
        errors: Vec::new(),
    };
    v.disjunction(false);
    if v.too_deep {
        return Vec::new();
    }
    for (s, e, name) in std::mem::take(&mut v.references) {
        if !v.specifiers.contains(&name) {
            v.error_with(s, e, m::REGEX_NO_SUCH_GROUP_NAME, name);
        }
    }
    for (s, e, value) in std::mem::take(&mut v.backreferences) {
        if value > u64::from(v.groups) {
            if v.groups > 0 {
                let count = v.groups.to_string();
                v.error_with(s, e, m::REGEX_BACKREFERENCE_PAST_GROUPS, count);
            } else {
                v.error(s, e, m::REGEX_BACKREFERENCE_NO_GROUPS);
            }
        }
    }
    v.errors
}

/// Whether the pattern has a named group `(?<name>` outside a class: it makes `\k` without
/// a name an error even without `u`.
fn has_named_group(p: &[u8]) -> bool {
    let mut i = 0;
    let mut in_class = false;
    while i < p.len() {
        match p[i] {
            b'\\' => i += 1,
            b'[' => in_class = true,
            b']' => in_class = false,
            b'(' if !in_class
                && p.get(i + 1) == Some(&b'?')
                && p.get(i + 2) == Some(&b'<')
                && !matches!(p.get(i + 3), Some(b'=' | b'!')) =>
            {
                return true;
            }
            _ => {}
        }
        i += 1;
    }
    false
}

/// A class atom or class set operand: nothing (a class escape or a nested class), one
/// character (a code point, or a UTF-16 unit without `u`) or several.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Atom {
    Empty,
    One(u32),
    Many,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SetOp {
    Subtraction,
    Intersection,
}

struct Validator<'a> {
    src: &'a str,
    b: &'a [u8],
    pos: usize,
    end: usize,
    /// `u` or `v`.
    unicode: bool,
    /// `v`.
    sets: bool,
    named_groups: bool,
    /// Whether the class set operand or expression just scanned may match a string of
    /// more than one character (`\q{ab}`, `\p{RGI_Emoji}`).
    may_strings: bool,
    /// Capturing groups, named or not.
    groups: u32,
    /// Names of the named groups.
    specifiers: HashSet<String>,
    /// `\k<name>`: the name's span and the name.
    references: Vec<(usize, usize, String)>,
    /// `\1`: its digits' span and value.
    backreferences: Vec<(usize, usize, u64)>,
    /// The group names of each enclosing alternative: two groups may share a name only in
    /// different alternatives.
    scopes: Vec<HashSet<String>>,
    /// Without `u`: the second UTF-16 unit of the astral character at `pos`.
    pending_low: u32,
    depth: u32,
    too_deep: bool,
    last_start: Option<usize>,
    errors: Vec<RegexError>,
}

impl Validator<'_> {
    fn error(&mut self, start: usize, end: usize, msg: DiagnosticMessage) {
        self.error_with(start, end, msg, String::new());
    }

    fn error_with(&mut self, start: usize, end: usize, msg: DiagnosticMessage, arg: String) {
        if self.too_deep || self.last_start == Some(start) {
            return;
        }
        if msg.code != m::REGEX_FLAG_NEEDS_TARGET.code
            && msg.code != m::REGEX_NAMED_GROUPS_NEED_TARGET.code
        {
            self.last_start = Some(start);
        }
        self.errors.push(RegexError {
            start,
            end: end.max(start),
            msg,
            arg,
        });
    }

    /// `'x' expected` at the current position.
    fn expected(&mut self, what: &str) {
        self.error_with(self.pos, self.pos, m::EXPECTED, what.to_string());
    }

    fn unexpected(&mut self, at: usize, c: char) {
        self.error_with(
            at,
            at + c.len_utf8(),
            m::REGEX_UNEXPECTED_CHAR,
            c.to_string(),
        );
    }

    fn at(&self, i: usize) -> Option<u8> {
        if i < self.end { Some(self.b[i]) } else { None }
    }

    fn peek(&self) -> Option<u8> {
        self.at(self.pos)
    }

    fn char_at(&self, i: usize) -> Option<char> {
        self.src.get(i..self.end)?.chars().next()
    }

    fn eat(&mut self, c: u8) -> bool {
        let found = self.peek() == Some(c);
        if found {
            self.pos += 1;
        }
        found
    }

    fn expect(&mut self, c: u8) {
        if !self.eat(c) {
            self.expected(&(c as char).to_string());
        }
    }

    /// Skips decimal digits; returns where they start.
    fn digits(&mut self) -> usize {
        let start = self.pos;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.pos += 1;
        }
        start
    }

    /// Enters a nesting level; false (and the pattern is abandoned) past [`MAX_DEPTH`].
    fn enter(&mut self) -> bool {
        if self.depth >= MAX_DEPTH {
            self.too_deep = true;
            self.pos = self.end;
            return false;
        }
        self.depth += 1;
        true
    }

    /// `Alternative ('|' Alternative)*`.
    fn disjunction(&mut self, in_group: bool) {
        if !self.enter() {
            return;
        }
        loop {
            self.scopes.push(HashSet::new());
            self.alternative(in_group);
            self.scopes.pop();
            if !self.eat(b'|') {
                break;
            }
        }
        self.depth -= 1;
    }

    /// A sequence of terms, up to `|`, the group's `)` or the end.
    fn alternative(&mut self, in_group: bool) {
        // Whether the term before can take a quantifier.
        let mut quantifiable = false;
        while self.pos < self.end {
            let start = self.pos;
            let c = self.b[start];
            match c {
                b'^' | b'$' => {
                    self.pos += 1;
                    quantifiable = false;
                }
                b'\\' => {
                    self.pos += 1;
                    if matches!(self.peek(), Some(b'b' | b'B')) {
                        self.pos += 1;
                        quantifiable = false;
                    } else {
                        self.atom_escape();
                        quantifiable = true;
                    }
                }
                b'(' => {
                    self.pos += 1;
                    quantifiable = self.group_head();
                    self.disjunction(true);
                    self.expect(b')');
                }
                b'{' => {
                    self.pos += 1;
                    if self.brace_quantifier(start) {
                        self.quantifier(start, &mut quantifiable);
                    } else {
                        quantifiable = true;
                    }
                }
                b'*' | b'+' | b'?' => self.quantifier(start, &mut quantifiable),
                b'.' => {
                    self.pos += 1;
                    quantifiable = true;
                }
                b'[' => {
                    self.pos += 1;
                    if self.sets {
                        self.class_set_expression();
                    } else {
                        self.class_ranges();
                        self.pending_low = 0;
                    }
                    self.expect(b']');
                    quantifiable = true;
                }
                b')' if in_group => return,
                b')' | b']' | b'}' => {
                    if self.unicode || c == b')' {
                        self.unexpected(start, c as char);
                    }
                    self.pos += 1;
                    quantifiable = true;
                }
                b'/' | b'|' => return,
                _ => {
                    self.source_char();
                    quantifiable = true;
                }
            }
        }
    }

    /// After `(`: the group's kind (`(?=`, `(?<name>`, `(?i-m:`, ...). Returns whether the
    /// group can take a quantifier.
    fn group_head(&mut self) -> bool {
        if !self.eat(b'?') {
            self.groups += 1;
            return true;
        }
        match self.peek() {
            Some(b'=' | b'!') => {
                self.pos += 1;
                // Annex B: a lookahead takes a quantifier without `u`.
                !self.unicode
            }
            Some(b'<') => {
                let name_start = self.pos;
                self.pos += 1;
                if matches!(self.peek(), Some(b'=' | b'!')) {
                    self.pos += 1;
                    return false;
                }
                self.group_name(false);
                self.expect(b'>');
                self.error(name_start, self.pos, m::REGEX_NAMED_GROUPS_NEED_TARGET);
                self.groups += 1;
                true
            }
            _ => {
                let flags_start = self.pos;
                let set = self.modifiers(0);
                if self.eat(b'-') {
                    self.modifiers(set);
                    if self.pos == flags_start + 1 {
                        self.error(flags_start, self.pos, m::REGEX_MODIFIER_FLAGS_MISSING);
                    }
                }
                self.expect(b':');
                true
            }
        }
    }

    /// After `{` at `start`: a `{n}`, `{n,}` or `{n,m}` quantifier. Returns whether a
    /// quantifier follows (then `pos` is at its last character); otherwise the `{` was a
    /// literal.
    fn brace_quantifier(&mut self, start: usize) -> bool {
        let digits_start = self.digits();
        let min_end = self.pos;
        let min = &self.src[digits_start..min_end];
        if !self.unicode && min.is_empty() {
            return false;
        }
        if self.eat(b',') {
            let max_start = self.digits();
            let max = &self.src[max_start..self.pos];
            if min.is_empty() {
                if !max.is_empty() || self.peek() == Some(b'}') {
                    self.error(digits_start, digits_start, m::REGEX_INCOMPLETE_QUANTIFIER);
                } else {
                    self.unexpected(start, '{');
                    return false;
                }
            } else if !max.is_empty()
                && compare_decimal(min, max) == std::cmp::Ordering::Greater
                && (self.unicode || self.peek() == Some(b'}'))
            {
                self.error(digits_start, self.pos, m::REGEX_QUANTIFIER_OUT_OF_ORDER);
            }
        } else if min.is_empty() {
            // Only reached with `u`: without it a `{` without digits is a literal.
            self.unexpected(start, '{');
            return false;
        }
        if self.peek() != Some(b'}') {
            if !self.unicode {
                return false;
            }
            self.expected("}");
            // The quantifier ends here: step back so that `quantifier` steps over nothing.
            self.pos -= 1;
        }
        true
    }

    /// At the last character of a quantifier prefix (`*`, `+`, `?`, `}`).
    fn quantifier(&mut self, start: usize, quantifiable: &mut bool) {
        self.pos += 1;
        self.eat(b'?');
        if !*quantifiable {
            self.error(start, self.pos, m::REGEX_NOTHING_TO_REPEAT);
        }
        *quantifiable = false;
    }

    /// The flags of a `(?ims-ims:` group, after the `currently` set ones.
    fn modifiers(&mut self, mut currently: u8) -> u8 {
        while let Some(c) = self.char_at(self.pos) {
            if !chars::is_id_part(c) {
                break;
            }
            let (at, end) = (self.pos, self.pos + c.len_utf8());
            match flag_bit(c) {
                None => self.error(at, end, m::UNKNOWN_REGEX_FLAG),
                Some(f) if currently & f != 0 => self.error(at, end, m::DUPLICATE_REGEX_FLAG),
                Some(f) if f & (FLAG_I | FLAG_M | FLAG_S) == 0 => {
                    self.error(at, end, m::REGEX_FLAG_NOT_TOGGLEABLE);
                }
                Some(f) => {
                    currently |= f;
                    if let Some(target) = flag_target(f) {
                        self.error_with(at, end, m::REGEX_FLAG_NEEDS_TARGET, target.to_string());
                    }
                }
            }
            self.pos = end;
        }
        currently
    }

    /// After `\` outside a class.
    fn atom_escape(&mut self) {
        match self.peek() {
            Some(b'k') => {
                self.pos += 1;
                if self.eat(b'<') {
                    self.group_name(true);
                    self.expect(b'>');
                } else if self.unicode || self.named_groups {
                    self.error(self.pos - 2, self.pos, m::REGEX_K_NEEDS_GROUP_NAME);
                }
            }
            Some(b'q') if self.sets => {
                self.pos += 1;
                self.error(self.pos - 2, self.pos, m::REGEX_Q_OUTSIDE_CLASS);
            }
            _ => {
                if !self.class_escape() && !self.decimal_escape() {
                    self.character_escape(true);
                }
            }
        }
    }

    /// `\1` (after the `\`): a backreference, checked against the groups at the end.
    fn decimal_escape(&mut self) -> bool {
        if !matches!(self.peek(), Some(b'1'..=b'9')) {
            return false;
        }
        let start = self.digits();
        let value = self.src[start..self.pos].parse().unwrap_or(u64::MAX);
        self.backreferences.push((start, self.pos, value));
        true
    }

    /// A character escape after `\` (`atom`: outside a class).
    fn character_escape(&mut self, atom: bool) -> Atom {
        let Some(c) = self.peek() else {
            self.error(self.pos - 1, self.pos, m::REGEX_UNDETERMINED_ESCAPE);
            return Atom::One(u32::from(b'\\'));
        };
        match c {
            b'c' => {
                self.pos += 1;
                match self.peek() {
                    Some(l) if l.is_ascii_alphabetic() => {
                        self.pos += 1;
                        Atom::One(u32::from(l & 0x1f))
                    }
                    _ => {
                        if self.unicode {
                            self.error(self.pos - 2, self.pos, m::REGEX_C_NEEDS_LETTER);
                        } else if atom {
                            // Annex B: `\c` is a backslash, then `c`.
                            self.pos -= 1;
                            return Atom::One(u32::from(b'\\'));
                        }
                        Atom::One(self.char_at(self.pos).map_or(0xFFFD, |c| c as u32))
                    }
                }
            }
            b'^' | b'$' | b'/' | b'\\' | b'.' | b'*' | b'+' | b'?' | b'(' | b')' | b'[' | b']'
            | b'{' | b'}' | b'|' => {
                self.pos += 1;
                Atom::One(u32::from(c))
            }
            _ => {
                self.pos -= 1;
                self.escape_sequence(atom)
            }
        }
    }

    /// The escape at `\` that is neither a syntax character nor `\c`.
    fn escape_sequence(&mut self, atom: bool) -> Atom {
        let start = self.pos;
        self.pos += 1;
        let Some(c) = self.peek() else {
            self.error(self.pos, self.pos, m::UNEXPECTED_END_OF_TEXT);
            return Atom::Empty;
        };
        self.pos += 1;
        let one = |v: u8| Atom::One(u32::from(v));
        match c {
            b'0' if !self.peek().is_some_and(|d| d.is_ascii_digit()) => one(0),
            b'0'..=b'7' => {
                // `\0`-`\3` take up to two more octal digits, `\4`-`\7` one.
                let more = if c <= b'3' { 2 } else { 1 };
                for _ in 0..more {
                    if !matches!(self.peek(), Some(b'0'..=b'7')) {
                        break;
                    }
                    self.pos += 1;
                }
                let value = self.b[start + 1..self.pos]
                    .iter()
                    .fold(0u32, |v, d| v * 8 + u32::from(d - b'0'));
                let syntax = format!("\\x{value:02x}");
                let msg = if !atom && c != b'0' {
                    m::REGEX_OCTAL_IN_CLASS
                } else {
                    m::OCTAL_ESCAPE_NOT_ALLOWED
                };
                self.error_with(start, self.pos, msg, syntax);
                Atom::One(value)
            }
            b'8' | b'9' => {
                if atom {
                    let text = self.src[start..self.pos].to_string();
                    self.error_with(start, self.pos, m::INVALID_ESCAPE_SEQUENCE, text);
                } else {
                    self.error(start, self.pos, m::REGEX_DECIMAL_IN_CLASS);
                }
                one(c)
            }
            b'b' => one(8),
            b't' => one(9),
            b'n' => one(10),
            b'v' => one(11),
            b'f' => one(12),
            b'r' => one(13),
            b'\'' | b'"' => one(c),
            b'u' => self.unicode_escape(start),
            b'x' => {
                for _ in 0..2 {
                    if !self.peek().is_some_and(|d| d.is_ascii_hexdigit()) {
                        self.error(self.pos, self.pos, m::HEXADECIMAL_DIGIT_EXPECTED);
                        return Atom::Many;
                    }
                    self.pos += 1;
                }
                Atom::One(hex_value(&self.b[start + 2..self.pos]))
            }
            _ => {
                let ch = self.char_at(self.pos - 1).unwrap_or('\u{FFFD}');
                self.pos += ch.len_utf8() - 1;
                if matches!(ch, '\u{2028}' | '\u{2029}') {
                    return Atom::Empty;
                }
                if self.unicode {
                    self.error(start, self.pos, m::REGEX_CANNOT_ESCAPE);
                }
                Atom::One(ch as u32)
            }
        }
    }

    /// `\uXXXX` or `\u{X..}` at `start`, with `pos` after the `u`.
    fn unicode_escape(&mut self, start: usize) -> Atom {
        let braced = self.peek() == Some(b'{');
        let value = self.unicode_escape_value();
        if braced {
            if !self.unicode {
                self.error(start, self.pos, m::REGEX_UNICODE_ESCAPE_NEEDS_FLAG);
            }
            return value.map_or(Atom::Many, Atom::One);
        }
        let Some(value) = value else {
            return Atom::Many;
        };
        // With `u`, `𝐀` is one code point.
        if self.unicode
            && (0xD800..0xDC00).contains(&value)
            && self.peek() == Some(b'\\')
            && self.at(self.pos + 1) == Some(b'u')
            && self.at(self.pos + 2) != Some(b'{')
        {
            let saved = self.pos;
            self.pos += 2;
            if let Some(low) = self.unicode_escape_value()
                && (0xDC00..0xE000).contains(&low)
            {
                return Atom::One(0x10000 + ((value - 0xD800) << 10) + (low - 0xDC00));
            }
            self.pos = saved;
        }
        Atom::One(value)
    }

    /// The value of a unicode escape after `\u`, or `None` (with its error) if malformed.
    fn unicode_escape_value(&mut self) -> Option<u32> {
        if !self.eat(b'{') {
            let digits = self.pos;
            for _ in 0..4 {
                if !self.peek().is_some_and(|d| d.is_ascii_hexdigit()) {
                    self.error(self.pos, self.pos, m::HEXADECIMAL_DIGIT_EXPECTED);
                    return None;
                }
                self.pos += 1;
            }
            return Some(hex_value(&self.b[digits..self.pos]));
        }
        let digits = self.pos;
        while self.peek().is_some_and(|d| d.is_ascii_hexdigit()) {
            self.pos += 1;
        }
        if self.pos == digits {
            self.error(self.pos, self.pos, m::HEXADECIMAL_DIGIT_EXPECTED);
            return None;
        }
        let value = hex_value(&self.b[digits..self.pos]);
        let mut valid = true;
        if value > 0x10FFFF {
            self.error(digits, self.pos, m::EXTENDED_UNICODE_ESCAPE_RANGE);
            valid = false;
        }
        if self.pos >= self.end {
            self.error(self.pos, self.pos, m::UNEXPECTED_END_OF_TEXT);
            valid = false;
        } else if !self.eat(b'}') {
            self.error(self.pos, self.pos, m::INVALID_UNICODE_ESCAPE);
            valid = false;
        }
        valid.then_some(value)
    }

    /// The name after `(?<` or `\k<`.
    fn group_name(&mut self, reference: bool) {
        let start = self.pos;
        let name = self.identifier();
        if self.pos == start {
            self.error(start, start, m::REGEX_GROUP_NAME_EXPECTED);
        } else if reference {
            self.references.push((start, self.pos, name));
        } else if self.scopes.iter().any(|s| s.contains(&name)) {
            self.error(start, self.pos, m::REGEX_DUPLICATE_GROUP_NAME);
        } else {
            if let Some(scope) = self.scopes.last_mut() {
                scope.insert(name.clone());
            }
            self.specifiers.insert(name);
        }
    }

    /// An identifier at `pos` (its characters may be unicode escapes), or nothing.
    fn identifier(&mut self) -> String {
        let mut name = String::new();
        match self.char_at(self.pos) {
            Some(c) if chars::is_id_start(c) => {
                name.push(c);
                self.pos += c.len_utf8();
            }
            Some('\\') => match self.peek_unicode_escape() {
                Some((c, len)) if chars::is_id_start(c) => {
                    name.push(c);
                    self.pos += len;
                }
                _ => return name,
            },
            _ => return name,
        }
        loop {
            match self.char_at(self.pos) {
                Some(c) if chars::is_id_part(c) => {
                    name.push(c);
                    self.pos += c.len_utf8();
                }
                Some('\\') => match self.peek_unicode_escape() {
                    Some((c, len)) if chars::is_id_part(c) => {
                        name.push(c);
                        self.pos += len;
                    }
                    _ => break,
                },
                _ => break,
            }
        }
        name
    }

    /// A well-formed `\uXXXX` or `\u{X..}` at `pos`: its character and length.
    fn peek_unicode_escape(&self) -> Option<(char, usize)> {
        if self.at(self.pos + 1) != Some(b'u') {
            return None;
        }
        let rest = self.b.get(self.pos + 2..self.end)?;
        let (digits, len) = if rest.first() == Some(&b'{') {
            let n = rest[1..]
                .iter()
                .take_while(|d| d.is_ascii_hexdigit())
                .count();
            if n == 0 || rest.get(1 + n) != Some(&b'}') {
                return None;
            }
            (&rest[1..1 + n], n + 4)
        } else {
            let digits = rest.get(..4)?;
            if !digits.iter().all(|d| d.is_ascii_hexdigit()) {
                return None;
            }
            (digits, 6)
        };
        Some((char::from_u32(hex_value(digits))?, len))
    }

    fn class_exit(&self) -> bool {
        self.pos >= self.end || self.b[self.pos] == b']'
    }

    /// A loop iteration that consumed nothing (and is not between the two UTF-16 units of
    /// one character) skips a character, so that every loop ends.
    fn ensure_progress(&mut self, before: (usize, u32)) {
        if (self.pos, self.pending_low) == before && self.pos < self.end {
            self.pos += 1;
            while self.pos < self.end && !self.src.is_char_boundary(self.pos) {
                self.pos += 1;
            }
        }
    }

    /// The contents of a class without `v`, after `[` (up to its `]`).
    fn class_ranges(&mut self) {
        self.pending_low = 0;
        self.eat(b'^');
        while !self.class_exit() {
            let before = (self.pos, self.pending_low);
            let min_start = self.pos;
            let min = self.class_atom();
            if self.eat(b'-') {
                if self.class_exit() {
                    return;
                }
                if min == Atom::Empty && self.unicode {
                    self.error(min_start, self.pos - 1, m::REGEX_RANGE_BOUNDED_BY_CLASS);
                }
                let max_start = self.pos;
                let max = self.class_atom();
                if max == Atom::Empty && self.unicode {
                    self.error(max_start, self.pos, m::REGEX_RANGE_BOUNDED_BY_CLASS);
                    continue;
                }
                if min == Atom::Empty {
                    continue;
                }
                // Without `u`, a class escape as the end of a range compares as U+FFFD.
                let max = if max == Atom::Empty {
                    Atom::One(0xFFFD)
                } else {
                    max
                };
                if let (Atom::One(lo), Atom::One(hi)) = (min, max)
                    && lo > hi
                {
                    self.error(min_start, self.pos, m::REGEX_RANGE_OUT_OF_ORDER);
                }
            }
            self.ensure_progress(before);
        }
    }

    /// One element of a class without `v`.
    fn class_atom(&mut self) -> Atom {
        if !self.eat(b'\\') {
            return self.source_char();
        }
        match self.peek() {
            Some(b'b') => {
                self.pos += 1;
                Atom::One(8)
            }
            Some(b'-') => {
                self.pos += 1;
                Atom::One(u32::from(b'-'))
            }
            _ if self.class_escape() => Atom::Empty,
            _ => self.character_escape(false),
        }
    }

    /// `\d`, `\s`, `\w` (and capitals) and `\p{..}` / `\P{..}`, after `\`. False (and
    /// nothing consumed) for anything else.
    fn class_escape(&mut self) -> bool {
        let start = self.pos - 1;
        match self.peek() {
            Some(b'd' | b'D' | b's' | b'S' | b'w' | b'W') => {
                self.pos += 1;
                true
            }
            Some(c @ (b'p' | b'P')) => {
                self.pos += 1;
                if self.eat(b'{') {
                    self.property(c == b'P');
                    self.expect(b'}');
                    if !self.unicode {
                        self.error(start, self.pos, m::REGEX_PROPERTY_NEEDS_UNICODE);
                    }
                } else if self.unicode {
                    let letter = (c as char).to_string();
                    self.error_with(
                        self.pos - 2,
                        self.pos,
                        m::REGEX_PROPERTY_NEEDS_BRACES,
                        letter,
                    );
                } else {
                    // Annex B: `\p` is `p`.
                    self.pos -= 1;
                    return false;
                }
                true
            }
            _ => false,
        }
    }

    /// The inside of `\p{..}`: `Name=Value`, or a lone name or value.
    fn property(&mut self, negated: bool) {
        let name_start = self.pos;
        let name_end = self.word();
        let name = &self.src[name_start..name_end];
        if self.eat(b'=') {
            let values = match name {
                "General_Category" | "gc" => Some(props::GENERAL_CATEGORY),
                "Script" | "sc" | "Script_Extensions" | "scx" => Some(props::SCRIPT),
                _ => None,
            };
            if name.is_empty() {
                self.error(name_end, name_end, m::REGEX_PROPERTY_NAME_EXPECTED);
            } else if values.is_none() {
                self.error(name_start, name_end, m::REGEX_UNKNOWN_PROPERTY_NAME);
            }
            let value_start = self.pos;
            let value_end = self.word();
            let value = &self.src[value_start..value_end];
            if value.is_empty() {
                self.error(value_end, value_end, m::REGEX_PROPERTY_VALUE_EXPECTED);
            } else if let Some(values) = values
                && values.binary_search(&value).is_err()
            {
                self.error(value_start, value_end, m::REGEX_UNKNOWN_PROPERTY_VALUE);
            }
        } else if name.is_empty() {
            self.error(name_end, name_end, m::REGEX_PROPERTY_EXPECTED);
        } else if props::BINARY_OF_STRINGS.binary_search(&name).is_ok() {
            if !self.sets {
                self.error(name_start, name_end, m::REGEX_STRING_PROPERTY_NEEDS_SETS);
            } else if negated {
                self.error(name_start, name_end, m::REGEX_STRINGS_IN_NEGATED_CLASS);
            } else {
                self.may_strings = true;
            }
        } else if props::GENERAL_CATEGORY.binary_search(&name).is_err()
            && props::BINARY.binary_search(&name).is_err()
        {
            self.error(name_start, name_end, m::REGEX_UNKNOWN_PROPERTY);
        }
    }

    /// Skips ASCII letters, digits and `_`; returns where they end.
    fn word(&mut self) -> usize {
        while self
            .peek()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == b'_')
        {
            self.pos += 1;
        }
        self.pos
    }

    /// The contents of a class with `v`, after `[` (up to its `]`): a union, an
    /// intersection (`&&`) or a subtraction (`--`) of operands.
    fn class_set_expression(&mut self) {
        if !self.enter() {
            return;
        }
        self.class_set_expression_inner();
        self.depth -= 1;
    }

    fn class_set_expression_inner(&mut self) {
        let negated = self.eat(b'^');
        if self.class_exit() {
            return;
        }
        let first = self.char_at(self.pos).unwrap_or('\u{FFFD}');
        let mut start = self.pos;
        let mut operand = if self.double_operator() {
            self.error(self.pos, self.pos, m::REGEX_CLASS_OPERAND_EXPECTED);
            self.may_strings = false;
            Atom::Empty
        } else {
            self.class_set_operand()
        };
        let mut may_strings = false;
        match self.peek() {
            Some(b'-') if self.at(self.pos + 1) == Some(b'-') => {
                if negated && self.may_strings {
                    self.error(start, self.pos, m::REGEX_STRINGS_IN_NEGATED_CLASS);
                }
                let first_may = self.may_strings;
                self.class_set_operation(SetOp::Subtraction);
                self.may_strings = !negated && first_may;
                return;
            }
            Some(b'&') if self.at(self.pos + 1) == Some(b'&') => {
                self.class_set_operation(SetOp::Intersection);
                if negated && self.may_strings {
                    self.error(start, self.pos, m::REGEX_STRINGS_IN_NEGATED_CLASS);
                }
                self.may_strings = !negated && self.may_strings;
                return;
            }
            // The message names the class's first character, as tsc's does.
            Some(b'&') => self.unexpected(self.pos, first),
            Some(b'-') => {}
            _ => {
                if negated && self.may_strings {
                    self.error(start, self.pos, m::REGEX_STRINGS_IN_NEGATED_CLASS);
                }
                may_strings = self.may_strings;
            }
        }
        // A union of operands and ranges.
        while self.pos < self.end {
            let before = (self.pos, self.pending_low);
            match self.b[self.pos] {
                b'-' => {
                    self.pos += 1;
                    if self.class_exit() {
                        break;
                    }
                    if self.eat(b'-') {
                        self.error(self.pos - 2, self.pos, m::REGEX_MIXED_CLASS_OPERATORS);
                        start = self.pos - 2;
                        operand = Atom::Many;
                        continue;
                    }
                    if operand == Atom::Empty {
                        self.error(start, self.pos - 1, m::REGEX_RANGE_BOUNDED_BY_CLASS);
                    }
                    let second_start = self.pos;
                    let second = self.class_set_operand();
                    if negated && self.may_strings {
                        self.error(second_start, self.pos, m::REGEX_STRINGS_IN_NEGATED_CLASS);
                    }
                    may_strings |= self.may_strings;
                    if second == Atom::Empty {
                        self.error(second_start, self.pos, m::REGEX_RANGE_BOUNDED_BY_CLASS);
                    } else if let (Atom::One(lo), Atom::One(hi)) = (operand, second)
                        && lo > hi
                    {
                        self.error(start, self.pos, m::REGEX_RANGE_OUT_OF_ORDER);
                    }
                }
                b'&' => {
                    start = self.pos;
                    self.pos += 1;
                    if self.eat(b'&') {
                        self.error(self.pos - 2, self.pos, m::REGEX_MIXED_CLASS_OPERATORS);
                        if self.peek() == Some(b'&') {
                            self.unexpected(self.pos, '&');
                            self.pos += 1;
                        }
                        operand = Atom::Many;
                    } else {
                        self.unexpected(self.pos - 1, '&');
                        operand = Atom::One(u32::from(b'&'));
                    }
                    continue;
                }
                _ => {}
            }
            if self.class_exit() {
                break;
            }
            start = self.pos;
            if self.double_operator() {
                self.error(self.pos, self.pos + 2, m::REGEX_MIXED_CLASS_OPERATORS);
                self.pos += 2;
                operand = Atom::Many;
            } else {
                operand = self.class_set_operand();
            }
            self.ensure_progress(before);
        }
        self.may_strings = !negated && may_strings;
    }

    /// `--` or `&&` at `pos`.
    fn double_operator(&self) -> bool {
        matches!(
            (self.peek(), self.at(self.pos + 1)),
            (Some(b'-'), Some(b'-')) | (Some(b'&'), Some(b'&'))
        )
    }

    /// The rest of a subtraction or intersection, after its first operand.
    fn class_set_operation(&mut self, op: SetOp) {
        let mut may_strings = self.may_strings;
        while !self.class_exit() {
            let before = (self.pos, self.pending_low);
            match self.b[self.pos] {
                b'-' => {
                    self.pos += 1;
                    if self.eat(b'-') {
                        if op != SetOp::Subtraction {
                            self.error(self.pos - 2, self.pos, m::REGEX_MIXED_CLASS_OPERATORS);
                        }
                    } else {
                        self.error(self.pos - 1, self.pos, m::REGEX_MIXED_CLASS_OPERATORS);
                    }
                }
                b'&' => {
                    self.pos += 1;
                    if self.eat(b'&') {
                        if op != SetOp::Intersection {
                            self.error(self.pos - 2, self.pos, m::REGEX_MIXED_CLASS_OPERATORS);
                        }
                        if self.peek() == Some(b'&') {
                            self.unexpected(self.pos, '&');
                            self.pos += 1;
                        }
                    } else {
                        self.unexpected(self.pos - 1, '&');
                    }
                }
                _ => self.expected(match op {
                    SetOp::Subtraction => "--",
                    SetOp::Intersection => "&&",
                }),
            }
            if self.class_exit() {
                self.error(self.pos, self.pos, m::REGEX_CLASS_OPERAND_EXPECTED);
                break;
            }
            self.class_set_operand();
            if op == SetOp::Intersection {
                may_strings = may_strings && self.may_strings;
            }
            self.ensure_progress(before);
        }
        self.may_strings = may_strings;
    }

    /// A nested class, a class escape, `\q{..}` or one character.
    fn class_set_operand(&mut self) -> Atom {
        self.may_strings = false;
        match self.peek() {
            Some(b'[') => {
                self.pos += 1;
                self.class_set_expression();
                self.expect(b']');
                Atom::Empty
            }
            Some(b'\\') => {
                self.pos += 1;
                if self.class_escape() {
                    return Atom::Empty;
                }
                if self.eat(b'q') {
                    if self.eat(b'{') {
                        self.class_strings();
                        self.expect(b'}');
                        return Atom::Empty;
                    }
                    self.error(self.pos - 2, self.pos, m::REGEX_Q_NEEDS_BRACES);
                    return Atom::One(u32::from(b'q'));
                }
                self.pos -= 1;
                self.class_set_character()
            }
            _ => self.class_set_character(),
        }
    }

    /// The alternatives of `\q{..}`, after `{` (up to its `}`).
    fn class_strings(&mut self) {
        let mut length = 0;
        while let Some(c) = self.peek() {
            match c {
                b'}' => {
                    self.may_strings |= length != 1;
                    return;
                }
                b'|' => {
                    self.may_strings |= length != 1;
                    self.pos += 1;
                    length = 0;
                }
                _ => {
                    let before = (self.pos, self.pending_low);
                    self.class_set_character();
                    self.ensure_progress(before);
                    length += 1;
                }
            }
        }
    }

    /// One character in a class with `v`.
    fn class_set_character(&mut self) -> Atom {
        let Some(c) = self.peek() else {
            return Atom::Empty;
        };
        if c == b'\\' {
            self.pos += 1;
            return match self.peek() {
                Some(b'b') => {
                    self.pos += 1;
                    Atom::One(8)
                }
                Some(
                    p @ (b'&' | b'-' | b'!' | b'#' | b'%' | b',' | b':' | b';' | b'<' | b'=' | b'>'
                    | b'@' | b'`' | b'~'),
                ) => {
                    self.pos += 1;
                    Atom::One(u32::from(p))
                }
                _ => self.character_escape(false),
            };
        }
        if self.at(self.pos + 1) == Some(c) && b"&!#%*+,.:;<=>?@`~".contains(&c) {
            self.error(self.pos, self.pos + 2, m::REGEX_DOUBLE_PUNCTUATOR);
            self.pos += 2;
            return Atom::Many;
        }
        if b"/()[]{}-|".contains(&c) {
            self.unexpected(self.pos, c as char);
            self.pos += 1;
            return Atom::One(u32::from(c));
        }
        self.source_char()
    }

    /// One source character. Without `u`, an astral character is two UTF-16 units: the
    /// first is returned without moving, the second on the next call.
    fn source_char(&mut self) -> Atom {
        if self.pos >= self.end {
            return Atom::Empty;
        }
        let Some(c) = self.char_at(self.pos) else {
            self.pos += 1;
            return Atom::Empty;
        };
        if !self.unicode {
            if self.pending_low != 0 {
                self.pos += c.len_utf8();
                return Atom::One(std::mem::take(&mut self.pending_low));
            }
            if let Some(u) = (c as u32).checked_sub(0x10000) {
                self.pending_low = 0xDC00 + (u & 0x3FF);
                return Atom::One(0xD800 + (u >> 10));
            }
        }
        self.pos += c.len_utf8();
        Atom::One(c as u32)
    }
}

/// Compares two numbers written as decimal digits, of any length.
fn compare_decimal(a: &str, b: &str) -> std::cmp::Ordering {
    let a = a.trim_start_matches('0');
    let b = b.trim_start_matches('0');
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}

fn hex_value(digits: &[u8]) -> u32 {
    digits.iter().fold(0u32, |v, &d| {
        v.saturating_mul(16)
            .saturating_add((d as char).to_digit(16).unwrap_or(0))
    })
}

#[cfg(test)]
mod unit_tests {
    use super::props;

    #[test]
    fn property_tables_are_sorted() {
        for table in [
            props::GENERAL_CATEGORY,
            props::SCRIPT,
            props::BINARY,
            props::BINARY_OF_STRINGS,
        ] {
            assert!(table.windows(2).all(|w| w[0] < w[1]));
        }
    }
}
