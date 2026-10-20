//! The `typescript` backend: find the project's TypeScript and build the command line.
//!
//! * TS 7 (`typescript@7`): the package's native binary lives in the platform package
//!   `@typescript/typescript-<os>-<arch>/lib/tsc[.exe]`, a sibling of `typescript` in
//!   `node_modules` (npm, and pnpm's `.pnpm/<pkg>/node_modules`). Its language server mode is
//!   `tsc --lsp --stdio` (from `tsc --lsp --help`: `-stdio`, `-pipe`, `-socket`).
//! * TS 5/6: `node <pkg>/lib/tsserver.js` with options that keep it to one process: no
//!   `--serverMode` (so no separate syntax server is needed; the daemon is the only client),
//!   `--disableAutomaticTypingAcquisition` (no typings installer), `--noGetErrOnBackgroundUpdate`
//!   (the daemon asks for diagnostics itself) and a per-backend `--cancellationPipeName`.
//!
//! Test/expert overrides: `TS_R_LSP_TSSERVER_CMD` / `TS_R_LSP_NATIVE_CMD` (JSON arrays) replace
//! `node tsserver.js` / `<exe> --lsp --stdio`; `TS_R_LSP_NODE` picks the node binary.

use super::{BackendSpec, Protocol};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

/// How a TypeScript install is started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TsEntry {
    /// `lib/tsserver.js` (TS 5/6 and older).
    TsServer(PathBuf),
    /// The native executable (TS 7+).
    Native(PathBuf),
}

/// A TypeScript package on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TsInstall {
    /// `node_modules/typescript` directory.
    pub package_dir: PathBuf,
    /// `version` from its package.json.
    pub version: String,
    /// Major version (0 if unparsable).
    pub major: u32,
    /// How to start it.
    pub entry: TsEntry,
}

impl TsInstall {
    /// The protocol this install's server speaks.
    pub fn protocol(&self) -> Protocol {
        match self.entry {
            TsEntry::TsServer(_) => Protocol::TsServer,
            TsEntry::Native(_) => Protocol::Lsp,
        }
    }
}

/// Finds `node_modules/typescript` in `root` or its ancestors.
pub fn find_typescript(root: &Path) -> Option<TsInstall> {
    root.ancestors()
        .find_map(|dir| typescript_at(&dir.join("node_modules").join("typescript")))
}

/// Reads the TypeScript package at `package_dir`. Accepts the package dir, its `lib` dir or
/// a path to `lib/tsserver.js` (how editors name a TypeScript).
pub fn typescript_at(path: &Path) -> Option<TsInstall> {
    let mut package_dir = path.to_path_buf();
    if package_dir.file_name().is_some_and(|n| n == "tsserver.js") {
        package_dir.pop();
    }
    if package_dir.file_name().is_some_and(|n| n == "lib")
        && !package_dir.join("package.json").exists()
    {
        package_dir.pop();
    }
    let pkg = fs::read_to_string(package_dir.join("package.json")).ok()?;
    let pkg: serde_json::Value = serde_json::from_str(&pkg).ok()?;
    let version = pkg.get("version")?.as_str()?.to_string();
    let major = version
        .split('.')
        .next()
        .and_then(|m| m.parse().ok())
        .unwrap_or(0);
    let tsserver = package_dir.join("lib").join("tsserver.js");
    let entry = if major < 7 && tsserver.is_file() {
        TsEntry::TsServer(tsserver)
    } else if let Some(exe) = native_exe(&package_dir) {
        TsEntry::Native(exe)
    } else if tsserver.is_file() {
        TsEntry::TsServer(tsserver)
    } else {
        return None;
    };
    Some(TsInstall {
        package_dir,
        version,
        major,
        entry,
    })
}

/// `@typescript/typescript-<os>-<arch>` for this machine.
pub fn platform_package() -> String {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "arm" => "arm",
        other => other,
    };
    format!("typescript-{os}-{arch}")
}

