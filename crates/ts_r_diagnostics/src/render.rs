//! tsc-compatible text output: the plain `file(line,col): error TSxxxx: msg` form and the
//! `--pretty` form with a code frame.

use std::fmt::Write as _;

use ts_r_span::{LineIndex, SourceText, Span};

use crate::{Category, Diagnostic, FileId};

/// Maps file ids to sources for rendering.
pub trait Files {
    fn source(&self, file: FileId) -> Option<&SourceText>;
}

/// A single source answers for every id.
impl Files for SourceText {
    fn source(&self, _file: FileId) -> Option<&SourceText> {
        Some(self)
    }
}

impl Files for [SourceText] {
    fn source(&self, file: FileId) -> Option<&SourceText> {
        self.get(file.0 as usize)
    }
}

impl Files for Vec<SourceText> {
    fn source(&self, file: FileId) -> Option<&SourceText> {
        self.get(file.0 as usize)
    }
}

const RESET: &str = "\x1b[0m";
const RED: &str = "\x1b[91m";
const YELLOW: &str = "\x1b[93m";
const CYAN: &str = "\x1b[96m";
const GREY: &str = "\x1b[90m";
const BLUE: &str = "\x1b[94m";
const INVERSE: &str = "\x1b[7m";

/// Rendering options.
#[derive(Clone, Copy, Debug)]
pub struct Renderer {
    /// Emit ANSI colours (only used by the pretty form).
    pub color: bool,
}

impl Default for Renderer {
    fn default() -> Self {
        Renderer { color: true }
    }
}

fn category_color(cat: Category) -> &'static str {
    match cat {
        Category::Error => RED,
        Category::Warning => YELLOW,
        Category::Suggestion => GREY,
        Category::Message => BLUE,
    }
}

/// One-based line and UTF-16 column, as tsc prints them.
fn position(src: &SourceText, offset: u32) -> (u32, u32) {
    let lc = src.line_col_utf16(offset);
    (lc.line + 1, lc.col + 1)
}

/// `file(line,col): error TS1005: ';' expected.` — tsc's `--pretty false` form.
/// Diagnostics whose file is unknown are printed without a location.
pub fn format_plain(diag: &Diagnostic, files: &(impl Files + ?Sized)) -> String {
    let mut out = String::new();
    if let Some(src) = files.source(diag.file) {
        let (line, col) = position(src, diag.span.start);
        let _ = write!(out, "{}({},{}): ", src.name(), line, col);
    }
    let _ = write!(
        out,
        "{} {}: {}",
        diag.category.as_str(),
        code_label(diag.code),
        diag.message
    );
    out
}

/// The printed form of a code: `TS2322` for tsc and ts-r codes, `TSR0201` for sound-mode
/// codes (stored as `100_000 + n`, design record 0007).
pub fn code_label(code: u32) -> String {
    if code >= crate::SOUND_CODE_BASE {
        format!("TSR{:04}", code - crate::SOUND_CODE_BASE)
    } else {
        format!("TS{code}")
    }
}

/// tsc's `--pretty` form: a header line, then the source line(s) with `~` underlining,
/// followed by related information.
pub fn format_pretty(diag: &Diagnostic, files: &(impl Files + ?Sized), r: Renderer) -> String {
    let mut out = String::new();
    let paint = |out: &mut String, color: &str, text: &str| {
        if r.color {
            let _ = write!(out, "{color}{text}{RESET}");
        } else {
            out.push_str(text);
        }
    };
    let src = files.source(diag.file);
    if let Some(src) = src {
        let (line, col) = position(src, diag.span.start);
        paint(&mut out, CYAN, src.name());
        out.push(':');
        paint(&mut out, YELLOW, &line.to_string());
        out.push(':');
        paint(&mut out, YELLOW, &col.to_string());
        out.push_str(" - ");
    }
    paint(
        &mut out,
        category_color(diag.category),
        diag.category.as_str(),
    );
    paint(&mut out, GREY, &format!(" {}: ", code_label(diag.code)));
    out.push_str(&diag.message);
    out.push('\n');
    if let Some(src) = src {
        out.push('\n');
        code_frame(
            &mut out,
            src,
            diag.span,
            "",
            category_color(diag.category),
            r.color,
        );
        out.push('\n');
    }
    // tsc 7: `  file:line:col - message`, then the code frame indented, then a blank line.
    for rel in &diag.related {
        if let Some(rsrc) = files.source(rel.file) {
            let (line, col) = position(rsrc, rel.span.start);
            out.push_str("  ");
            paint(&mut out, CYAN, rsrc.name());
            out.push(':');
            paint(&mut out, YELLOW, &line.to_string());
            out.push(':');
            paint(&mut out, YELLOW, &col.to_string());
            let _ = writeln!(out, " - {}", rel.message);
            code_frame(&mut out, rsrc, rel.span, "    ", CYAN, r.color);
            out.push('\n');
        } else {
            let _ = writeln!(out, "    {}", rel.message);
        }
    }
    // A diagnostic without a location ends with one more newline, which callers drop when
    // nothing (no code frame) follows the message.
    if src.is_none() {
        out.push('\n');
    }
    out
}

