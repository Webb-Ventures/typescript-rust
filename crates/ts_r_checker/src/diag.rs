//! Diagnostic messages from the generated tsc catalog, and message chains.

use ts_r_diagnostics::{Category, DiagnosticMessage};

pub use crate::diagnostics_generated as dg;
use crate::diagnostics_generated::DiagnosticInfo;

/// Converts a generated catalog entry to the diagnostics crate's message type.
pub const fn msg(info: DiagnosticInfo) -> DiagnosticMessage {
    DiagnosticMessage {
        code: info.code,
        category: match info.category {
            dg::Category::Error => Category::Error,
            dg::Category::Warning => Category::Warning,
            dg::Category::Suggestion => Category::Suggestion,
            dg::Category::Message => Category::Message,
        },
        text: info.message,
    }
}

/// A chain of messages: the head is the diagnostic's message, each further entry is printed
/// on its own line, indented two spaces per level (tsc's `DiagnosticMessageChain`).
#[derive(Clone, Debug, Default)]
pub struct Chain {
    /// `(code, text)` from the outermost to the innermost message.
    pub items: Vec<(u32, String)>,
    /// Explicit indentation levels `(item index, level)`; other items use their index.
    pub nested: Vec<(usize, usize)>,
}

impl Chain {
    pub fn new() -> Chain {
        Chain::default()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Adds `info(args)` as the new outermost message.
    pub fn push_front(&mut self, info: DiagnosticInfo, args: &[&str]) {
        self.items.insert(0, (info.code, msg(info).format(args)));
    }

    /// Adds `info(args)` as the new innermost message.
    pub fn push_back(&mut self, info: DiagnosticInfo, args: &[&str]) {
        self.items.push((info.code, msg(info).format(args)));
    }

    pub fn code(&self) -> u32 {
        self.items.first().map_or(0, |i| i.0)
    }

    /// The full text: head, then each further message on a new line indented by level.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for (i, (_, m)) in self.items.iter().enumerate() {
            if i > 0 {
                out.push('\n');
                let level = self.nested.iter().find(|x| x.0 == i).map_or(i, |x| x.1);
                for _ in 0..level {
                    out.push_str("  ");
                }
            }
            out.push_str(m);
        }
        out
    }
}
