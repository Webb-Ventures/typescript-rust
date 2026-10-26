//! `unicorn/no-manually-wrapped-comments`: prose broken over several comment lines by hand.
//!
//! Two shapes are reported, each fixed by joining the lines with single spaces:
//! - a run of standalone line comments (alone on their lines, same start column, on
//!   consecutive lines), where each comment but the last does not end a sentence (`.`, `!`,
//!   `?`, `:` or an emoji), every comment is prose, and the run is set apart: a blank line (or
//!   the file edge, or a line comment ending a sentence) before it, and a blank line after it
//!   (or a line comment, when the run itself ends a sentence). Reported at the first comment.
//! - inside a standalone multi-line block comment (no copyright, `SPDX-` or `@license` line),
//!   a run of prose lines (leading `*` stripped) up to the first line ending a sentence, set
//!   apart by a blank line, the comment edge or a sentence end before, and by a blank line or
//!   the comment edge after (or ending a sentence itself). Reported over the run.
//!
//! Not prose: empty text, tool directives (`eslint`, `global`, `@ts-check`, `c8 ignore`,
//! `prettier-ignore`, `cspell:`, ...), `TODO:`-style annotations, licence and modeline
//! headers, region markers, list items, separators, URLs and anything with code characters.

use super::prelude::*;
use ts_r_ast::{Comment, CommentKind};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-manually-wrapped-comments",
        "Disallow manually wrapped comments.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<NoManuallyWrappedComments>,
};

#[derive(Debug, Default)]
struct NoManuallyWrappedComments;

const MESSAGE: &str = "Avoid manually wrapping comments.";

/// Whitespace as JavaScript's `trim` and `\s` see it.
fn is_js_space(c: char) -> bool {
    (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}'
}

fn js_trim(s: &str) -> &str {
    s.trim_matches(is_js_space)
}

fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// A word boundary at the start of `rest`, which follows a word character.
fn boundary(rest: &str) -> bool {
    !rest.chars().next().is_some_and(is_word)
}

fn starts_space_or_end(rest: &str) -> bool {
    rest.chars().next().is_none_or(is_js_space)
}

fn is_directive(text: &str) -> bool {
    if text.starts_with(['#', '/', '@']) {
        return true;
    }
    if let Some(rest) = text.strip_prefix("eslint") {
        return starts_space_or_end(rest) || rest.starts_with('-');
    }
    if let Some(rest) = text.strip_prefix("global") {
        return boundary(rest) || rest.strip_prefix('s').is_some_and(boundary);
    }
    if let Some(rest) = text.strip_prefix("exported") {
        return boundary(rest);
    }
    if text == "no default" {
        return true;
    }
    if let Some(rest) = text.strip_prefix("noinspection") {
        return boundary(rest);
    }
    if let Some(rest) = text.strip_prefix("yamllint") {
        return starts_space_or_end(rest);
    }
    for tool in ["c8", "istanbul", "v8"] {
        if let Some(rest) = text.strip_prefix(tool) {
            let after = rest.trim_start_matches(is_js_space);
            if after.len() < rest.len()
                && let Some(rest) = after.strip_prefix("ignore")
                && boundary(rest)
            {
                return true;
            }
        }
    }
    [
        "biome-",
        "deno-",
        "dprint-",
        "oxlint-",
        "prettier-",
        "cspell:",
        "spell-checker:",
    ]
    .iter()
    .any(|p| text.starts_with(p))
}

/// `TODO:`, `FIXME(name):`: two or more capitals, an optional parenthesised tag, a colon.
fn is_annotation(text: &str) -> bool {
    let run = text.bytes().take_while(u8::is_ascii_uppercase).count();
    if run < 2 {
        return false;
    }
    let rest = &text[run..];
    if rest.starts_with(':') {
        return true;
    }
    let Some(inner) = rest.strip_prefix('(') else {
        return false;
    };
    inner
        .find(')')
        .is_some_and(|close| close > 0 && inner[close + 1..].starts_with(':'))
}

fn is_copyright(text: &str) -> bool {
    text.starts_with('©')
        || text
            .get(..9)
            .is_some_and(|p| p.eq_ignore_ascii_case("copyright") && boundary(&text[9..]))
}

fn is_spdx(text: &str) -> bool {
    text.starts_with("SPDX-")
}