/// Prints the lines covered by `span` with an underline. Spans over more than four lines show
/// the first two and last line with `...` between, like tsc.
fn code_frame(
    out: &mut String,
    src: &SourceText,
    span: Span,
    indent: &str,
    color: &str,
    use_color: bool,
) {
    let text = src.text();
    let idx: &LineIndex = src.line_index();
    let start = src.line_col_utf16(span.start);
    let end_off = span.end.max(span.start);
    let end = src.line_col_utf16(end_off);
    let first = start.line;
    let last = end.line;
    let gutter_width = (last + 1).to_string().len();
    let mut line = first;
    while line <= last {
        if last - first >= 4 && line == first + 2 {
            gutter(out, indent, "...", gutter_width, use_color);
            out.push('\n');
            line = last;
        }
        let Some(lspan) = idx.line_span(line) else {
            break;
        };
        let raw = lspan.text(text);
        let content = raw.trim_end_matches(['\n', '\r', '\u{2028}', '\u{2029}']);
        let shown = content.replace('\t', " ");
        gutter(
            out,
            indent,
            &(line + 1).to_string(),
            gutter_width,
            use_color,
        );
        out.push(' ');
        out.push_str(&shown);
        out.push('\n');
        // Underline.
        let line_len16: u32 = content.encode_utf16().count() as u32;
        let from = if line == first { start.col } else { 0 };
        let to = if line == last { end.col } else { line_len16 };
        let width = if to > from {
            to - from
        } else if line == first {
            1
        } else {
            0
        };
        gutter(out, indent, "", gutter_width, use_color);
        out.push(' ');
        let mut marks = String::with_capacity((from + width) as usize);
        for _ in 0..from {
            marks.push(' ');
        }
        for _ in 0..width {
            marks.push('~');
        }
        if use_color {
            let _ = write!(out, "{color}{marks}{RESET}");
        } else {
            out.push_str(&marks);
        }
        out.push('\n');
        line += 1;
    }
}

fn gutter(out: &mut String, indent: &str, label: &str, width: usize, use_color: bool) {
    out.push_str(indent);
    if use_color {
        let _ = write!(out, "{INVERSE}{label:>width$}{RESET}");
    } else {
        let _ = write!(out, "{label:>width$}");
    }
}

