//! `xtask ecosystem shim tsc [tsc arguments]`: the `tsc` the tests stage puts into a package's
//! `node_modules` (as `.bin/tsc` and `typescript/bin/tsc`).
//!
//! - Type checking: `ts-r-check` on the same project (command-line options are written into a
//!   temporary tsconfig next to the project's, which `extends` it).
//! - JavaScript: ts-r's transform, one file at a time (what `isolatedModules` allows), of the
//!   root files and the project files they import, into `outDir` with the `rootDir` layout
//!   tsc uses.
//! - Declarations: the reference tsc 7 with `--emitDeclarationOnly --noCheck`, because ts-r
//!   has no declaration emitter yet. Source maps are not written.
//!
//! Exit codes follow tsc: 0 clean, 1 errors and nothing emitted, 2 errors and outputs
//! written. Every run appends one line to `$TS_R_ECO_SHIM_LOG`:
//! `tsc<TAB>exit<TAB>detail`. This is harness code: it only calls the compiler crates' public
//! APIs and binaries.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use serde_json::{Map, Value};
use ts_r_config::options::{OptionType, find_option_ignore_case};

use crate::util::Result;

pub const ENV_LOG: &str = "TS_R_ECO_SHIM_LOG";
pub const ENV_BIN_DIR: &str = "TS_R_ECO_BIN_DIR";
pub const ENV_TSC: &str = "TS_R_ECO_TSC";
/// Set to `1`: report ts-r-check's errors but exit as if there were none (the tests stage's
/// second ts-r run, which shows whether the rest of the pipeline works).
pub const ENV_IGNORE_CHECK: &str = "TS_R_ECO_IGNORE_CHECK";

pub fn run(mut items: Vec<String>) -> Result<ExitCode> {
    if items.first().map(String::as_str) != Some("tsc") {
        return Err("usage: xtask ecosystem shim tsc [tsc arguments]".into());
    }
    items.remove(0);
    let code = match tsc(&items) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("tsc (ts-r shim): {e}");
            record(1, &format!("shim error: {e}"));
            1
        }
    };
    Ok(ExitCode::from(code))
}

/// Appends one event to the shim log.
fn record(code: u8, detail: &str) {
    let Ok(log) = std::env::var(ENV_LOG) else {
        return;
    };
    use std::io::Write as _;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
    {
        let detail: String = detail.split_whitespace().collect::<Vec<_>>().join(" ");
        let _ = writeln!(f, "tsc\t{code}\t{}", super::report::truncate(&detail, 300));
    }
}

#[derive(Debug, Default, PartialEq)]
struct Cli {
    projects: Vec<String>,
    files: Vec<String>,
    options: Map<String, Value>,
    build: bool,
    version: bool,
    unsupported: Option<String>,
    ignored: Vec<String>,
}