fn is_structured(text: &str) -> bool {
    if let Some(rest) = text.strip_prefix("language=") {
        return rest.chars().next().is_some_and(|c| !is_js_space(c));
    }
    let region = text.strip_prefix("end").unwrap_or(text);
    if let Some(rest) = region.strip_prefix("region")
        && starts_space_or_end(rest)
    {
        return true;
    }
    let fold = text.strip_prefix("</").or_else(|| text.strip_prefix('<'));
    fold.and_then(|f| f.strip_prefix("editor-fold"))
        .is_some_and(boundary)
}

fn is_list_item(text: &str) -> bool {
    if let Some(rest) = text.strip_prefix(['*', '+', '-']) {
        return rest.chars().next().is_some_and(is_js_space);
    }
    let digits = text.bytes().take_while(u8::is_ascii_digit).count();
    digits > 0
        && text[digits..]
            .strip_prefix(['.', ')'])
            .is_some_and(|rest| rest.chars().next().is_some_and(is_js_space))
}

fn is_separator(text: &str) -> bool {
    text.len() >= 3 && text.bytes().all(|b| b"#*-=_~".contains(&b))
}

fn has_url(text: &str) -> bool {
    if text.contains("www.") {
        return true;
    }
    text.match_indices("http").any(|(i, _)| {
        let rest = &text[i + 4..];
        let rest = rest.strip_prefix('s').unwrap_or(rest);
        rest.starts_with("://") && !text[..i].chars().next_back().is_some_and(is_word)
    })
}

fn is_ignored(text: &str) -> bool {
    is_directive(text)
        || is_annotation(text)
        || is_spdx(text)
        || is_copyright(text)
        || text.starts_with("vi:")
        || text.starts_with("vim:")
        || text.starts_with("-*-")
        || is_structured(text)
        || is_list_item(text)
        || is_separator(text)
        || has_url(text)
        || text.contains(['`', '{', '}', '(', ')', '[', ']'])
}

fn is_prose(text: &str) -> bool {
    !text.is_empty() && !is_ignored(text)
}

fn in_ranges(c: char, ranges: &[(u32, u32)]) -> bool {
    let c = c as u32;
    let i = ranges.partition_point(|&(_, hi)| hi < c);
    ranges.get(i).is_some_and(|&(lo, _)| lo <= c)
}

fn is_pictographic(c: char) -> bool {
    in_ranges(c, EXTENDED_PICTOGRAPHIC)
}

fn is_variation_selector(c: char) -> bool {
    in_ranges(c, VARIATION_SELECTOR)
}

/// The text ends a sentence: `!`, `.`, `:`, `?` or a pictograph (optionally followed by a
/// variation selector), or an emoji sequence ending in a skin tone, a flag, a keycap or a tag.
fn ends_sentence(text: &str) -> bool {
    let mut rev = text.chars().rev();
    let Some(last) = rev.next() else {
        return false;
    };
    let ends = |c: char| matches!(c, '!' | '.' | ':' | '?') || is_pictographic(c);
    if ends(last) {
        return true;
    }
    let before = rev.next();
    match last {
        c if is_variation_selector(c) => before.is_some_and(ends),
        '\u{1F3FB}'..='\u{1F3FF}' | '\u{E007F}' => true,
        '\u{1F1E6}'..='\u{1F1FF}' => {
            // A flag is a pair of regional indicators; count the run to pair them.
            let run = 1 + text
                .chars()
                .rev()
                .skip(1)
                .take_while(|c| matches!(c, '\u{1F1E6}'..='\u{1F1FF}'))
                .count();
            run % 2 == 0
        }
        '\u{20E3}' => {
            before == Some('\u{FE0F}')
                && rev
                    .next()
                    .is_some_and(|c| c.is_ascii_digit() || c == '#' || c == '*')
        }
        _ => false,
    }
}

/// The file's lines as ESLint splits them (at `\r\n`, `\r`, `\n`, U+2028 and U+2029).
struct Lines<'a> {
    src: &'a str,
    /// `(start, end)` byte offsets of each line's content.
    lines: Vec<(u32, u32)>,
}

impl<'a> Lines<'a> {
    fn new(src: &'a str) -> Self {
        let mut lines = Vec::new();
        let mut start = 0usize;
        let mut iter = src.char_indices().peekable();
        while let Some((i, c)) = iter.next() {
            let len = match c {
                '\r' if iter.peek().is_some_and(|&(_, n)| n == '\n') => {
                    iter.next();
                    2
                }
                '\r' | '\n' | '\u{2028}' | '\u{2029}' => c.len_utf8(),
                _ => continue,
            };
            lines.push((start as u32, i as u32));
            start = i + len;
        }
        lines.push((start as u32, src.len() as u32));
        Lines { src, lines }
    }