/// Finds the native executable of a TS 7 package.
pub fn native_exe(package_dir: &Path) -> Option<PathBuf> {
    let exe = format!("tsc{}", std::env::consts::EXE_SUFFIX);
    let platform = platform_package();
    let real = fs::canonicalize(package_dir).unwrap_or_else(|_| package_dir.to_path_buf());
    let mut candidates = vec![
        package_dir
            .join("node_modules")
            .join("@typescript")
            .join(&platform),
    ];
    for dir in [package_dir, real.as_path()] {
        // Sibling in the same node_modules (npm hoisting, pnpm's virtual store).
        if let Some(nm) = dir.parent() {
            candidates.push(nm.join("@typescript").join(&platform));
        }
    }
    candidates
        .into_iter()
        .map(|c| c.join("lib").join(&exe))
        .find(|p| p.is_file())
}

/// Locates `node`: `TS_R_LSP_NODE`, then `PATH`.
pub fn find_node() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("TS_R_LSP_NODE").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    let name = format!("node{}", std::env::consts::EXE_SUFFIX);
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(&name))
        .find(|p| p.is_file())
}

fn override_cmd(var: &str) -> Option<Vec<String>> {
    let raw = std::env::var(var).ok()?;
    let v: Vec<String> = serde_json::from_str(&raw).ok()?;
    (!v.is_empty()).then_some(v)
}

/// Options for starting tsserver.
#[derive(Clone, Debug, Default)]
pub struct TsServerOptions {
    /// `--cancellationPipeName` value (must end with `*`).
    pub cancellation_pipe: Option<String>,
    /// `--locale`.
    pub locale: Option<String>,
    /// `--globalPlugins` (comma separated, from the editor).
    pub global_plugins: Option<String>,
    /// `--pluginProbeLocations` (comma separated, from the editor).
    pub plugin_probe_locations: Option<String>,
    /// Node to run tsserver with, when not found on `PATH` (e.g. the editor's Electron).
    pub node_fallback: Option<PathBuf>,
}

/// Command line for tsserver (TS 5/6). `None` if no node binary can be found.
pub fn tsserver_spec(
    install: &TsInstall,
    tsserver_js: &Path,
    root: &Path,
    opts: &TsServerOptions,
) -> Option<BackendSpec> {
    let mut env = Vec::new();
    let (program, mut args): (PathBuf, Vec<OsString>) =
        if let Some(cmd) = override_cmd("TS_R_LSP_TSSERVER_CMD") {
            (
                PathBuf::from(&cmd[0]),
                cmd[1..].iter().map(OsString::from).collect(),
            )
        } else {
            let node = match find_node() {
                Some(n) => n,
                None => {
                    let n = opts.node_fallback.clone()?;
                    // The editor's Electron binary behaves as node with this set.
                    env.push(("ELECTRON_RUN_AS_NODE".to_string(), "1".to_string()));
                    n
                }
            };
            (node, vec![tsserver_js.as_os_str().to_owned()])
        };
    for a in [
        "--useInferredProjectPerProjectRoot",
        "--disableAutomaticTypingAcquisition",
        "--noGetErrOnBackgroundUpdate",
    ] {
        args.push(a.into());
    }
    if let Some(c) = &opts.cancellation_pipe {
        args.push("--cancellationPipeName".into());
        args.push(c.into());
    }
    args.push("--locale".into());
    args.push(opts.locale.clone().unwrap_or_else(|| "en".into()).into());
    if let Some(p) = &opts.global_plugins {
        args.push("--globalPlugins".into());
        args.push(p.into());
    }
    if let Some(p) = &opts.plugin_probe_locations {
        args.push("--pluginProbeLocations".into());
        args.push(p.into());
    }
    Some(BackendSpec {
        protocol: Protocol::TsServer,
        label: format!("tsserver {}", install.version),
        version: install.version.clone(),
        program,
        args,
        env,
        cwd: root.to_path_buf(),
    })
}

