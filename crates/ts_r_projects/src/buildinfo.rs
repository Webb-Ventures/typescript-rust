//! ts-r's `.tsbuildinfo` (design record 0012, decision 1): our own JSON at tsc's path. It
//! records what the up-to-date check and the next incremental build need, nothing more: the
//! root files, whether the last build reported errors and, for `incremental` and `composite`
//! projects, the program's files (content hash, declaration signature, dependencies),
//! the emit still pending per file, and hashes of the option classes that decide re-emit.
//!
//! Deterministic: fixed key order, files in program order (deterministic itself), pending
//! entries sorted, paths relative to the buildinfo's directory, no timestamps. Libraries are
//! left out (they change only with the ts-r version, which is checked first).

use serde::{Deserialize, Serialize};
use ts_r_config::{CompilerOptions, path};

/// The `version` we write and expect.
pub fn current_version() -> String {
    format!(
        "ts-r {} (tsc {})",
        env!("CARGO_PKG_VERSION"),
        ts_r_config::cli::TSC_VERSION
    )
}

/// Bits of [`ProgramState::pending`].
pub mod pending {
    /// JavaScript and its source map.
    pub const JS: u8 = 1;
    /// Declaration file and its map.
    pub const DTS: u8 = 2;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildInfo {
    pub version: String,
    /// Root files, relative to the buildinfo's directory, in config order.
    pub root: Vec<String>,
    /// The last build reported errors other than semantic ones (syntax, options, emit):
    /// what a `noCheck` build reports too.
    #[serde(default, skip_serializing_if = "is_false")]
    pub errors: bool,
    /// The last build reported semantic (checker) errors.
    #[serde(default, skip_serializing_if = "is_false")]
    pub semantic_errors: bool,
    /// A `noCheck` build left files unchecked: a checking build must run (and report).
    #[serde(default, skip_serializing_if = "is_false")]
    pub check_pending: bool,
    /// Program state of `incremental` / `composite` projects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program: Option<ProgramState>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgramState {
    pub options: OptionHashes,
    /// Every non-library file of the program, in program order.
    pub files: Vec<FileState>,
    /// `(index into files, pending bits)`, sorted by index.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending: Vec<(u32, u8)>,
    /// The declaration output written last with changed content (relative), for the
    /// downstream projects' up-to-date check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_changed_dts: Option<String>,
    /// `package.json` files module resolution read (relative, sorted): newer than the
    /// buildinfo means out of date.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub package_jsons: Vec<String>,
    /// `package.json` files resolution looked for and did not find: one appearing means out
    /// of date.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing_package_jsons: Vec<String>,
    /// Hash of the library files (names and contents, in program order): a different set
    /// affects every file (observed: `tsc/libraryResolution/when-noLib-toggles` re-emits
    /// everything).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub libs: String,
}

/// Hashes of the option classes (`ts_r_config::options::affects`). `js`/`dts` are the
/// options the last JavaScript / declaration emit used (kept while that kind is not
/// emitted), so turning an output kind off and on again re-emits only when its options
/// changed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OptionHashes {
    pub check: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub js: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dts: Option<String>,
    /// The map options of the last JavaScript emit ([`map_bits`](crate::incremental)):
    /// a change is a "change in compilerOptions" even when the config file is newer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub js_maps: Option<u8>,
    /// The same for declarations (`declarationMap`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dts_maps: Option<u8>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileState {
    /// Relative to the buildinfo's directory.
    pub path: String,
    /// Content hash (hex).
    pub hash: String,
    /// Declaration signature (hex): the hash of the `.d.ts` text the file had when it was
    /// last emitted or its shape last computed with declarations. Empty with an empty
    /// [`FileState::api`]: the signature is the file's version (never computed, or reset;
    /// tsc's "used version"), so the next computation is a change.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub signature: String,
    /// The declared API's hash (`ts_r_decls`): kept next to the signature, and compared
    /// when the shape is computed without a declaration output (`noEmit`, JavaScript only,
    /// `declaration` turned off).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub api: String,
    /// The file affects the global scope (a script, or `declare global`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub global: bool,
    /// Indices of the program files it depends on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deps: Vec<u32>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// What reading a buildinfo found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Read {
    Missing,
    /// Unreadable or not ours (a tsc buildinfo): the version it names, if any.
    Foreign(Option<String>),
    Ours(Box<BuildInfo>),
}

/// Parses buildinfo text. A tsc buildinfo or one from another ts-r version is
/// [`Read::Foreign`], with its `version` when there is one.
pub fn parse(text: &str) -> Read {
    #[derive(Deserialize)]
    struct VersionOnly {
        version: Option<String>,
    }
    let version = serde_json::from_str::<VersionOnly>(text)
        .ok()
        .and_then(|v| v.version);
    if version.as_deref() != Some(current_version().as_str()) {
        return Read::Foreign(version);
    }
    match serde_json::from_str::<BuildInfo>(text) {
        Ok(b) => Read::Ours(Box::new(b)),
        Err(_) => Read::Foreign(version),
    }
}

pub fn read(file: &str) -> Read {
    match std::fs::read(file) {
        Ok(bytes) => parse(&String::from_utf8_lossy(&bytes)),
        Err(_) => Read::Missing,
    }
}

/// The JSON text written (one line, as tsc writes it).
pub fn to_text(b: &BuildInfo) -> String {
    serde_json::to_string(b).unwrap_or_default()
}

