//! Root files named in `files` that do not exist (observed in tsc 7.0.2's
//! `tsc/commandLine/*file-in-tsconfig-does-not-exist` and `tsbuild/sample/reports-error-if-
//! input-file-is-missing` baselines):
//!
//! - a name with a TypeScript extension: TS6053 "File '<abs>' not found.";
//! - a name without one: TS6231 "Could not resolve the path '<abs>' with the extensions: ...",
//!   unless a file with one of those extensions exists;
//! - both are global errors with the chain "The file is in the program because: / Part of
//!   'files' list in <config name>" and, when the list is in the project's own config, related
//!   information at the entry: "File is matched by 'files' list specified here.".
//! - files named on the command line: TS6053 with the name as written and the chain "Root
//!   file specified for compilation" (`tsc/commandLine/Parse-enum-type-options`).

use ts_r_config::{FileSystem, ParsedConfig, path};
use ts_r_diagnostics::{Category, Diagnostic, FileId, RelatedInfo};
use ts_r_span::Span;

/// Extensions tried for an extensionless `files` entry, in tsc's order.
const TS_EXTENSIONS: &[&str] = &[".ts", ".tsx", ".d.ts", ".cts", ".d.cts", ".mts", ".d.mts"];
const JS_EXTENSIONS: &[&str] = &[".js", ".jsx", ".cjs", ".mjs"];

/// Diagnostics for `files` entries of `config` that name no file. `file_id` gives the id of
/// a file for related information (the caller adds it to its sources).
pub fn missing_root_diagnostics(
    config: &ParsedConfig,
    fs: &dyn FileSystem,
    file_id: &mut dyn FnMut(&str) -> FileId,
) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    let Some(files) = &config.files else {
        return out;
    };
    if config.command_line {
        for spec in &files.specs {
            let abs = path::resolve(&files.base_dir, spec);
            if !fs.file_exists(&abs) {
                out.push(Diagnostic {
                    code: 6053,
                    category: Category::Error,
                    message: format!(
                        "File '{spec}' not found.\n  The file is in the program because:\n    Root file specified for compilation"
                    ),
                    file: FileId(u32::MAX),
                    span: Span::EMPTY,
                    related: Vec::new(),
                });
            }
        }
        return out;
    }
    let cs = config.case_sensitive();
    let own = path::eq(&files.file, &config.config_path, cs);
    let text = if own {
        fs.read_file(&config.config_path)
    } else {
        None
    };
    let entries = text
        .as_deref()
        .map(|t| string_literals(t, files.span))
        .unwrap_or_default();
    let allow_js = config.options.effective().allow_js();
    let name = path::file_name(&config.config_path);
    for spec in &files.specs {
        let abs = path::resolve(&files.base_dir, spec);
        let lower = abs.to_ascii_lowercase();
        let known = TS_EXTENSIONS
            .iter()
            .chain(if allow_js { JS_EXTENSIONS } else { &[] })
            .any(|e| lower.ends_with(e))
            || lower.ends_with(".json");
        let message = if known {
            if fs.file_exists(&abs) {
                continue;
            }
            format!("File '{abs}' not found.")
        } else {
            let tried: Vec<&str> = TS_EXTENSIONS
                .iter()
                .chain(if allow_js { JS_EXTENSIONS } else { &[] })
                .copied()
                .collect();
            if tried.iter().any(|e| fs.file_exists(&format!("{abs}{e}"))) {
                continue;
            }
            let list: Vec<String> = tried.iter().map(|e| format!("'{e}'")).collect();
            format!(
                "Could not resolve the path '{abs}' with the extensions: {}.",
                list.join(", ")
            )
        };
        let code = if known { 6053 } else { 6231 };
        let mut related = Vec::new();
        if let Some(&(s, e)) = entries.iter().find(|(_, v)| v == spec).map(|(sp, _)| sp) {
            related.push(RelatedInfo {
                code: 1410,
                category: Category::Message,
                message: "File is matched by 'files' list specified here.".to_owned(),
                file: file_id(&config.config_path),
                span: Span::new(s, e),
            });
        }
        out.push(Diagnostic {
            code,
            category: Category::Error,
            message: format!(
                "{message}\n  The file is in the program because:\n    Part of 'files' list in {name}"
            ),
            file: FileId(u32::MAX),
            span: Span::EMPTY,
            related,
        });
    }
    out
}

/// The string literals inside `span` of JSON text: `(span with quotes, value)`. Escapes are
/// kept as written (a spec with escapes simply finds no entry).
fn string_literals(text: &str, span: (u32, u32)) -> Vec<((u32, u32), String)> {
    let bytes = text.as_bytes();
    let end = (span.1 as usize).min(bytes.len());
    let mut i = (span.0 as usize).min(end);
    let mut out = Vec::new();
    while i < end {
        if bytes[i] != b'"' {
            i += 1;
            continue;
        }
        let start = i;
        i += 1;
        while i < end && bytes[i] != b'"' {
            if bytes[i] == b'\\' {
                i += 1;
            }
            i += 1;
        }
        if i >= end {
            break;
        }
        let value = text.get(start + 1..i).unwrap_or_default().to_owned();
        out.push(((start as u32, i as u32 + 1), value));
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals() {
        let t = r#"{ "files": ["a.ts", "b\"c"] }"#;
        let l = string_literals(t, (11, t.len() as u32));
        assert_eq!(l.len(), 2);
        assert_eq!(l[0], ((12, 18), "a.ts".to_owned()));
        assert_eq!(l[1].1, "b\\\"c");
        // Unterminated: nothing past the end, no panic.
        assert!(string_literals("\"abc", (0, 4)).is_empty());
    }
}
