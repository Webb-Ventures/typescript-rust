//! Lint diagnostics, fixes and suggestions.

use ts_r_span::Span;

/// Rule severity, as in ESLint (`0`/`"off"`, `1`/`"warn"`, `2`/`"error"`).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub enum Severity {
    #[default]
    Off = 0,
    Warn = 1,
    Error = 2,
}

impl Severity {
    /// Parses `"off" | "warn" | "error" | 0 | 1 | 2` (and the oxlint/Biome spellings `"deny"`,
    /// `"allow"`, `"info"`).
    pub fn from_json(v: &serde_json::Value) -> Option<Severity> {
        match v {
            serde_json::Value::Number(n) => match n.as_u64()? {
                0 => Some(Severity::Off),
                1 => Some(Severity::Warn),
                2 => Some(Severity::Error),
                _ => None,
            },
            serde_json::Value::String(s) => Severity::parse(s),
            _ => None,
        }
    }

    pub fn parse(s: &str) -> Option<Severity> {
        match s.trim() {
            "off" | "0" | "allow" => Some(Severity::Off),
            "warn" | "1" | "warning" | "info" => Some(Severity::Warn),
            "error" | "2" | "deny" | "on" => Some(Severity::Error),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Off => "off",
            Severity::Warn => "warning",
            Severity::Error => "error",
        }
    }
}

/// One text replacement. `span` is a UTF-8 byte range of the original source.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Edit {
    pub span: Span,
    pub text: String,
}

/// A fix: one or more non-overlapping edits applied together. Like ESLint, a fix made of
/// several edits is treated as one replacement of the range that covers them all.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Fix {
    pub edits: Vec<Edit>,
}

impl Fix {
    pub fn replace(span: Span, text: impl Into<String>) -> Fix {
        Fix {
            edits: vec![Edit {
                span,
                text: text.into(),
            }],
        }
    }

    pub fn delete(span: Span) -> Fix {
        Fix::replace(span, "")
    }

    pub fn insert(pos: u32, text: impl Into<String>) -> Fix {
        Fix::replace(Span::at(pos), text)
    }

    /// Adds an edit; keeps edits sorted.
    pub fn and(mut self, span: Span, text: impl Into<String>) -> Fix {
        self.edits.push(Edit {
            span,
            text: text.into(),
        });
        self.edits.sort_by_key(|e| (e.span.start, e.span.end));
        self
    }

    /// The range covering every edit.
    pub fn range(&self) -> Span {
        let start = self.edits.iter().map(|e| e.span.start).min().unwrap_or(0);
        let end = self.edits.iter().map(|e| e.span.end).max().unwrap_or(0);
        Span::new(start, end)
    }

    /// The fix as a single replacement of [`Fix::range`] (ESLint's `{ range, text }` form).
    pub fn merged(&self, src: &str) -> Edit {
        let range = self.range();
        let mut text = String::new();
        let mut pos = range.start;
        for e in &self.edits {
            if e.span.start < pos {
                continue; // overlapping edits inside one fix: keep the first
            }
            text.push_str(src.get(pos as usize..e.span.start as usize).unwrap_or(""));
            text.push_str(&e.text);
            pos = e.span.end;
        }
        Edit { span: range, text }
    }
}

/// An optional fix the user can pick (not applied by `--fix`).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Suggestion {
    pub message_id: Option<&'static str>,
    pub desc: String,
    pub fix: Fix,
}

/// One reported problem.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Diagnostic {
    /// Rule name as configured (`no-debugger`, `@typescript-eslint/no-explicit-any`), or `None`
    /// for engine messages (parse errors, unused disable directives).
    pub rule: Option<&'static str>,
    pub severity: Severity,
    pub span: Span,
    pub message: String,
    pub message_id: Option<&'static str>,
    pub fix: Option<Fix>,
    pub suggestions: Vec<Suggestion>,
    /// A parse error (ESLint's `fatal: true`).
    pub fatal: bool,
}

impl Diagnostic {
    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }
}