fn parse_cli(args: &[String], cwd: &Path) -> Cli {
    let mut cli = Cli::default();
    let mut i = 0;
    // `/`-separated like tsc's own paths (and the generated tsconfig) on every OS.
    let abs = |p: &str| slashes(&cwd.join(p).to_string_lossy());
    while i < args.len() {
        let a = args[i].as_str();
        let next = args.get(i + 1).map(String::as_str);
        let (flag, inline) = match a.split_once('=') {
            Some((f, v)) if f.starts_with('-') => (f, Some(v)),
            _ => (a, None),
        };
        let take = |i: &mut usize| -> Option<String> {
            if let Some(v) = inline {
                return Some(v.to_string());
            }
            *i += 1;
            args.get(*i).cloned()
        };
        match flag {
            "-v" | "--version" => cli.version = true,
            "-p" | "--project" => {
                if let Some(v) = take(&mut i) {
                    cli.projects.push(abs(&v));
                }
            }
            "-b" | "--build" => cli.build = true,
            "-w" | "--watch" | "--init" | "--showConfig" | "--listFilesOnly" | "-h" | "--help"
            | "--all" => cli.unsupported = Some(flag.to_string()),
            "--force"
            | "--verbose"
            | "--dry"
            | "--clean"
            | "--listEmittedFiles"
            | "--listFiles"
            | "--extendedDiagnostics"
            | "--diagnostics"
            | "--pretty"
            | "--preserveWatchOutput" => {
                if matches!(next, Some("true" | "false")) && inline.is_none() {
                    i += 1;
                }
                if flag == "--clean" {
                    cli.unsupported = Some(flag.to_string());
                }
            }
            "-d" => {
                cli.options.insert("declaration".into(), Value::Bool(true));
            }
            "-m" | "-t" => {
                let name = if flag == "-m" { "module" } else { "target" };
                if let Some(v) = take(&mut i) {
                    cli.options.insert(name.into(), Value::String(v));
                }
            }
            f if f.starts_with("--") => {
                let name = &f[2..];
                match find_option_ignore_case(name) {
                    Some(decl) => {
                        let value = match decl.ty {
                            OptionType::Bool => match (inline, next) {
                                (Some(v), _) => Value::Bool(v != "false"),
                                (None, Some(v @ ("true" | "false"))) => {
                                    i += 1;
                                    Value::Bool(v == "true")
                                }
                                _ => Value::Bool(true),
                            },
                            OptionType::Number => match take(&mut i).and_then(|v| v.parse().ok()) {
                                Some(n) => Value::Number(n),
                                None => Value::Null,
                            },
                            OptionType::Path => take(&mut i)
                                .map(|v| Value::String(abs(&v)))
                                .unwrap_or(Value::Null),
                            OptionType::PathList => take(&mut i)
                                .map(|v| {
                                    Value::Array(v.split(',').map(|p| abs(p).into()).collect())
                                })
                                .unwrap_or(Value::Null),
                            OptionType::StringList | OptionType::Lib => take(&mut i)
                                .map(|v| {
                                    Value::Array(v.split(',').map(|s| s.trim().into()).collect())
                                })
                                .unwrap_or(Value::Null),
                            OptionType::PathMap | OptionType::Plugins => {
                                cli.ignored.push(f.to_string());
                                Value::Null
                            }
                            _ => take(&mut i).map(Value::String).unwrap_or(Value::Null),
                        };
                        if !value.is_null() {
                            cli.options.insert(decl.name.to_string(), value);
                        }
                    }
                    None => cli.ignored.push(f.to_string()),
                }
            }
            _ if cli.build => cli.projects.push(abs(a)),
            _ => cli.files.push(abs(a)),
        }
        i += 1;
    }
    cli
}

/// `tsconfig.json` for a `-p`/`-b` argument (a file or a directory).
fn config_file(p: &str) -> PathBuf {
    let p = PathBuf::from(p);
    if p.is_dir() {
        p.join("tsconfig.json")
    } else {
        p
    }
}

/// The nearest `tsconfig.json` from `dir` upwards (what tsc does without `-p`).
fn find_config(dir: &Path) -> Option<PathBuf> {
    dir.ancestors()
        .map(|d| d.join("tsconfig.json"))
        .find(|p| p.is_file())
}

