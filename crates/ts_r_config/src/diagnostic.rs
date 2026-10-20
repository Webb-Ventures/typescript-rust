//! Config diagnostics.
//!
//! These are a stop-gap until `ts_r_diagnostics` lands: a tsc code, a message, an optional
//! file and an optional UTF-8 byte span. Line/column conversion happens only at the edges
//! ([`line_col`]).

use std::fmt;

/// Severity of a [`ConfigDiagnostic`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    /// An error (tsc reports these as `error TSxxxx`).
    Error,
    /// A warning or suggestion.
    Warning,
}

/// A diagnostic produced while reading configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigDiagnostic {
    /// tsc diagnostic code (`5023` for "Unknown compiler option").
    pub code: u32,
    /// Fully formatted message text.
    pub message: String,
    /// Absolute path of the file the diagnostic points into, if any.
    pub file: Option<String>,
    /// Byte range `[start, end)` inside `file`.
    pub span: Option<(u32, u32)>,
    /// Severity.
    pub severity: Severity,
    /// Extra lines tsc prints under the message (for example the `paths` hint for `baseUrl`).
    pub related: Vec<String>,
}

impl ConfigDiagnostic {
    /// An error with a file location.
    pub fn at(code: u32, message: impl Into<String>, file: &str, span: (u32, u32)) -> Self {
        Self {
            code,
            message: message.into(),
            file: Some(file.to_owned()),
            span: Some(span),
            severity: Severity::Error,
            related: Vec::new(),
        }
    }

    /// An error with no location.
    pub fn global(code: u32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            file: None,
            span: None,
            severity: Severity::Error,
            related: Vec::new(),
        }
    }

    /// Adds an extra message line.
    pub fn with_related(mut self, line: impl Into<String>) -> Self {
        self.related.push(line.into());
        self
    }

    /// Renders like tsc's non-pretty output, e.g.
    /// `tsconfig.json(3,5): error TS5023: Unknown compiler option 'x'.`
    /// `display_path` maps an absolute path to how it should be shown; `text` provides file
    /// contents for line/column conversion.
    pub fn render(
        &self,
        display_path: impl Fn(&str) -> String,
        text: impl Fn(&str) -> Option<String>,
    ) -> String {
        let sev = match self.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        let mut out = match (&self.file, self.span) {
            (Some(f), Some((start, _))) => {
                let (line, col) = text(f).map(|t| line_col(&t, start)).unwrap_or((0, 0));
                format!(
                    "{}({},{}): {sev} TS{}: {}",
                    display_path(f),
                    line + 1,
                    col + 1,
                    self.code,
                    self.message
                )
            }
            _ => format!("{sev} TS{}: {}", self.code, self.message),
        };
        for r in &self.related {
            out.push_str("\n  ");
            out.push_str(r);
        }
        out
    }
}

impl fmt::Display for ConfigDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TS{}: {}", self.code, self.message)
    }
}

/// Zero-based (line, UTF-16 column) for a byte offset, matching tsc's positions.
pub fn line_col(text: &str, offset: u32) -> (u32, u32) {
    let offset = (offset as usize).min(text.len());
    let mut line = 0u32;
    let mut line_start = 0usize;
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < offset {
        match bytes[i] {
            b'\n' => {
                line += 1;
                line_start = i + 1;
            }
            b'\r' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'\n' && i + 1 < offset {
                    i += 1;
                }
                line += 1;
                line_start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    let mut start = line_start;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    let mut end = offset;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let col = if start <= end {
        text[start..end].encode_utf16().count() as u32
    } else {
        0
    };
    (line, col)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions() {
        assert_eq!(line_col("ab\ncd", 4), (1, 1));
        assert_eq!(line_col("ab\r\ncd", 4), (1, 0));
        assert_eq!(line_col("é\nx", 3), (1, 0));
        assert_eq!(line_col("éx", 2), (0, 1));
    }
}