/// tsc's closing summary line, e.g. `Found 2 errors in the same file, starting at: a.ts:3`.
/// `first` is the first error's file name and one-based line. Returns an empty string when
/// there are no errors.
pub fn format_summary(error_count: usize, file_count: usize, first: Option<(&str, u32)>) -> String {
    match (error_count, file_count, first) {
        (0, _, _) => String::new(),
        (1, _, Some((name, line))) => format!("Found 1 error in {name}:{line}"),
        (1, _, None) => "Found 1 error.".to_owned(),
        (n, 1, Some((name, line))) => {
            format!("Found {n} errors in the same file, starting at: {name}:{line}")
        }
        (n, files, _) if files > 1 => format!("Found {n} errors in {files} files."),
        (n, _, _) => format!("Found {n} errors."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Diagnostic, messages};

    #[test]
    fn sound_codes_render_as_tsr() {
        assert_eq!(code_label(2322), "TS2322");
        assert_eq!(code_label(99001), "TS99001");
        assert_eq!(code_label(100_201), "TSR0201");
        assert_eq!(code_label(101_401), "TSR1401");
        let src = SourceText::new("a.ts", "x!;\n");
        let mut d = Diagnostic::new(messages::EXPRESSION_EXPECTED, Span::new(0, 2), &[]);
        d.code = 100_301;
        assert_eq!(
            format_plain(&d, &src),
            "a.ts(1,1): error TSR0301: Expression expected."
        );
    }

    #[test]
    fn plain() {
        let src = SourceText::new("src/a.ts", "let x = ;\n");
        let d = Diagnostic::new(messages::EXPRESSION_EXPECTED, Span::new(8, 9), &[]);
        assert_eq!(
            format_plain(&d, &src),
            "src/a.ts(1,9): error TS1109: Expression expected."
        );
    }

    #[test]
    fn plain_utf16_column() {
        let src = SourceText::new("a.ts", "'😀' x");
        let d = Diagnostic::new(messages::EXPECTED, Span::new(7, 8), &[";"]);
        // The emoji is two UTF-16 units, so `x` is at column 6.
        assert_eq!(
            format_plain(&d, &src),
            "a.ts(1,6): error TS1005: ';' expected."
        );
    }

    #[test]
    fn pretty_no_color() {
        let src = SourceText::new("a.ts", "const a = 1\nlet x = = 2;\n");
        let d = Diagnostic::new(messages::EXPRESSION_EXPECTED, Span::new(20, 21), &[]);
        let out = format_pretty(&d, &src, Renderer { color: false });
        assert_eq!(
            out,
            "a.ts:2:9 - error TS1109: Expression expected.\n\n2 let x = = 2;\n          ~\n\n"
        );
    }

    #[test]
    fn pretty_color_and_related() {
        let src = SourceText::new("a.ts", "f(1;\n");
        let d = Diagnostic::new(messages::EXPECTED, Span::new(3, 4), &[")"]).with_related(
            messages::THE_PARSER_EXPECTED_TO_FIND_A_1_TO_MATCH_THE_0_TOKEN_HERE,
            FileId(0),
            Span::new(1, 2),
            &["(", ")"],
        );
        let out = format_pretty(&d, &src, Renderer::default());
        assert!(out.starts_with("\x1b[96ma.ts\x1b[0m:\x1b[93m1\x1b[0m:\x1b[93m4\x1b[0m - \x1b[91merror\x1b[0m\x1b[90m TS1005: \x1b[0m')' expected."));
        assert!(out.contains("The parser expected to find a ')' to match the '(' token here."));
        let plain = format_pretty(&d, &src, Renderer { color: false });
        assert!(
            plain.ends_with(
                "  a.ts:1:2 - The parser expected to find a ')' to match the '(' token here.\n    1 f(1;\n       ~\n\n"
            ),
            "{plain}"
        );
    }

    #[test]
    fn pretty_multiline_elides() {
        let text = "a\nb\nc\nd\ne\nf\n";
        let src = SourceText::new("m.ts", text);
        let d = Diagnostic::new(messages::UNEXPECTED_TOKEN, Span::new(0, 11), &[]);
        let out = format_pretty(&d, &src, Renderer { color: false });
        assert!(out.contains("1 a\n  ~\n2 b\n  ~\n...\n6 f\n"), "{out}");
    }

    #[test]
    fn summary() {
        assert_eq!(format_summary(0, 0, None), "");
        assert_eq!(
            format_summary(1, 1, Some(("a.ts", 3))),
            "Found 1 error in a.ts:3"
        );
        assert_eq!(
            format_summary(2, 1, Some(("a.ts", 3))),
            "Found 2 errors in the same file, starting at: a.ts:3"
        );
        assert_eq!(
            format_summary(5, 2, Some(("a.ts", 3))),
            "Found 5 errors in 2 files."
        );
    }
}
