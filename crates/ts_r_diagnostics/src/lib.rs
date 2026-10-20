//! Diagnostics: codes, messages, related spans, and tsc-style rendering.
//!
//! Codes follow tsc's numbering where an equivalent tsc diagnostic exists, so users can search
//! for `TS1005` and get the familiar meaning. Messages are templates with `{0}`, `{1}`, ...
//! placeholders, filled in by [`Diagnostic::new`].

mod render;

pub mod messages;

pub use render::{Files, Renderer, code_label, format_plain, format_pretty, format_summary};

/// Sound-mode diagnostics (`TSR0001`..`TSR9999`, design record 0007) are stored as
/// `SOUND_CODE_BASE + n` and printed with the `TSR` prefix by [`code_label`].
pub const SOUND_CODE_BASE: u32 = 100_000;
use ts_r_span::Span;

/// Severity of a diagnostic, as tsc names them.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum Category {
    Error,
    Warning,
    Suggestion,
    Message,
}

impl Category {
    pub fn as_str(self) -> &'static str {
        match self {
            Category::Error => "error",
            Category::Warning => "warning",
            Category::Suggestion => "suggestion",
            Category::Message => "message",
        }
    }
}

/// A static message template from the catalogue in [`messages`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DiagnosticMessage {
    pub code: u32,
    pub category: Category,
    /// Template text; `{0}`, `{1}`... are replaced by arguments.
    pub text: &'static str,
}

impl DiagnosticMessage {
    /// Fills the template with `args`. Missing arguments leave the placeholder as is.
    pub fn format(&self, args: &[&str]) -> String {
        format_template(self.text, args)
    }
}

fn format_template(text: &str, args: &[&str]) -> String {
    if args.is_empty() || !text.contains('{') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len() + 16);
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let close = after.find('}');
        let index = close.and_then(|c| after[..c].parse::<usize>().ok());
        match (close, index) {
            (Some(c), Some(i)) if i < args.len() => {
                out.push_str(args[i]);
                rest = &after[c + 1..];
            }
            _ => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Identifies a source file for rendering. The meaning of the number is up to the caller's
/// [`Files`] implementation; single-file callers use `FileId(0)`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, PartialOrd, Ord)]
pub struct FileId(pub u32);

/// Extra location attached to a diagnostic ("'x' was declared here").
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RelatedInfo {
    pub code: u32,
    pub category: Category,
    pub message: String,
    pub file: FileId,
    pub span: Span,
}

/// One problem found in a source file.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Diagnostic {
    pub code: u32,
    pub category: Category,
    pub message: String,
    pub file: FileId,
    pub span: Span,
    pub related: Vec<RelatedInfo>,
}

impl Diagnostic {
    /// Builds a diagnostic from a catalogue message and its arguments.
    pub fn new(msg: DiagnosticMessage, span: Span, args: &[&str]) -> Diagnostic {
        Diagnostic {
            code: msg.code,
            category: msg.category,
            message: msg.format(args),
            file: FileId(0),
            span,
            related: Vec::new(),
        }
    }

    pub fn with_file(mut self, file: FileId) -> Diagnostic {
        self.file = file;
        self
    }

    pub fn with_related(
        mut self,
        msg: DiagnosticMessage,
        file: FileId,
        span: Span,
        args: &[&str],
    ) -> Diagnostic {
        self.related.push(RelatedInfo {
            code: msg.code,
            category: msg.category,
            message: msg.format(args),
            file,
            span,
        });
        self
    }

    pub fn is_error(&self) -> bool {
        self.category == Category::Error
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_formatting() {
        assert_eq!(messages::EXPECTED.format(&[";"]), "';' expected.");
        assert_eq!(format_template("{0} and {1}", &["a", "b"]), "a and b");
        assert_eq!(format_template("{1} {0} {2}", &["a", "b"]), "b a {2}");
        assert_eq!(format_template("use {} {x}", &["a"]), "use {} {x}");
        assert_eq!(format_template("no args {0}", &[]), "no args {0}");
    }

    #[test]
    fn builder() {
        let d = Diagnostic::new(messages::EXPECTED, Span::new(1, 2), &[")"]).with_related(
            messages::THE_PARSER_EXPECTED_TO_FIND_A_1_TO_MATCH_THE_0_TOKEN_HERE,
            FileId(0),
            Span::new(0, 1),
            &["(", ")"],
        );
        assert_eq!(d.code, 1005);
        assert!(d.is_error());
        assert_eq!(
            d.related[0].message,
            "The parser expected to find a ')' to match the '(' token here."
        );
    }
}