    /// The zero-based line holding `pos`.
    fn line_of(&self, pos: u32) -> usize {
        self.lines
            .partition_point(|&(start, _)| start <= pos)
            .saturating_sub(1)
    }

    /// The zero-based line's text, `None` past either end.
    fn text(&self, line: isize) -> Option<&'a str> {
        let &(start, end) = self.lines.get(usize::try_from(line).ok()?)?;
        self.src.get(start as usize..end as usize)
    }

    fn is_blank(&self, line: isize) -> bool {
        self.text(line).is_none_or(|t| js_trim(t).is_empty())
    }

    /// The UTF-16 column of `pos`.
    fn column(&self, pos: u32) -> usize {
        let start = self.lines[self.line_of(pos)].0;
        self.src
            .get(start as usize..pos as usize)
            .map_or(0, |s| s.encode_utf16().count())
    }
}

/// One comment as the rule sees it.
struct Note<'a> {
    span: Span,
    kind: CommentKind,
    /// The text between the delimiters.
    value: &'a str,
    start_line: usize,
    end_line: usize,
    column: usize,
}

impl Note<'_> {
    fn text(&self) -> &str {
        js_trim(self.value)
    }

    fn is_line(&self) -> bool {
        matches!(self.kind, CommentKind::Line | CommentKind::Hashbang)
    }

    fn prefix(&self) -> &'static str {
        if self.kind == CommentKind::Hashbang {
            "#"
        } else {
            "//"
        }
    }
}

fn note<'a>(src: &'a str, lines: &Lines<'_>, c: &Comment) -> Note<'a> {
    let raw = c.span.text(src);
    let value = match c.kind {
        CommentKind::Line => raw.strip_prefix("//").unwrap_or(raw),
        CommentKind::Hashbang => raw.strip_prefix("#!").unwrap_or(raw),
        CommentKind::Block => {
            let v = raw.strip_prefix("/*").unwrap_or(raw);
            v.strip_suffix("*/").unwrap_or(v)
        }
    };
    Note {
        span: c.span,
        kind: c.kind,
        value,
        start_line: lines.line_of(c.span.start),
        end_line: lines.line_of(c.span.end),
        column: lines.column(c.span.start),
    }
}

/// Only whitespace between the start of the comment's line (the last `\n`) and the comment.
fn starts_line(src: &str, pos: u32) -> bool {
    let before = src.get(..pos as usize).unwrap_or("");
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    js_trim(&before[line_start..]).is_empty()
}

/// The text of a line comment on `line` written with `prefix`, if the line is one.
fn line_comment_text<'a>(line: Option<&'a str>, prefix: &str) -> Option<&'a str> {
    js_trim(line?).strip_prefix(prefix).map(js_trim)
}

fn separated(lines: &Lines<'_>, first: &Note<'_>, last: &Note<'_>) -> bool {
    let before = first.start_line as isize - 1;
    let before_ok = lines.is_blank(before)
        || line_comment_text(lines.text(before), first.prefix()).is_some_and(ends_sentence);
    let after = last.end_line as isize + 1;
    before_ok
        && (lines.is_blank(after)
            || (line_comment_text(lines.text(after), last.prefix()).is_some()
                && ends_sentence(last.text())))
}

impl NoManuallyWrappedComments {
    fn standalone_line(&self, src: &str, n: &Note<'_>) -> bool {
        n.is_line() && is_prose(n.text()) && starts_line(src, n.span.start)
    }

    fn block(&self, cx: &mut Ctx<'_, '_>, n: &Note<'_>) {
        // `(text, start, end)` of each line, without surrounding space or a leading `*`.
        let mut rows: Vec<(&str, u32, u32)> = Vec::new();
        let mut offset = n.span.start + 2;
        for line in n.value.split('\n') {
            let mut content = line.trim_start_matches(is_js_space);
            if let Some(rest) = content.strip_prefix('*') {
                content = rest.trim_start_matches(is_js_space);
            }
            let text = content.trim_end_matches(is_js_space);
            let start = offset + (line.len() - content.len()) as u32;
            rows.push((text, start, start + text.len() as u32));
            offset += line.len() as u32 + 1;
        }
        if rows
            .iter()
            .any(|&(t, _, _)| is_copyright(t) || is_spdx(t) || t.starts_with("@license"))
        {
            return;
        }
        let mut i = 0;
        while i < rows.len() {
            if !is_prose(rows[i].0) {
                i += 1;
                continue;
            }
            let mut len = 1;
            while i + len < rows.len()
                && !ends_sentence(rows[i + len - 1].0)
                && is_prose(rows[i + len].0)
            {
                len += 1;
            }
            if len == 1 {
                i += 1;
                continue;
            }
            let group = &rows[i..i + len];
            let before = i.checked_sub(1).map(|p| rows[p].0);
            let after = rows.get(i + len).map(|r| r.0);
            i += len;
            let last = group[len - 1];
            let before_ok = before.is_none_or(|t| t.is_empty() || ends_sentence(t));
            let after_ok = after.is_none_or(str::is_empty) || ends_sentence(last.0);
            if !before_ok || !after_ok {
                continue;
            }
            let text = group.iter().map(|r| r.0).collect::<Vec<_>>().join(" ");
            let span = Span::new(group[0].1, last.2);
            cx.report(span, MESSAGE)
                .id("no-manually-wrapped-comments")
                .fix(Fix::replace(span, text));
        }
    }
}

