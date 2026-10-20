//! Requests for editor bridges that pick an engine per project themselves, like the VS Code
//! extension's tsserver stand-in (`editors/vscode/standin/router.js`): it sends a file to the
//! ts-r checker only when that file's project passed the parity check, so it must learn the
//! project and its parity state *without opening the file here*.
//!
//! `ts-r/projectInfo` `{textDocument: {uri}}` answers `{project, config, state, parity,
//! configDiagnostics}`: the owning project (the same lookup as `didOpen`), started if needed,
//! which also starts its parity check (or reads the cached result; the outcome then follows as
//! a `ts-r/status`). `configDiagnostics` are the config's parse and file-discovery diagnostics
//! (`[{code, message, severity: 1 | 2, file, range?}]`, LSP ranges), which tsserver clients
//! show on the tsconfig (`configFileDiag`).
//!
//! Cross-engine mirroring: a file the bridge serves with TypeScript can be imported by a
//! project it serves with ts-r, which would otherwise read the saved text.
//!
//! * `ts-r/usedBy` `{uris: [uri]}` answers `{<uri>: [project]}`: for each file, the loaded
//!   projects whose program contains it (`config` path, or `"(inferred)"`), as of their last
//!   load; files no project uses are left out.
//! * `ts-r/inputsChanged` `{project}` (notification) follows a load that changed a project's
//!   file set, so the bridge asks again.
//! * A `didOpen` with `languageId` [`MIRROR_LANGUAGE_ID`] is a *mirror*: every project reads
//!   its open text, no project owns it (no diagnostics; its project is not started and its
//!   fallback TypeScript never opens it). The language id is kept by the daemon's document
//!   mirror, so a backend restart replays it as a mirror too.

use super::{INFERRED, Server};
use crate::text::{TextDoc, to_lsp_range};
use serde_json::{Map, Value, json};
use ts_r_config::{ConfigDiagnostic, FileSystem, RealFs};

/// `languageId` of a document another engine serves (see the module docs).
pub const MIRROR_LANGUAGE_ID: &str = "ts-r-mirror";

impl Server {
    /// `ts-r/usedBy`: which loaded projects' programs contain each file.
    pub(super) fn used_by(&self, params: &Value) -> Value {
        let mut out = Map::new();
        for uri in params["uris"].as_array().into_iter().flatten() {
            let Some(uri) = uri.as_str() else {
                continue;
            };
            let path = crate::uri::key_for_uri(uri);
            let users: Vec<Value> = self
                .projects
                .iter()
                .filter(|(_, p)| p.inputs.contains(&path))
                .map(|(k, _)| json!(k))
                .collect();
            if !users.is_empty() {
                out.insert(uri.to_string(), Value::Array(users));
            }
        }
        Value::Object(out)
    }

    /// `ts-r/projectInfo`: the project that owns a document and its parity state.
    pub(super) fn project_info(&mut self, params: &Value) -> Value {
        let Some(uri) = params["textDocument"]["uri"].as_str() else {
            return Value::Null;
        };
        let path = crate::uri::key_for_uri(uri);
        let owner = match self.docs.get(&path) {
            Some(d) => d.owner.clone(),
            None => self.owner_of(&path),
        };
        self.ensure_project(&owner);
        let Some(p) = self.projects.get(&owner) else {
            return Value::Null;
        };
        let config_diagnostics: Vec<Value> = match self.configs.get(&owner) {
            Some(Some(c)) => config_diagnostics(&c.diagnostics),
            _ => Vec::new(),
        };
        json!({
            "project": self.display_name(&owner),
            "config": if owner == INFERRED { Value::Null } else { json!(owner) },
            "state": p.state.as_str(),
            "parity": p.parity.as_ref().map(super::parity::Summary::to_json),
            "configDiagnostics": config_diagnostics,
        })
    }
}

/// Config diagnostics as JSON with LSP ranges (UTF-16) into the file they point at.
fn config_diagnostics(diags: &[ConfigDiagnostic]) -> Vec<Value> {
    let fs = RealFs::new();
    diags
        .iter()
        .map(|d| {
            let range = match (&d.file, d.span) {
                (Some(file), Some((start, end))) => fs.read_file(file).map(|text| {
                    let doc = TextDoc::new(text);
                    to_lsp_range(
                        doc.position_at(start as usize),
                        doc.position_at(end as usize),
                    )
                }),
                _ => None,
            };
            json!({
                "code": d.code,
                "message": d.message,
                "severity": match d.severity {
                    ts_r_config::Severity::Error => 1,
                    ts_r_config::Severity::Warning => 2,
                },
                "file": d.file,
                "range": range,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_diagnostics_have_utf16_ranges() {
        let dir = std::env::temp_dir().join(format!("ts-r-bridge-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("tsconfig.json");
        std::fs::write(&file, "{\"é\": 1, \"extends\": \"x\"}").unwrap();
        let f = file.to_string_lossy().into_owned();
        let d = ConfigDiagnostic::at(6053, "File 'x' not found.", &f, (22, 25));
        let v = config_diagnostics(&[d, ConfigDiagnostic::global(5023, "Unknown.")]);
        // Byte 22 is UTF-16 column 21: `é` is two bytes, one unit.
        assert_eq!(v[0]["range"]["start"]["character"], 21);
        assert_eq!(v[0]["code"], 6053);
        assert_eq!(v[0]["severity"], 1);
        assert!(v[1]["range"].is_null());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