/// Removes the temporary tsconfig however the shim ends.
struct TempFile(PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn tsc(args: &[String]) -> std::result::Result<u8, String> {
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let cli = parse_cli(args, &cwd);
    if cli.version {
        println!("Version 7.0.0 (ts-r ecosystem shim)");
        return Ok(0);
    }
    if let Some(f) = &cli.unsupported {
        return Err(format!("{f} is not supported"));
    }
    // Each unit: (directory for the temporary config, its JSON).
    let mut units: Vec<(PathBuf, Value)> = Vec::new();
    let options = Value::Object(cli.options.clone());
    if !cli.files.is_empty() {
        // tsc ignores tsconfig.json when files are named on the command line.
        units.push((
            cwd.clone(),
            serde_json::json!({ "compilerOptions": options, "files": cli.files }),
        ));
    } else {
        let configs: Vec<PathBuf> = if cli.projects.is_empty() {
            vec![find_config(&cwd).ok_or("no tsconfig.json found (TS5057)")?]
        } else {
            cli.projects.iter().map(|p| config_file(p)).collect()
        };
        for c in configs {
            if !c.is_file() {
                return Err(format!("cannot find {} (TS5058)", c.display()));
            }
            let dir = c.parent().unwrap_or(Path::new(".")).to_path_buf();
            let base = c
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            units.push((
                dir,
                serde_json::json!({ "extends": format!("./{base}"), "compilerOptions": options }),
            ));
        }
    }
    let bin_dir = PathBuf::from(std::env::var(ENV_BIN_DIR).map_err(|_| "TS_R_ECO_BIN_DIR unset")?);
    let mut worst = 0u8;
    let mut details = Vec::new();
    if !cli.ignored.is_empty() {
        details.push(format!("ignored {}", cli.ignored.join(" ")));
    }
    for (dir, json) in units {
        let tmp = TempFile(dir.join(format!("tsconfig.ts-r-shim-{}.json", std::process::id())));
        std::fs::write(&tmp.0, json.to_string()).map_err(|e| e.to_string())?;
        let (code, detail) = unit(&tmp.0, &bin_dir)?;
        worst = worst.max(code);
        details.push(detail);
    }
    record(worst, &details.join("; "));
    Ok(worst)
}

/// Checks and emits one project. Returns tsc's exit code and a log detail.
fn unit(config: &Path, bin_dir: &Path) -> std::result::Result<(u8, String), String> {
    // 1. Type check (`--noEmit`: ts-r-check emits too, but this shim emits below, per file,
    // for every target).
    let out = Command::new(bin_dir.join("ts-r-check"))
        .arg("-p")
        .arg(config)
        .args(["--pretty", "false", "--threads", "1", "--noEmit"])
        .output()
        .map_err(|e| format!("ts-r-check: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).replace(
        &config
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        "tsconfig.json",
    );
    print!("{stdout}");
    eprint!("{}", String::from_utf8_lossy(&out.stderr));
    let errors: Vec<&str> = stdout.lines().filter(|l| l.contains("error TS")).collect();
    let crashed = !out.status.success() && errors.is_empty();
    if crashed {
        return Err(format!(
            "ts-r-check exited {:?}: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
        ));
    }
    let mut detail = if errors.is_empty() {
        "check ok".to_string()
    } else {
        format!("check: {} errors, first {}", errors.len(), errors[0].trim())
    };
    let ignore = std::env::var(ENV_IGNORE_CHECK).is_ok_and(|v| v == "1");
    if ignore && !errors.is_empty() {
        detail = format!("ignored {detail}");
    }

    // 2. Emit.
    let parsed = ts_r_config::load_tsconfig(&ts_r_config::RealFs::new(), &config.to_string_lossy());
    let o = &parsed.options;
    let no_emit = o.effective().no_emit();
    let skip_on_error = !errors.is_empty() && o.no_emit_on_error().unwrap_or(false);
    let decl_only = o.emit_declaration_only().unwrap_or(false);
    let mut emitted = false;
    if !no_emit && !skip_on_error {
        if !decl_only {
            let n = emit_js(&parsed)?;
            detail.push_str(&format!(", emitted {n} js"));
            emitted = true;
        }
        if o.effective().declaration() || decl_only {
            detail.push_str(&declarations(config));
            emitted = true;
        }
    }
    let code = match (errors.is_empty() || ignore, emitted) {
        (true, _) => 0,
        (false, false) => 1,
        (false, true) => 2,
    };
    Ok((code, detail))
}

/// Declarations from the reference tsc (ts-r has no declaration emitter yet).
fn declarations(config: &Path) -> String {
    let Ok(tsc) = std::env::var(ENV_TSC) else {
        return ", no declarations (TS_R_ECO_TSC unset)".into();
    };
    let o = Command::new(tsc)
        .arg("-p")
        .arg(config)
        .args([
            "--emitDeclarationOnly",
            "--declaration",
            "--noCheck",
            "--pretty",
            "false",
        ])
        .output();
    match o {
        Ok(o) if o.status.success() => ", declarations by tsc".into(),
        Ok(o) => format!(
            ", tsc declarations exited {:?}: {}",
            o.status.code(),
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .next()
                .unwrap_or("")
        ),
        Err(e) => format!(", tsc declarations failed: {e}"),
    }
}

fn is_dts(f: &str) -> bool {
    [".d.ts", ".d.mts", ".d.cts"].iter().any(|e| f.ends_with(e)) || {
        // `foo.d.css.ts` style arbitrary-extension declarations.
        let base = f.rsplit('/').next().unwrap_or(f);
        base.contains(".d.") && base.ends_with(".ts")
    }
}

/// The source files tsc would emit: the config's root files and every project file they
/// import (transitively), without declaration files, JSON and files under `node_modules`.
fn emitted_sources(parsed: &ts_r_config::ParsedConfig) -> Vec<String> {
    let program = ts_r_checker::load_program(
        &ts_r_config::RealFs::new(),
        &parsed.file_names,
        &parsed.options,
        None,
        1,
    );
    program
        .files
        .iter()
        .filter(|f| !f.is_lib && !f.is_external && !is_dts(&f.path) && !f.path.ends_with(".json"))
        .map(|f| f.path.clone())
        .collect()
}

/// Writes ts-r's JavaScript for every emitted source file. Returns the number of files
/// written.
fn emit_js(parsed: &ts_r_config::ParsedConfig) -> std::result::Result<usize, String> {
    use ts_r_codegen::{CodegenOptions, SourceMapOptions};
    use ts_r_transform::{TransformOptions, transpile};

    let o = &parsed.options;
    if o.out_file().is_some() {
        return Err("outFile is not supported".into());
    }
    let sources = emitted_sources(parsed);
    let sources: Vec<&String> = sources.iter().collect();
    let out_dir = o.out_dir().map(PathBuf::from);
    let root = o
        .root_dir()
        .map(PathBuf::from)
        .unwrap_or_else(|| common_dir(&sources, &parsed.config_dir));
    let preserve_jsx = matches!(o.effective().jsx(), Some(ts_r_config::JsxEmit::Preserve));
    let inline_map = o.inline_source_map().unwrap_or(false);
    let maps = o.source_map().unwrap_or(false) && !inline_map;
    let mut n = 0;
    for f in sources {
        let is_js = [".js", ".jsx", ".mjs", ".cjs"]
            .iter()
            .any(|e| f.ends_with(e));
        if is_js && out_dir.is_none() {
            continue; // tsc never overwrites an input
        }
        let src = std::fs::read_to_string(f).map_err(|e| format!("{f}: {e}"))?;
        let cjs_file = f.ends_with(".cts")
            || f.ends_with(".cjs")
            || (!f.ends_with(".mts") && !f.ends_with(".mjs") && !type_module(Path::new(f)));
        let rel = Path::new(f)
            .strip_prefix(&root)
            .map(Path::to_path_buf)
            .unwrap_or_else(|_| PathBuf::from(Path::new(f).file_name().unwrap_or_default()));
        let base = match &out_dir {
            Some(d) => d.join(&rel),
            None => PathBuf::from(f),
        };
        let dest = js_path(&base, preserve_jsx);
        let dest_name = dest
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let codegen = CodegenOptions {
            source_map: (maps || inline_map).then(|| SourceMapOptions {
                file: Some(dest_name.clone()),
                source: relative(dest.parent().unwrap_or(Path::new("/")), Path::new(f)),
                source_root: o.source_root().map(String::from),
                include_content: o.inline_sources().unwrap_or(false),
            }),
            ..CodegenOptions::default()
        };
        let opts = TransformOptions::from_compiler_options(o, f, cjs_file);
        let out = transpile(f, &src, &opts, &codegen);
        if let Some(d) = out.diagnostics.iter().find(|d| d.is_error()) {
            return Err(format!("emit {f}: TS{} {}", d.code, d.message));
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut code = out.code;
        if let Some(map) = &out.map {
            let json = map.to_json();
            if !code.ends_with('\n') {
                code.push('\n');
            }
            if inline_map {
                code.push_str("//# sourceMappingURL=data:application/json;base64,");
                code.push_str(&super::packages::base64(json.as_bytes()));
            } else {
                let map_path = PathBuf::from(format!("{}.map", dest.display()));
                std::fs::write(&map_path, json).map_err(|e| e.to_string())?;
                code.push_str(&format!("//# sourceMappingURL={dest_name}.map"));
            }
        }
        std::fs::write(&dest, code).map_err(|e| format!("{}: {e}", dest.display()))?;
        n += 1;
    }
    Ok(n)
}

/// `to` relative to the directory `from` (both absolute), with `/` separators.
/// `\` → `/` on Windows (where both separate); unchanged elsewhere.
fn slashes(s: &str) -> String {
    if cfg!(windows) {
        s.replace('\\', "/")
    } else {
        s.to_owned()
    }
}

fn relative(from: &Path, to: &Path) -> String {
    let a: Vec<_> = from.components().collect();
    let b: Vec<_> = to.components().collect();
    let common = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let mut parts: Vec<String> = vec!["..".to_string(); a.len() - common];
    parts.extend(
        b[common..]
            .iter()
            .map(|c| c.as_os_str().to_string_lossy().into_owned()),
    );
    parts.join("/")
}

/// `a/b.ts` → `a/b.js` (`.tsx` → `.jsx` under `jsx: preserve`, `.mts` → `.mjs`, `.cts` → `.cjs`).
fn js_path(p: &Path, preserve_jsx: bool) -> PathBuf {
    let s = p.to_string_lossy();
    for (from, to) in [
        (".mts", ".mjs"),
        (".cts", ".cjs"),
        (".tsx", if preserve_jsx { ".jsx" } else { ".js" }),
        (".ts", ".js"),
    ] {
        if let Some(stem) = s.strip_suffix(from) {
            return PathBuf::from(format!("{stem}{to}"));
        }
    }
    p.to_path_buf()
}

/// The longest common directory of the source files (tsc's computed `rootDir`).
fn common_dir(files: &[&String], fallback: &str) -> PathBuf {
    let mut common: Option<PathBuf> = None;
    for f in files {
        let dir = Path::new(f.as_str()).parent().unwrap_or(Path::new("/"));
        common = Some(match common {
            None => dir.to_path_buf(),
            Some(c) => {
                let mut acc = PathBuf::new();
                for (a, b) in c.components().zip(dir.components()) {
                    if a != b {
                        break;
                    }
                    acc.push(a);
                }
                acc
            }
        });
    }
    common.unwrap_or_else(|| PathBuf::from(fallback))
}

/// Whether the nearest `package.json` says `"type": "module"`.
fn type_module(file: &Path) -> bool {
    for dir in file.ancestors().skip(1) {
        let pj = dir.join("package.json");
        if let Ok(text) = std::fs::read_to_string(&pj) {
            return serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| v.get("type").and_then(Value::as_str).map(|t| t == "module"))
                .unwrap_or(false);
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn parses_tsc_command_lines() {
        let cwd = Path::new("/w");
        let c = parse_cli(&args("-p .tshy/esm.json"), cwd);
        assert_eq!(c.projects, ["/w/.tshy/esm.json"]);
        let c = parse_cli(
            &args("--noEmit --types node,mocha source/index.d.ts --strict false --outDir out"),
            cwd,
        );
        assert_eq!(c.files, ["/w/source/index.d.ts"]);
        assert_eq!(c.options["noEmit"], Value::Bool(true));
        assert_eq!(c.options["strict"], Value::Bool(false));
        assert_eq!(c.options["types"], serde_json::json!(["node", "mocha"]));
        assert_eq!(c.options["outDir"], Value::String("/w/out".into()));
        let c = parse_cli(&args("-b packages/a --verbose"), cwd);
        assert!(c.build);
        assert_eq!(c.projects, ["/w/packages/a"]);
        assert_eq!(
            parse_cli(&args("--watch"), cwd).unsupported.as_deref(),
            Some("--watch")
        );
        assert_eq!(parse_cli(&args("--bogus"), cwd).ignored, ["--bogus"]);
    }

    #[test]
    fn output_paths() {
        assert_eq!(
            relative(Path::new("/p/dist/esm"), Path::new("/p/src/a.ts")),
            "../../src/a.ts"
        );
        assert_eq!(relative(Path::new("/p"), Path::new("/p/a.ts")), "a.ts");
        assert_eq!(js_path(Path::new("a/b.mts"), false), Path::new("a/b.mjs"));
        assert_eq!(js_path(Path::new("a/b.tsx"), true), Path::new("a/b.jsx"));
        assert_eq!(js_path(Path::new("a/b.tsx"), false), Path::new("a/b.js"));
        let a = "/p/src/a.ts".to_string();
        let b = "/p/src/x/b.ts".to_string();
        assert_eq!(common_dir(&[&a, &b], "/p"), Path::new("/p/src"));
        assert!(is_dts("/p/a.d.ts") && is_dts("/p/a.d.css.ts") && !is_dts("/p/a.ts"));
    }

    #[test]
    fn imported_files_are_emitted_too() {
        let dir = std::env::temp_dir().join(format!("ts-r-shim-emit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let files = [
            (
                "tsconfig.json",
                r#"{"compilerOptions": {"rootDir": "src", "outDir": "dist", "module": "nodenext"}, "files": ["src/index.ts"]}"#,
            ),
            (
                "src/index.ts",
                "import { dep } from \"./lib/dep.js\";\nconsole.log(dep);\n",
            ),
            ("src/lib/dep.ts", "export const dep: number = 1;\n"),
            ("src/lib/types.d.ts", "export {};\n"),
        ];
        for (name, text) in files {
            let p = dir.join(name);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        let cfg = dir.join("tsconfig.json");
        let parsed =
            ts_r_config::load_tsconfig(&ts_r_config::RealFs::new(), &cfg.to_string_lossy());
        let n = emit_js(&parsed);
        let dep = std::fs::read_to_string(dir.join("dist/lib/dep.js"));
        let index = dir.join("dist/index.js").is_file();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(n, Ok(2));
        assert!(index);
        assert!(dep.unwrap().contains("exports.dep = 1"));
    }
}
