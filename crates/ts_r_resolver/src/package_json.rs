//! The parts of `package.json` module resolution reads.

use ts_r_config::jsonc::{self, Value};

/// A parsed `package.json`. Fields with the wrong JSON type are treated as absent, like tsc.
#[derive(Debug, Clone)]
pub struct PackageJson {
    /// Directory containing the file.
    pub dir: String,
    /// `name`
    pub name: Option<String>,
    /// `version`
    pub version: Option<String>,
    /// `type` (`"module"` or `"commonjs"`).
    pub type_field: Option<String>,
    /// `main`
    pub main: Option<String>,
    /// `types`
    pub types: Option<String>,
    /// `typings`
    pub typings: Option<String>,
    /// `typesVersions`
    pub types_versions: Option<Value>,
    /// `exports`
    pub exports: Option<Value>,
    /// `imports`
    pub imports: Option<Value>,
    /// Names in `peerDependencies`, in object order.
    pub peer_dependencies: Vec<String>,
}

impl PackageJson {
    /// Parses `text` (JSON with comments tolerated). Returns `None` when the root is not an
    /// object.
    pub fn parse(dir: &str, file: &str, text: &str) -> Option<Self> {
        let root = jsonc::parse(text, file).root?;
        root.as_object()?;
        let s = |k: &str| root.get(k).and_then(Value::as_str).map(str::to_owned);
        let v = |k: &str| {
            root.get(k)
                .filter(|v| !matches!(v.kind, jsonc::ValueKind::Invalid))
                .cloned()
        };
        Some(Self {
            dir: dir.to_owned(),
            name: s("name"),
            version: s("version"),
            type_field: s("type"),
            main: s("main"),
            types: s("types"),
            typings: s("typings"),
            types_versions: v("typesVersions").filter(|v| v.as_object().is_some()),
            exports: v("exports"),
            imports: v("imports").filter(|v| v.as_object().is_some()),
            peer_dependencies: root
                .get("peerDependencies")
                .and_then(Value::as_object)
                .map(|m| m.iter().map(|m| m.key.clone()).collect())
                .unwrap_or_default(),
        })
    }

    /// `"type": "module"`.
    pub fn is_module(&self) -> bool {
        self.type_field.as_deref() == Some("module")
    }

    /// The declaration entry point field: `typings` wins over `types` when present.
    pub fn types_entry(&self) -> Option<&str> {
        self.typings.as_deref().or(self.types.as_deref())
    }

    /// The `typesVersions` path map that applies to `version`, if any (first matching range
    /// in object order).
    pub fn types_versions_paths(&self, version: (u64, u64, u64)) -> Option<&Value> {
        let tv = self.types_versions.as_ref()?;
        tv.as_object()?
            .iter()
            .find(|m| crate::semver::range_matches(&m.key, version))
            .map(|m| &m.value)
            .filter(|v| v.as_object().is_some())
    }
}