/// Command line for TS 7's native language server.
pub fn native_lsp_spec(install: &TsInstall, exe: &Path, root: &Path) -> BackendSpec {
    let (program, args): (PathBuf, Vec<OsString>) = match override_cmd("TS_R_LSP_NATIVE_CMD") {
        Some(cmd) => (
            PathBuf::from(&cmd[0]),
            cmd[1..].iter().map(OsString::from).collect(),
        ),
        None => (exe.to_path_buf(), vec!["--lsp".into(), "--stdio".into()]),
    };
    BackendSpec {
        protocol: Protocol::Lsp,
        label: format!("typescript {} (native LSP)", install.version),
        version: install.version.clone(),
        program,
        args,
        env: Vec::new(),
        cwd: root.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(p: &Path, s: &str) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, s).unwrap();
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tsr-lsp-ts-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn finds_tsserver_in_ancestor() {
        let d = tmp("ts6");
        write(
            &d.join("node_modules/typescript/package.json"),
            r#"{"version":"6.0.3"}"#,
        );
        write(&d.join("node_modules/typescript/lib/tsserver.js"), "");
        fs::create_dir_all(d.join("apps/web")).unwrap();
        let ts = find_typescript(&d.join("apps/web")).unwrap();
        assert_eq!(ts.version, "6.0.3");
        assert_eq!(ts.major, 6);
        assert_eq!(ts.protocol(), Protocol::TsServer);
        // An editor-style path to tsserver.js resolves to the same package.
        let via_js = typescript_at(&d.join("node_modules/typescript/lib/tsserver.js")).unwrap();
        assert_eq!(via_js.version, "6.0.3");
        let via_lib = typescript_at(&d.join("node_modules/typescript/lib")).unwrap();
        assert_eq!(via_lib.package_dir, ts.package_dir);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn finds_native_sibling_package() {
        let d = tmp("ts7");
        write(
            &d.join("node_modules/typescript/package.json"),
            r#"{"version":"7.0.2"}"#,
        );
        let exe = d
            .join("node_modules/@typescript")
            .join(platform_package())
            .join("lib")
            .join(format!("tsc{}", std::env::consts::EXE_SUFFIX));
        write(&exe, "");
        let ts = find_typescript(&d).unwrap();
        assert_eq!(ts.major, 7);
        assert_eq!(ts.entry, TsEntry::Native(exe.clone()));
        let spec = native_lsp_spec(&ts, &exe, &d);
        assert_eq!(spec.protocol, Protocol::Lsp);
        assert_eq!(
            spec.args,
            vec![OsString::from("--lsp"), OsString::from("--stdio")]
        );
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn missing_or_broken_packages() {
        let d = tmp("none");
        fs::create_dir_all(&d).unwrap();
        assert!(typescript_at(&d.join("node_modules/typescript")).is_none());
        write(&d.join("node_modules/typescript/package.json"), "{not json");
        assert!(typescript_at(&d.join("node_modules/typescript")).is_none());
        // TS 7 package without its platform binary.
        write(
            &d.join("node_modules/typescript/package.json"),
            r#"{"version":"7.0.0"}"#,
        );
        assert!(typescript_at(&d.join("node_modules/typescript")).is_none());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn tsserver_command_line_disables_extra_processes() {
        let ts = TsInstall {
            package_dir: "/p/node_modules/typescript".into(),
            version: "5.9.3".into(),
            major: 5,
            entry: TsEntry::TsServer("/p/node_modules/typescript/lib/tsserver.js".into()),
        };
        let opts = TsServerOptions {
            cancellation_pipe: Some("/c/x*".into()),
            node_fallback: Some("/electron".into()),
            ..Default::default()
        };
        let spec = tsserver_spec(
            &ts,
            Path::new("/p/node_modules/typescript/lib/tsserver.js"),
            Path::new("/p"),
            &opts,
        )
        .unwrap();
        let args: Vec<String> = spec
            .args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(args.contains(&"--disableAutomaticTypingAcquisition".to_string()));
        assert!(
            !args
                .iter()
                .any(|a| a == "--serverMode" || a == "--syntaxOnly")
        );
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--cancellationPipeName" && w[1] == "/c/x*")
        );
        assert_eq!(spec.protocol, Protocol::TsServer);
    }
}
