//! Front <-> daemon wire format.
//!
//! Every message in both directions is one `Content-Length` frame holding a JSON object.
//! The first frame from a front is a hello; the daemon answers with a welcome or an error.
//! After that, frames carry plain LSP messages (LSP fronts) or plain tsserver messages
//! (tsserver fronts), plus ts-r control messages, which are objects with a `"tsr"` key:
//!
//! ```json
//! {"tsr":"hello","protocol":1,"kind":"lsp","root":"/repo","name":"window 1","pid":42,
//!  "settings":{"memoryBudgetMB":1500},"tsserver":{"cancellationPipeName":"/tmp/x*"},
//!  "fallbackTsserver":"/Applications/.../typescript/lib","nodePath":"/usr/bin/node"}
//! {"tsr":"welcome","daemonPid":7,"backend":"tsserver 6.0.3","notes":[]}
//! {"tsr":"error","message":"no TypeScript found"}
//! {"tsr":"cancel","seq":12}          tsserver front: the editor cancelled request 12
//! {"tsr":"status"}                    -> {"tsr":"status","status":{...}}
//! ```

use crate::ipc::PROTOCOL_VERSION;
use serde_json::{Value, json};
use std::path::Path;

/// What kind of front is connecting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrontKind {
    /// An LSP client (editor speaking LSP through `ts-r-lsp` on stdio).
    Lsp,
    /// The tsserver stand-in (VS Code's built-in TypeScript extension).
    TsServer,
    /// `ts-r-lsp status`: asks for a status report, then disconnects.
    Status,
}

impl FrontKind {
    /// Wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            FrontKind::Lsp => "lsp",
            FrontKind::TsServer => "tsserver",
            FrontKind::Status => "status",
        }
    }

    /// Parses a wire name.
    pub fn parse(s: &str) -> Option<FrontKind> {
        match s {
            "lsp" => Some(FrontKind::Lsp),
            "tsserver" => Some(FrontKind::TsServer),
            "status" => Some(FrontKind::Status),
            _ => None,
        }
    }
}

/// Options a tsserver front passes through from the editor's tsserver command line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TsServerArgs {
    /// `--cancellationPipeName` (handled by the front; informational).
    pub cancellation_pipe: Option<String>,
    /// `--globalPlugins`.
    pub global_plugins: Option<String>,
    /// `--pluginProbeLocations`.
    pub plugin_probe_locations: Option<String>,
    /// `--locale`.
    pub locale: Option<String>,
    /// `--serverMode` (`partialSemantic` / `syntactic` when the editor starts a syntax server).
    pub server_mode: Option<String>,
}

impl TsServerArgs {
    /// Parses tsserver command-line arguments; unknown ones are ignored.
    pub fn parse<S: AsRef<str>>(args: &[S]) -> TsServerArgs {
        let mut out = TsServerArgs::default();
        let mut it = args.iter().map(AsRef::as_ref);
        while let Some(a) = it.next() {
            let slot = match a {
                "--cancellationPipeName" => &mut out.cancellation_pipe,
                "--globalPlugins" => &mut out.global_plugins,
                "--pluginProbeLocations" => &mut out.plugin_probe_locations,
                "--locale" => &mut out.locale,
                "--serverMode" => &mut out.server_mode,
                _ => continue,
            };
            *slot = it.next().map(str::to_string);
        }
        out
    }

    fn to_json(&self) -> Value {
        json!({
            "cancellationPipeName": self.cancellation_pipe,
            "globalPlugins": self.global_plugins,
            "pluginProbeLocations": self.plugin_probe_locations,
            "locale": self.locale,
            "serverMode": self.server_mode,
        })
    }

    fn from_json(v: &Value) -> TsServerArgs {
        let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
        TsServerArgs {
            cancellation_pipe: s("cancellationPipeName"),
            global_plugins: s("globalPlugins"),
            plugin_probe_locations: s("pluginProbeLocations"),
            locale: s("locale"),
            server_mode: s("serverMode"),
        }
    }
}