impl Rule for NoManuallyWrappedComments {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        if ast.comments.is_empty() {
            return;
        }
        let src = cx.src();
        let lines = Lines::new(src);
        let notes: Vec<Note<'_>> = ast.comments.iter().map(|c| note(src, &lines, c)).collect();
        let mut i = 0;
        while i < notes.len() {
            let n = &notes[i];
            if n.kind == CommentKind::Block
                && n.start_line != n.end_line
                && starts_line(src, n.span.start)
            {
                self.block(cx, n);
                i += 1;
                continue;
            }
            let mut len = 1;
            while i + len < notes.len() {
                let (a, b) = (&notes[i + len - 1], &notes[i + len]);
                if !(self.standalone_line(src, a)
                    && self.standalone_line(src, b)
                    && a.end_line + 1 == b.start_line
                    && a.column == b.column
                    && !ends_sentence(a.text()))
                {
                    break;
                }
                len += 1;
            }
            if len == 1 {
                i += 1;
                continue;
            }
            let group = &notes[i..i + len];
            i += len;
            let (first, last) = (&group[0], &group[len - 1]);
            if !separated(&lines, first, last) {
                continue;
            }
            let text = group.iter().map(Note::text).collect::<Vec<_>>().join(" ");
            let span = Span::new(first.span.start, last.span.end);
            cx.report(first.span, MESSAGE)
                .id("no-manually-wrapped-comments")
                .fix(Fix::replace(span, format!("{} {text}", first.prefix())));
        }
    }
}

const VARIATION_SELECTOR: &[(u32, u32)] = &[
    (0x180B, 0x180D),
    (0x180F, 0x180F),
    (0xFE00, 0xFE0F),
    (0xE0100, 0xE01EF),
];

