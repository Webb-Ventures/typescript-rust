//! Type-aware lint rules on the project's checker (M4).
//!
//! After `check_and_publish` checks an owned file, the type-aware rules its lint config
//! enables (`ts-r-lint`'s `@typescript-eslint/*-type-checked` rules) run on the same checker
//! worker through `ts_r_lint::typed::lint_program_file`, so they reuse every type the check
//! computed. Their diagnostics (`source: "ts-r-lint"`) are published with the file's
//! semantic diagnostics; the daemon's lint worker runs the file's other rules without types
//! and the daemon merges both. Projects on the fallback TypeScript (parity mismatch) skip
//! them, as `ts-r-lint` does for projects whose diagnostics differ from tsc.
//!
//! Hooks in `project.rs`: [`Project::typed_lint`] in `check_and_publish`, the spans moved
//! with the semantic ones in `publish_syntax`, [`Project::lint_lsp`] in `publish`, and
//! [`Project::lint_config_changed`] in `on_doc` and on `PMsg::Config`.
//! `TS_R_LSP_TYPED_LINT=0` turns it off.

use super::{KeptCk, Project};
use crate::lint::{Linted, TypedConfigs};
use crate::text::TextDoc;
use crate::tsr::convert::LspLines;
use serde_json::Value;
use std::sync::OnceLock;
use std::time::Instant;
use ts_r_ast::SourceKind;
use ts_r_checker::ProgramFiles;
use ts_r_diagnostics::FileId;
use ts_r_parser::ParseOptions;

/// `TS_R_LSP_TYPED_LINT=0` disables type-aware lint in the ts-r backend.
fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("TS_R_LSP_TYPED_LINT").map_or(true, |v| v != "0"))
}

/// Per project: lint configs and counters.
#[derive(Debug, Default)]
pub struct LintState {
    configs: TypedConfigs,
    /// Files linted at least once (the first run is logged).
    seen: std::collections::HashSet<String>,
    /// Runs, their total and slowest time.
    pub runs: u64,
    pub total_ms: f64,
    pub max_ms: f64,
}

impl Project {
    /// Runs the type-aware lint rules on worker `k`, which has just checked `file`.
    pub(super) fn typed_lint<'c, 'p>(
        &mut self,
        program: &'p ProgramFiles,
        cks: &mut Vec<KeptCk<'c, 'p>>,
        k: usize,
        file: FileId,
        path: &str,
    ) -> Vec<ts_r_lint::Diagnostic> {
        if !enabled() {
            return Vec::new();
        }
        let f = &program.files[file.0 as usize];
        if f.is_lib || f.is_declaration() {
            return Vec::new();
        }
        let root = self.shared.root.clone();
        let Some(cfg) = self.lint.configs.config_for(&root, path) else {
            return Vec::new();
        };
        let first = self.lint.seen.insert(path.to_string());
        let t = Instant::now();
        let Some(r) = self.guarded(cks, path, |cks| {
            ts_r_lint::typed::lint_program_file(&mut cks[k].ck, file, &cfg)
        }) else {
            return Vec::new();
        };
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        self.lint.runs += 1;
        self.lint.total_ms += ms;
        self.lint.max_ms = self.lint.max_ms.max(ms);
        if first || ms > 50.0 {
            crate::daemon::log(&format!(
                "ts-r {}: type-aware lint of {path}: {} rule(s), {} problem(s) in {ms:.2} ms \
                 (mean {:.2} ms over {} runs)",
                self.name,
                cfg.rules.rules.len(),
                r.diagnostics.len(),
                self.lint.total_ms / self.lint.runs as f64,
                self.lint.runs,
            ));
        }
        r.diagnostics
            .into_iter()
            .filter(|d| !d.fatal && d.rule.is_some())
            .collect()
    }

    /// Whether `path`'s lint config is known (or type-aware lint is off), so linting it
    /// costs only the rules.
    pub(super) fn lint_config_ready(&self, path: &str) -> bool {
        !enabled() || self.lint.configs.is_known(path)
    }

    /// The published lint diagnostics of a file as LSP diagnostics.
    pub(super) fn lint_lsp(
        diags: &[ts_r_lint::Diagnostic],
        text: &str,
        lines: &LspLines,
    ) -> Vec<Value> {
        diags
            .iter()
            .map(|d| crate::lint::lsp_diagnostic_at(d, lines.range(text, d.span.start, d.span.end)))
            .collect()
    }

    /// Quick fixes of the file's type-aware lint diagnostics for `textDocument/codeAction`
    /// (fix, suggestions, disable comments). `source.fixAll.ts-r` stays with the daemon's
    /// lint worker, so one fix-all action is offered, without the type-aware fixes. None
    /// until the file is checked after an edit: `publish_syntax` moves the published spans,
    /// not the edits of their fixes and suggestions.
    pub(super) fn lint_code_actions(
        &self,
        program: &ProgramFiles,
        file: FileId,
        path: &str,
        params: &Value,
    ) -> Vec<Value> {
        let (Some(pb), Some(f)) = (self.published.get(path), program.files.get(file.0 as usize))
        else {
            return Vec::new();
        };
        if pb.lint.is_empty() || !pb.fresh {
            return Vec::new();
        }
        let (Some(uri), Some((start, end))) = (
            params["textDocument"]["uri"].as_str(),
            crate::text::lsp_range(&params["range"]),
        ) else {
            return Vec::new();
        };
        let only: Vec<String> = params["context"]["only"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
        let text = TextDoc::new(f.text.to_string());
        let lsp = pb
            .lint
            .iter()
            .map(|d| crate::lint::lsp_diagnostic(d, &text, 0))
            .collect();
        // The grammar the lint worker parses the file with (for disable comments in JSX).
        let parse = self
            .lint
            .configs
            .cached(path)
            .map_or_else(ParseOptions::default, |fc| {
                crate::lint::parse_options(&fc.env)
            });
        let linted = Linted {
            text,
            diags: pb.lint.clone(),
            lsp,
            bom: 0,
            kind: SourceKind::from_path(path),
            parse,
        };
        let mut out = crate::lint::code_actions(&linted, uri, start, end, &only);
        out.retain(|a| a["kind"] != crate::lint::FIX_ALL_KIND);
        out
    }

    /// A lint config file changed (on disk), or the project's config: load configs again
    /// and re-lint every open file.
    pub(super) fn lint_config_changed(&mut self) {
        self.lint.configs.reload();
        for p in self.owned.keys() {
            if !self.todo.contains(p) {
                self.todo.push_back(p.clone());
            }
        }
    }
}