/// Where tsc puts a project's buildinfo: `tsBuildInfoFile`, else
/// `<config file name without .json>.tsbuildinfo` next to the config, moved under `outDir`
/// like an output when the config lies inside `rootDir` (default: the config's directory).
pub fn path_for(options: &CompilerOptions, config_path: &str) -> String {
    if let Some(p) = options.ts_build_info_file() {
        return p.to_owned();
    }
    let name = path::file_name(config_path);
    let stem = name
        .strip_suffix(".json")
        .or_else(|| name.strip_suffix(".JSON"))
        .unwrap_or(name);
    let config_dir = path::directory_of(config_path);
    let dir = match options.out_dir() {
        Some(out) => {
            let root = options.root_dir().unwrap_or(config_dir);
            if path::contains(root, config_dir, true) {
                let rel = path::relative(root, config_dir, true);
                if rel.is_empty() {
                    out.to_owned()
                } else {
                    path::combine(out, &rel)
                }
            } else {
                config_dir.to_owned()
            }
        }
        None => config_dir.to_owned(),
    };
    path::combine(&dir, &format!("{stem}.tsbuildinfo"))
}

/// True when the project keeps program state (`incremental`, or `composite` unless
/// `incremental` is explicitly off).
pub fn is_incremental(options: &CompilerOptions) -> bool {
    options.effective().incremental()
}

/// `file` relative to the buildinfo's directory (`./a.ts`, `../core/index.d.ts`).
pub fn relative_to(buildinfo: &str, file: &str, case_sensitive: bool) -> String {
    let dir = path::directory_of(buildinfo);
    let rel = path::relative(dir, file, case_sensitive);
    if rel.starts_with("../") || rel.starts_with('/') || rel == ".." {
        rel
    } else {
        format!("./{rel}")
    }
}

/// The absolute path of `rel` (as written by [`relative_to`]).
pub fn absolute(buildinfo: &str, rel: &str) -> String {
    path::resolve(path::directory_of(buildinfo), rel)
}

/// The content hash of input `path` ([`FileState::hash`]): a declaration file's trailing
/// `//# sourceMappingURL=` line does not count, so turning `declarationMap` on or off in a
/// referenced project changes no input of its dependents (observed: they are "up to date
/// with .d.ts files from its dependencies").
pub fn content_hash(path: &str, text: &[u8]) -> String {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".d.ts") || lower.ends_with(".d.mts") || lower.ends_with(".d.cts") {
        let body = text.strip_suffix(b"\n").unwrap_or(text);
        let start = body.iter().rposition(|&b| b == b'\n').map_or(0, |n| n + 1);
        if body[start..].starts_with(b"//# sourceMappingURL=") {
            return hash_hex(&text[..start]);
        }
    }
    hash_hex(text)
}

/// Hex of the first 16 bytes of a BLAKE3 hash.
pub fn hash_hex(data: &[u8]) -> String {
    let h = blake3::hash(data);
    let mut s = String::with_capacity(32);
    for b in &h.as_bytes()[..16] {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let b = BuildInfo {
            version: current_version(),
            root: vec!["./a.ts".into()],
            errors: true,
            semantic_errors: false,
            check_pending: true,
            program: Some(ProgramState {
                options: OptionHashes {
                    check: "c".into(),
                    js: Some("j".into()),
                    dts: None,
                    js_maps: Some(1),
                    dts_maps: None,
                },
                files: vec![FileState {
                    path: "./a.ts".into(),
                    hash: "h".into(),
                    signature: "s".into(),
                    api: "a".into(),
                    global: true,
                    deps: vec![],
                }],
                pending: vec![(0, pending::JS)],
                latest_changed_dts: None,
                package_jsons: vec!["./package.json".into()],
                missing_package_jsons: vec![],
                libs: "l".into(),
            }),
        };
        let text = to_text(&b);
        assert_eq!(parse(&text), Read::Ours(Box::new(b)));
        assert_eq!(
            parse(r#"{"version":"FakeTSVersion","root":[]}"#),
            Read::Foreign(Some("FakeTSVersion".into()))
        );
        assert_eq!(parse("garbage"), Read::Foreign(None));
    }

    #[test]
    fn relative_paths() {
        assert_eq!(
            relative_to("/p/tsconfig.tsbuildinfo", "/p/a.ts", true),
            "./a.ts"
        );
        assert_eq!(
            relative_to("/p/out/tsconfig.tsbuildinfo", "/p/a.ts", true),
            "../a.ts"
        );
        assert_eq!(absolute("/p/out/x.tsbuildinfo", "../a.ts"), "/p/a.ts");
    }

    #[test]
    fn buildinfo_paths() {
        use ts_r_config::{OptionId, OptionValue};
        let set = |pairs: &[(OptionId, &str)]| {
            let mut o = CompilerOptions::new();
            for (id, v) in pairs {
                o.set(*id, Some(OptionValue::String((*v).to_owned())), None);
            }
            o
        };
        let cfg = "/p/tsconfig.json";
        assert_eq!(path_for(&set(&[]), cfg), "/p/tsconfig.tsbuildinfo");
        assert_eq!(
            path_for(&set(&[(OptionId::OutDir, "/p/dist")]), cfg),
            "/p/dist/tsconfig.tsbuildinfo"
        );
        // A config outside rootDir stays where it is.
        assert_eq!(
            path_for(
                &set(&[(OptionId::OutDir, "/p/dist"), (OptionId::RootDir, "/p/src")]),
                cfg
            ),
            "/p/tsconfig.tsbuildinfo"
        );
        // rootDir above the config: the config's directory is kept under outDir.
        assert_eq!(
            path_for(
                &set(&[
                    (OptionId::OutDir, "/p/server/dist"),
                    (OptionId::RootDir, "/p")
                ]),
                "/p/server/tsconfig.json"
            ),
            "/p/server/dist/server/tsconfig.tsbuildinfo"
        );
    }
}