/// Unicode 17 `Extended_Pictographic`, the version Node 24 matches against.
const EXTENDED_PICTOGRAPHIC: &[(u32, u32)] = &[
    (0xA9, 0xA9),
    (0xAE, 0xAE),
    (0x203C, 0x203C),
    (0x2049, 0x2049),
    (0x2122, 0x2122),
    (0x2139, 0x2139),
    (0x2194, 0x2199),
    (0x21A9, 0x21AA),
    (0x231A, 0x231B),
    (0x2328, 0x2328),
    (0x23CF, 0x23CF),
    (0x23E9, 0x23F3),
    (0x23F8, 0x23FA),
    (0x24C2, 0x24C2),
    (0x25AA, 0x25AB),
    (0x25B6, 0x25B6),
    (0x25C0, 0x25C0),
    (0x25FB, 0x25FE),
    (0x2600, 0x2604),
    (0x260E, 0x260E),
    (0x2611, 0x2611),
    (0x2614, 0x2615),
    (0x2618, 0x2618),
    (0x261D, 0x261D),
    (0x2620, 0x2620),
    (0x2622, 0x2623),
    (0x2626, 0x2626),
    (0x262A, 0x262A),
    (0x262E, 0x262F),
    (0x2638, 0x263A),
    (0x2640, 0x2640),
    (0x2642, 0x2642),
    (0x2648, 0x2653),
    (0x265F, 0x2660),
    (0x2663, 0x2663),
    (0x2665, 0x2666),
    (0x2668, 0x2668),
    (0x267B, 0x267B),
    (0x267E, 0x267F),
    (0x2692, 0x2697),
    (0x2699, 0x2699),
    (0x269B, 0x269C),
    (0x26A0, 0x26A1),
    (0x26A7, 0x26A7),
    (0x26AA, 0x26AB),
    (0x26B0, 0x26B1),
    (0x26BD, 0x26BE),
    (0x26C4, 0x26C5),
    (0x26C8, 0x26C8),
    (0x26CE, 0x26CF),
    (0x26D1, 0x26D1),
    (0x26D3, 0x26D4),
    (0x26E9, 0x26EA),
    (0x26F0, 0x26F5),
    (0x26F7, 0x26FA),
    (0x26FD, 0x26FD),
    (0x2702, 0x2702),
    (0x2705, 0x2705),
    (0x2708, 0x270D),
    (0x270F, 0x270F),
    (0x2712, 0x2712),
    (0x2714, 0x2714),
    (0x2716, 0x2716),
    (0x271D, 0x271D),
    (0x2721, 0x2721),
    (0x2728, 0x2728),
    (0x2733, 0x2734),
    (0x2744, 0x2744),
    (0x2747, 0x2747),
    (0x274C, 0x274C),
    (0x274E, 0x274E),
    (0x2753, 0x2755),
    (0x2757, 0x2757),
    (0x2763, 0x2764),
    (0x2795, 0x2797),
    (0x27A1, 0x27A1),
    (0x27B0, 0x27B0),
    (0x27BF, 0x27BF),
    (0x2934, 0x2935),
    (0x2B05, 0x2B07),
    (0x2B1B, 0x2B1C),
    (0x2B50, 0x2B50),
    (0x2B55, 0x2B55),
    (0x3030, 0x3030),
    (0x303D, 0x303D),
    (0x3297, 0x3297),
    (0x3299, 0x3299),
    (0x1F004, 0x1F004),
    (0x1F02C, 0x1F02F),
    (0x1F094, 0x1F09F),
    (0x1F0AF, 0x1F0B0),
    (0x1F0C0, 0x1F0C0),
    (0x1F0CF, 0x1F0D0),
    (0x1F0F6, 0x1F0FF),
    (0x1F170, 0x1F171),
    (0x1F17E, 0x1F17F),
    (0x1F18E, 0x1F18E),
    (0x1F191, 0x1F19A),
    (0x1F1AE, 0x1F1E5),
    (0x1F201, 0x1F20F),
    (0x1F21A, 0x1F21A),
    (0x1F22F, 0x1F22F),
    (0x1F232, 0x1F23A),
    (0x1F23C, 0x1F23F),
    (0x1F249, 0x1F25F),
    (0x1F266, 0x1F321),
    (0x1F324, 0x1F393),
    (0x1F396, 0x1F397),
    (0x1F399, 0x1F39B),
    (0x1F39E, 0x1F3F0),
    (0x1F3F3, 0x1F3F5),
    (0x1F3F7, 0x1F3FA),
    (0x1F400, 0x1F4FD),
    (0x1F4FF, 0x1F53D),
    (0x1F549, 0x1F54E),
    (0x1F550, 0x1F567),
    (0x1F56F, 0x1F570),
    (0x1F573, 0x1F57A),
    (0x1F587, 0x1F587),
    (0x1F58A, 0x1F58D),
    (0x1F590, 0x1F590),
    (0x1F595, 0x1F596),
    (0x1F5A4, 0x1F5A5),
    (0x1F5A8, 0x1F5A8),
    (0x1F5B1, 0x1F5B2),
    (0x1F5BC, 0x1F5BC),
    (0x1F5C2, 0x1F5C4),
    (0x1F5D1, 0x1F5D3),
    (0x1F5DC, 0x1F5DE),
    (0x1F5E1, 0x1F5E1),
    (0x1F5E3, 0x1F5E3),
    (0x1F5E8, 0x1F5E8),
    (0x1F5EF, 0x1F5EF),
    (0x1F5F3, 0x1F5F3),
    (0x1F5FA, 0x1F64F),
    (0x1F680, 0x1F6C5),
    (0x1F6CB, 0x1F6D2),
    (0x1F6D5, 0x1F6E5),
    (0x1F6E9, 0x1F6E9),
    (0x1F6EB, 0x1F6F0),
    (0x1F6F3, 0x1F6FF),
    (0x1F7DA, 0x1F7FF),
    (0x1F80C, 0x1F80F),
    (0x1F848, 0x1F84F),
    (0x1F85A, 0x1F85F),
    (0x1F888, 0x1F88F),
    (0x1F8AE, 0x1F8AF),
    (0x1F8BC, 0x1F8BF),
    (0x1F8C2, 0x1F8CF),
    (0x1F8D9, 0x1F8FF),
    (0x1F90C, 0x1F93A),
    (0x1F93C, 0x1F945),
    (0x1F947, 0x1F9FF),
    (0x1FA58, 0x1FA5F),
    (0x1FA6E, 0x1FAFF),
    (0x1FC00, 0x1FFFD),
];
