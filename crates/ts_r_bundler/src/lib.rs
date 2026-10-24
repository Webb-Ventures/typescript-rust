//! The ts-r bundler. Design: `docs/design/0008-bundler.md`.

pub mod bundle;
pub mod chunk;
pub mod cjs_exports;
pub mod data_url;
pub mod expr;
pub mod features;
pub mod fscache;
pub mod graph;
#[cfg(feature = "hmr")]
pub mod hmr;
pub mod link;
pub mod lowering;
pub mod meta;
pub mod metafile;
pub mod options;
pub mod plan;
pub mod pool;
pub mod prepare;
pub mod print;
pub mod resolve;
pub mod runtime;
pub mod scan;
pub mod sourcemap;

pub use bundle::{BuildContext, BuildResult, BuildStats, OutputFile, build};
pub use features::{Feature, Features};
pub use options::*;

use std::fmt;

/// Message severity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Severity {
    Error,
    Warning,
}

/// An error or warning, esbuild-style (`file:line:column: text`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Message {
    pub severity: Severity,
    /// Path relative to the working directory.
    pub file: Option<String>,
    /// 1-based line (0 = no location).
    pub line: u32,
    /// 0-based column in UTF-16 code units.
    pub column: u32,
    pub text: String,
    /// The source line, for display.
    pub line_text: Option<String>,
}

impl Message {
    pub fn new(severity: Severity, file: Option<String>, text: String) -> Message {
        Message {
            severity,
            file,
            line: 0,
            column: 0,
            text,
            line_text: None,
        }
    }

    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }
}

impl fmt::Display for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self.severity {
            Severity::Error => "ERROR",
            Severity::Warning => "WARNING",
        };
        write!(f, "{kind}: {}", self.text)?;
        if let Some(file) = &self.file {
            if self.line > 0 {
                write!(f, "\n    {file}:{}:{}:", self.line, self.column)?;
                if let Some(t) = &self.line_text {
                    write!(f, "\n      {} | {t}", self.line)?;
                }
            } else {
                write!(f, "\n    {file}")?;
            }
        }
        Ok(())
    }
}

/// Byte offset -> line/column for one source text.
#[derive(Debug)]
pub struct LineIndex<'a> {
    src: &'a str,
    starts: Vec<u32>,
}

impl<'a> LineIndex<'a> {
    pub fn new(src: &'a str) -> LineIndex<'a> {
        let mut starts = vec![0];
        for (i, b) in src.bytes().enumerate() {
            if b == b'\n' {
                starts.push(i as u32 + 1);
            }
        }
        LineIndex { src, starts }
    }

    /// 1-based line, 0-based UTF-16 column.
    pub fn line_col(&self, pos: u32) -> (u32, u32) {
        let pos = pos.min(self.src.len() as u32);
        let line = self.starts.partition_point(|&s| s <= pos).saturating_sub(1);
        let start = self.starts[line] as usize;
        let mut end = pos as usize;
        while end > start && !self.src.is_char_boundary(end) {
            end -= 1;
        }
        let col = self.src[start..end].encode_utf16().count() as u32;
        (line as u32 + 1, col)
    }

    pub fn message(&self, severity: Severity, file: &str, pos: u32, text: String) -> Message {
        let (line, column) = self.line_col(pos);
        let ls = self.starts[(line - 1) as usize] as usize;
        let le = self.src[ls..].find('\n').map_or(self.src.len(), |i| ls + i);
        Message {
            severity,
            file: Some(file.to_owned()),
            line,
            column,
            text,
            line_text: Some(self.src[ls..le].trim_end_matches('\r').to_owned()),
        }
    }
}

/// Eight-character, file-name-safe hash text (base32, upper case like esbuild's names).
pub fn hash_text(hash: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut v: u64 = 0;
    for &b in hash.iter().take(5) {
        v = v << 8 | b as u64;
    }
    (0..8)
        .map(|i| ALPHABET[((v >> (35 - i * 5)) & 31) as usize] as char)
        .collect()
}