/// The first message of every connection.
#[derive(Clone, Debug, PartialEq)]
pub struct Hello {
    /// Front kind.
    pub kind: FrontKind,
    /// Workspace root.
    pub root: String,
    /// Display name (for logs and status).
    pub name: String,
    /// Front's pid.
    pub pid: u32,
    /// Raw `ts-r.*` settings.
    pub settings: Value,
    /// tsserver options (tsserver fronts).
    pub tsserver: TsServerArgs,
    /// A tsserver to use when the project has none (e.g. VS Code's bundled TypeScript).
    pub fallback_tsserver: Option<String>,
    /// Node (or Electron-as-node) to run tsserver with when `node` is not on PATH.
    pub node_path: Option<String>,
}

impl Hello {
    /// A hello for `kind` at `root`.
    pub fn new(kind: FrontKind, root: &Path) -> Hello {
        Hello {
            kind,
            root: root.to_string_lossy().into_owned(),
            name: format!("{} front {}", kind.as_str(), std::process::id()),
            pid: std::process::id(),
            settings: json!({}),
            tsserver: TsServerArgs::default(),
            fallback_tsserver: None,
            node_path: None,
        }
    }

    /// Wire form.
    pub fn to_json(&self) -> Value {
        json!({
            "tsr": "hello",
            "protocol": PROTOCOL_VERSION,
            "kind": self.kind.as_str(),
            "root": self.root,
            "name": self.name,
            "pid": self.pid,
            "settings": self.settings,
            "tsserver": self.tsserver.to_json(),
            "fallbackTsserver": self.fallback_tsserver,
            "nodePath": self.node_path,
        })
    }

    /// Parses a hello; `Err` explains what is wrong.
    pub fn from_json(v: &Value) -> Result<Hello, String> {
        if v.get("tsr").and_then(Value::as_str) != Some("hello") {
            return Err("expected a hello message".into());
        }
        let protocol = v.get("protocol").and_then(Value::as_u64).unwrap_or(0);
        if protocol != u64::from(PROTOCOL_VERSION) {
            return Err(format!(
                "front speaks protocol {protocol}, daemon speaks {PROTOCOL_VERSION}; restart the editor"
            ));
        }
        let kind = v
            .get("kind")
            .and_then(Value::as_str)
            .and_then(FrontKind::parse)
            .ok_or("unknown front kind")?;
        let root = v
            .get("root")
            .and_then(Value::as_str)
            .ok_or("hello without root")?
            .to_string();
        let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
        Ok(Hello {
            kind,
            root,
            name: s("name").unwrap_or_else(|| kind.as_str().to_string()),
            pid: v
                .get("pid")
                .and_then(Value::as_u64)
                .and_then(|p| u32::try_from(p).ok())
                .unwrap_or(0),
            settings: v.get("settings").cloned().unwrap_or(json!({})),
            tsserver: v
                .get("tsserver")
                .map(TsServerArgs::from_json)
                .unwrap_or_default(),
            fallback_tsserver: s("fallbackTsserver"),
            node_path: s("nodePath"),
        })
    }
}

/// Is this a ts-r control message (rather than an LSP or tsserver message)?
pub fn control_kind(v: &Value) -> Option<&str> {
    v.get("tsr").and_then(Value::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_round_trip() {
        let mut h = Hello::new(FrontKind::TsServer, Path::new("/repo"));
        h.tsserver = TsServerArgs::parse(&[
            "--useInferredProjectPerProjectRoot",
            "--cancellationPipeName",
            "/tmp/c*",
            "--globalPlugins",
            "a,b",
            "--locale",
        ]);
        h.fallback_tsserver = Some("/vscode/typescript/lib".into());
        let back = Hello::from_json(&h.to_json()).unwrap();
        assert_eq!(back, h);
        assert_eq!(back.tsserver.cancellation_pipe.as_deref(), Some("/tmp/c*"));
        assert_eq!(back.tsserver.locale, None);
    }

    #[test]
    fn hello_errors() {
        assert!(Hello::from_json(&json!({"tsr": "welcome"})).is_err());
        assert!(
            Hello::from_json(&json!({"tsr": "hello", "protocol": 999, "kind": "lsp", "root": "/"}))
                .is_err()
        );
        assert!(
            Hello::from_json(
                &json!({"tsr": "hello", "protocol": PROTOCOL_VERSION, "kind": "x", "root": "/"})
            )
            .is_err()
        );
        assert_eq!(control_kind(&json!({"tsr": "cancel"})), Some("cancel"));
        assert_eq!(control_kind(&json!({"seq": 1})), None);
    }
}
