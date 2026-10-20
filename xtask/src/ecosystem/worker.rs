//! One package, one stage. The driver runs `xtask ecosystem worker <stage> ...` under
//! `scripts/limit.sh` (2048 MB / 120 s for the whole process tree) and reads the JSON
//! [`StageResult`] it writes to `--out`. Progress (`at <file>`) goes to stderr so the driver
//! can name the file that was being processed when a limit killed the worker.
//!
//! - parse: every JS/TS file parses with `ts_r_parser`; files with errors are re-checked
//!   with the oracle (`node --check` for .js/.mjs/.cjs, tsc 7 `--noCheck` otherwise), and
//!   only files the oracle accepts count as failures.
//! - transform: TS sources (not `.d.ts`) transpile without errors, the output reparses, and
//!   its AST shape matches tsc 7's emit for the same tsconfig.
//! - bundle: each entry point (Node's resolution of the package's exports) is bundled by
//!   esbuild and ts-r-build, unminified and minified; each bundle runs in Node and the
//!   exported shape (keys and typeof) must match esbuild's (or the unbundled module's).
//! - check: `import * as m from "<pkg>"` with `skipLibCheck: false`, ts-r-check against tsc 7.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::{Duration, Instant};

use serde::Deserialize;
use ts_r_parser::ts_r_ast::SourceKind;

use super::proc;
use super::report::{StageResult, Status, signature, truncate};
use crate::args::Args;
use crate::util::{Result, root};

/// Time the worker gives itself before it stops starting new work (the hard limit is
/// limit.sh's).
const SOFT_BUDGET: Duration = Duration::from_secs(100);
/// Files larger than this are not parsed (minified bundles above it are rare and slow the
/// oracle down).
const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
/// At most this many TS files per package are compared with tsc (one tsc run each).
const MAX_TSC_FILES: u32 = 300;
/// At most this many entry points per package are bundled.
const MAX_ENTRIES: usize = 6;
/// Output kept from a tsc or ts-r-check run whose diagnostics are compared as a set (well
/// inside the worker's 2048 MB); a longer stream makes the comparison incomplete.
const DIAG_CAPTURE: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Tools {
    pub node: String,
    pub esbuild: PathBuf,
    pub tsc: PathBuf,
    pub ts_r_build: PathBuf,
    pub ts_r_check: PathBuf,
    pub lib_dir: Option<PathBuf>,
    pub js_dir: PathBuf,
}

impl Tools {
    pub fn new(bin_dir: &Path) -> Tools {
        let root = root();
        let node_bin = root.join("bench/node_modules/.bin");
        let lib_dir = std::fs::read_dir(root.join("bench/node_modules/@typescript"))
            .ok()
            .and_then(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| e.path().join("lib"))
                    .find(|p| p.join("lib.d.ts").is_file())
            });
        let bin = if bin_dir.is_absolute() {
            bin_dir.to_path_buf()
        } else {
            root.join(bin_dir)
        };
        Tools {
            node: "node".into(),
            esbuild: node_bin.join("esbuild"),
            tsc: node_bin.join("tsc"),
            ts_r_build: bin.join("ts-r-build"),
            ts_r_check: bin.join("ts-r-check"),
            lib_dir,
            js_dir: root.join("ecosystem/js"),
        }
    }
}

struct Ctx {
    name: String,
    pkg_dir: PathBuf,
    install_dir: Option<PathBuf>,
    tmp: PathBuf,
    tools: Tools,
    start: Instant,
}

impl Ctx {
    fn over_budget(&self) -> bool {
        self.start.elapsed() > SOFT_BUDGET
    }

    fn remaining(&self) -> Duration {
        SOFT_BUDGET
            .saturating_sub(self.start.elapsed())
            .max(Duration::from_secs(5))
    }
}

/// `xtask ecosystem worker <stage> --name N --pkg-dir D --tmp T [--install-dir I]
/// [--bin-dir B] --out F`.
pub fn run(mut args: Args) -> Result<ExitCode> {
    let name = args.opt("name")?.ok_or("--name is required")?;
    let pkg_dir = PathBuf::from(args.opt("pkg-dir")?.ok_or("--pkg-dir is required")?);
    let tmp = PathBuf::from(args.opt("tmp")?.ok_or("--tmp is required")?);
    let install_dir = args.opt("install-dir")?.map(PathBuf::from);
    let bin_dir = PathBuf::from(
        args.opt("bin-dir")?
            .unwrap_or_else(|| "target/release".into()),
    );
    let out = PathBuf::from(args.opt("out")?.ok_or("--out is required")?);
    let stage = args.positional().ok_or("worker: missing stage")?;
    args.finish()?;
    std::fs::create_dir_all(&tmp)?;
    let ctx = Ctx {
        name,
        pkg_dir,
        install_dir,
        tmp,
        tools: Tools::new(&bin_dir),
        start: Instant::now(),
    };
    let mut result = match stage.as_str() {
        "parse" => parse_stage(&ctx),
        "transform" => transform_stage(&ctx),
        "bundle" => bundle_stage(&ctx),
        "check" => check_stage(&ctx),
        other => return Err(format!("worker: unknown stage {other}").into()),
    };
    result.ms = ctx.start.elapsed().as_millis() as u64;
    std::fs::write(&out, serde_json::to_string(&result)?)?;
    Ok(ExitCode::SUCCESS)
}

// ---------------------------------------------------------------------------------------
// Files

const PARSE_EXTS: &[&str] = &["js", "mjs", "cjs", "jsx", "ts", "mts", "cts", "tsx"];

/// Package-relative paths of files with one of `exts`, sorted, skipping `node_modules` and
/// symlinks.
fn walk(dir: &Path, exts: &[&str]) -> Vec<String> {
    fn go(base: &Path, dir: &Path, exts: &[&str], out: &mut Vec<String>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<_> = rd.filter_map(|e| e.ok()).collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let Ok(ft) = e.file_type() else { continue };
            let p = e.path();
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                if e.file_name() != "node_modules" {
                    go(base, &p, exts, out);
                }
            } else if ft.is_file() {
                let ext = p.extension().and_then(|x| x.to_str()).unwrap_or("");
                if exts.contains(&ext)
                    && let Ok(rel) = p.strip_prefix(base)
                {
                    out.push(rel.to_string_lossy().replace('\\', "/"));
                }
            }
        }
    }
    let mut out = Vec::new();
    go(dir, dir, exts, &mut out);
    out
}

fn is_dts(rel: &str) -> bool {
    SourceKind::from_path(rel) == SourceKind::Dts
}

fn read_source(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > MAX_FILE_BYTES {
        return None;
    }
    String::from_utf8(std::fs::read(path).ok()?).ok()
}

fn line_of(src: &str, offset: u32) -> usize {
    let end = (offset as usize).min(src.len());
    src.as_bytes()[..end]
        .iter()
        .filter(|b| **b == b'\n')
        .count()
        + 1
}

// ---------------------------------------------------------------------------------------
// parse

fn parse_stage(ctx: &Ctx) -> StageResult {
    let mut r = StageResult::default();
    let files = walk(&ctx.pkg_dir, PARSE_EXTS);
    let mut bytes = 0usize;
    let mut skipped = 0usize;
    let mut parse_time = Duration::ZERO;
    // (rel, cause, detail)
    let mut candidates: Vec<(String, String, String)> = Vec::new();
    for rel in &files {
        if ctx.over_budget() {
            r.note = format!(
                "soft time budget reached after {} of {} files",
                r.checked,
                files.len()
            );
            break;
        }
        let path = ctx.pkg_dir.join(rel);
        let Some(src) = read_source(&path) else {
            skipped += 1;
            continue;
        };
        eprintln!("at {rel}");
        let t = Instant::now();
        let parsed = ts_r_parser::parse(&src, SourceKind::from_path(rel));
        parse_time += t.elapsed();
        bytes += src.len();
        r.checked += 1;
        if let Some(d) = parsed.diagnostics.iter().find(|d| d.is_error()) {
            let cause = signature(&format!("TS{}: {}", d.code, d.message));
            let detail = format!(
                "line {}: TS{}: {}",
                line_of(&src, d.span.start),
                d.code,
                d.message
            );
            candidates.push((rel.clone(), cause, detail));
        }
    }
    r.metrics.insert("files".into(), r.checked as f64);
    r.metrics.insert("bytes".into(), bytes as f64);
    r.metrics
        .insert("parse_ms".into(), parse_time.as_secs_f64() * 1000.0);
    if skipped > 0 {
        r.metrics.insert("skipped_files".into(), skipped as f64);
    }

    // Oracle: only files node / tsc accept are failures.
    let (js, other): (Vec<_>, Vec<_>) = candidates
        .into_iter()
        .partition(|(rel, _, _)| matches!(rel.rsplit('.').next(), Some("js" | "mjs" | "cjs")));
    let mut rejected_by_oracle = 0usize;
    for (rel, cause, detail) in js {
        if ctx.over_budget() {
            r.fail(
                cause,
                rel,
                format!("{detail} (oracle not run: time budget)"),
            );
            continue;
        }
        let o = proc::run(
            Command::new(&ctx.tools.node)
                .arg("--check")
                .arg(ctx.pkg_dir.join(&rel)),
            Duration::from_secs(30),
        );
        if o.ok() {
            r.fail(cause, rel, detail);
        } else {
            rejected_by_oracle += 1;
        }
    }
    if !other.is_empty() {
        let files: Vec<String> = other.iter().map(|(rel, _, _)| rel.clone()).collect();
        match tsc_syntax_rejects(ctx, &files) {
            Some(rejected) => {
                for (rel, cause, detail) in other {
                    if rejected.contains(&rel) {
                        rejected_by_oracle += 1;
                    } else {
                        r.fail(cause, rel, detail);
                    }
                }
            }
            None => {
                for (rel, cause, detail) in other {
                    r.fail(cause, rel, format!("{detail} (tsc oracle unavailable)"));
                }
            }
        }
    }
    if rejected_by_oracle > 0 {
        r.metrics
            .insert("oracle_rejected_files".into(), rejected_by_oracle as f64);
    }
    r.finish();
    r
}

/// Files (package-relative) for which tsc reports a syntax error (TS1xxx, TS17xxx, TS8xxx).
fn tsc_syntax_rejects(ctx: &Ctx, files: &[String]) -> Option<BTreeSet<String>> {
    let dir = ctx.tmp.join("tsc-parse");
    std::fs::create_dir_all(&dir).ok()?;
    let abs: Vec<String> = files
        .iter()
        .map(|f| ctx.pkg_dir.join(f).to_string_lossy().into_owned())
        .collect();
    let config = serde_json::json!({
        "compilerOptions": {
            "noEmit": true, "noCheck": true, "noResolve": true, "allowJs": true,
            "types": [], "jsx": "preserve", "skipLibCheck": true, "target": "esnext",
            "module": "esnext", "moduleResolution": "bundler"
        },
        "files": abs,
    });
    let cfg = dir.join("tsconfig.json");
    std::fs::write(&cfg, config.to_string()).ok()?;
    let o = proc::run_capturing(
        // tsc prints paths relative to its working directory; resolve them against `dir` below.
        Command::new(&ctx.tools.tsc)
            .current_dir(&dir)
            .arg("-p")
            .arg(&cfg)
            .args(["--pretty", "false", "--singleThreaded"]),
        ctx.remaining(),
        DIAG_CAPTURE,
    );
    if o.timed_out || o.code.is_none() || !o.stdout_complete {
        return None;
    }
    let mut out = BTreeSet::new();
    for d in crate::oracle::parse_tsc_output(&o.stdout, "") {
        let syntax = matches!(d.code, 1000..=1999 | 8000..=8999 | 17000..=18999);
        if !syntax {
            continue;
        }
        let p = Path::new(&d.file);
        let abs = if p.is_absolute() {
            p.to_path_buf()
        } else {
            dir.join(p)
        };
        let abs = std::fs::canonicalize(&abs).unwrap_or(abs);
        let base = std::fs::canonicalize(&ctx.pkg_dir).unwrap_or(ctx.pkg_dir.clone());
        if let Ok(rel) = abs.strip_prefix(&base) {
            out.insert(rel.to_string_lossy().replace('\\', "/"));
        }
    }
    Some(out)
}

// ---------------------------------------------------------------------------------------
// transform

fn ts_source(rel: &str) -> bool {
    matches!(rel.rsplit('.').next(), Some("ts" | "tsx" | "mts" | "cts")) && !is_dts(rel)
}

fn js_name(rel: &str) -> String {
    for (from, to) in [
        (".mts", ".mjs"),
        (".cts", ".cjs"),
        (".tsx", ".js"),
        (".ts", ".js"),
    ] {
        if let Some(stem) = rel.strip_suffix(from) {
            return format!("{stem}{to}");
        }
    }
    rel.to_string()
}

fn transform_stage(ctx: &Ctx) -> StageResult {
    use ts_r_codegen::CodegenOptions;
    use ts_r_transform::{TransformOptions, transpile};

    let files: Vec<String> = walk(&ctx.pkg_dir, &["ts", "tsx", "mts", "cts"])
        .into_iter()
        .filter(|f| ts_source(f))
        .collect();
    if files.is_empty() {
        return StageResult::skip("no TypeScript sources");
    }
    let mut r = StageResult::default();
    let dir = ctx.tmp.join("transform");
    let out_dir = dir.join("tsc-out");
    let _ = std::fs::create_dir_all(&dir);

    // One tsconfig read by both tools; it extends the package's own when that one loads.
    let fs = ts_r_config::RealFs::new();
    let pkg_cfg = ctx.pkg_dir.join("tsconfig.json");
    let extends = pkg_cfg.is_file() && {
        let parsed = ts_r_config::load_tsconfig(&fs, &pkg_cfg.to_string_lossy());
        parsed.diagnostics.is_empty()
    };
    let abs: Vec<String> = files
        .iter()
        .map(|f| ctx.pkg_dir.join(f).to_string_lossy().into_owned())
        .collect();
    let mut config = serde_json::json!({
        "compilerOptions": {
            "noCheck": true, "noResolve": true, "noEmit": false, "noEmitOnError": false,
            "declaration": false, "declarationMap": false, "sourceMap": false, "inlineSourceMap": false,
            "composite": false, "incremental": false, "emitDeclarationOnly": false,
            "skipLibCheck": true, "types": [],
            "outDir": out_dir.to_string_lossy(), "rootDir": ctx.pkg_dir.to_string_lossy(),
        },
        "files": [],
    });
    if extends {
        config["extends"] = serde_json::Value::String(pkg_cfg.to_string_lossy().into_owned());
        r.note = "uses the package's tsconfig.json".into();
    } else {
        let o = &mut config["compilerOptions"];
        o["target"] = "es2022".into();
        o["module"] = "esnext".into();
        o["moduleResolution"] = "bundler".into();
        o["jsx"] = "react-jsx".into();
    }
    let cfg_path = dir.join("tsconfig.json");
    let write_cfg = |file: &str| {
        let mut c = config.clone();
        c["files"] = serde_json::json!([file]);
        std::fs::write(&cfg_path, c.to_string())
    };
    if write_cfg(&abs[0]).is_err() {
        return StageResult::error("setup: cannot write tsconfig", "", "");
    }
    let parsed = ts_r_config::load_tsconfig(&fs, &cfg_path.to_string_lossy());

    let pkg_type_module = package_type_module(&ctx.pkg_dir);
    let mut compared = 0u32;
    let mut tsc_failed = 0u32;
    for (rel, abs) in files.iter().zip(&abs) {
        if ctx.over_budget() || compared >= MAX_TSC_FILES {
            r.note = format!(
                "compared {compared} of {} files (time or file cap)",
                files.len()
            );
            break;
        }
        let Some(src) = read_source(Path::new(abs)) else {
            continue;
        };
        eprintln!("at {rel}");
        let cjs_file = rel.ends_with(".cts") || (!rel.ends_with(".mts") && !pkg_type_module);
        let opts = TransformOptions::from_compiler_options(&parsed.options, abs, cjs_file);
        let ours = transpile(abs, &src, &opts, &CodegenOptions::default());
        if ours.has_errors() {
            // Syntax errors belong to the parse stage.
            continue;
        }
        r.checked += 1;
        let jsx_out = rel.ends_with(".tsx") && ours.code.contains('<');
        let kind = if jsx_out {
            SourceKind::Jsx
        } else {
            SourceKind::Js
        };
        let reparsed = ts_r_parser::parse(&ours.code, kind);
        if let Some(d) = reparsed.diagnostics.iter().find(|d| d.is_error()) {
            r.fail(
                format!("output does not reparse: TS{}", d.code),
                rel.clone(),
                format!("line {}: {}", line_of(&ours.code, d.span.start), d.message),
            );
            continue;
        }
        // tsc on this file alone (like `transpileModule`): no other file's types can
        // inline const enums or elide re-exports, exactly what a transpiler sees.
        let theirs_path = out_dir.join(js_name(rel));
        let _ = std::fs::remove_file(&theirs_path);
        if write_cfg(abs).is_err() {
            continue;
        }
        let tsc = proc::run(
            Command::new(&ctx.tools.tsc).arg("-p").arg(&cfg_path).args([
                "--pretty",
                "false",
                "--singleThreaded",
            ]),
            Duration::from_secs(20).min(ctx.remaining()),
        );
        if tsc.timed_out || tsc.code.is_none() {
            tsc_failed += 1;
            continue;
        }
        let theirs = std::fs::read_to_string(&theirs_path)
            .or_else(|_| std::fs::read_to_string(theirs_path.with_extension("jsx")));
        let Ok(theirs) = theirs else {
            tsc_failed += 1;
            continue;
        };
        compared += 1;
        if let Some((cause, detail)) = compare_js(&ours.code, &theirs) {
            r.fail(cause, rel.clone(), detail);
        }
    }
    r.metrics
        .insert("compared_with_tsc".into(), f64::from(compared));
    if tsc_failed > 0 {
        r.metrics
            .insert("tsc_no_output".into(), f64::from(tsc_failed));
    }
    r.finish();
    r
}

fn package_type_module(dir: &Path) -> bool {
    std::fs::read_to_string(dir.join("package.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .is_some_and(|v| v["type"] == "module")
}

/// Compares two JavaScript outputs by AST shape (comments dropped). Returns a cause (the
/// node kinds at the first difference) and a short diff.
pub fn compare_js(ours: &str, theirs: &str) -> Option<(String, String)> {
    let shape = |js: &str| {
        let js = blank_jsx_file_name(js);
        let kind = if ts_r_parser::parse(&js, SourceKind::Js)
            .diagnostics
            .iter()
            .any(|d| d.is_error())
        {
            SourceKind::Jsx
        } else {
            SourceKind::Js
        };
        let parsed = ts_r_parser::parse(&js, kind);
        ts_r_codegen::shape::shape(&parsed.ast)
    };
    let (a, b) = (shape(ours), shape(theirs));
    if a == b {
        return None;
    }
    let al: Vec<&str> = a.lines().collect();
    let bl: Vec<&str> = b.lines().collect();
    let i = al.iter().zip(&bl).take_while(|(x, y)| x == y).count();
    let node_kind = |l: Option<&&str>| {
        l.map(|l| {
            let l = l.trim();
            let after = l.split_once(": ").map_or(l, |(_, r)| r);
            after.split_whitespace().next().unwrap_or("").to_string()
        })
        .unwrap_or_else(|| "<end>".into())
    };
    let cause = format!(
        "emit differs: tsc {} vs ours {}",
        node_kind(bl.get(i)),
        node_kind(al.get(i))
    );
    let detail = format!(
        "tsc:  {}\nours: {}",
        truncate(bl.get(i).map_or("<end>", |s| s.trim()), 200),
        truncate(al.get(i).map_or("<end>", |s| s.trim()), 200)
    );
    Some((cause, detail))
}

/// `const _jsxFileName = "<abs path>"` differs between tools' temp paths.
fn blank_jsx_file_name(js: &str) -> String {
    const NEEDLE: &str = "const _jsxFileName = \"";
    match js.find(NEEDLE) {
        Some(i) => {
            let start = i + NEEDLE.len();
            let end = js[start..].find('"').map_or(start, |e| start + e);
            format!("{}{}", &js[..start], &js[end..])
        }
        None => js.to_owned(),
    }
}

// ---------------------------------------------------------------------------------------
// bundle

#[derive(Debug, Deserialize)]
struct Resolved {
    spec: String,
    require: Option<String>,
    import: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct Shape {
    #[serde(default)]
    error: Option<String>,
    #[serde(default, rename = "type")]
    ty: String,
    #[serde(default)]
    keys: Vec<(String, String)>,
}

/// Specifiers for a package's entry points: `exports` subpaths without wildcards (`.` first),
/// or the bare name.
pub fn entry_specs(name: &str, pj: &serde_json::Value) -> Vec<String> {
    let mut subpaths: Vec<String> = Vec::new();
    match &pj["exports"] {
        serde_json::Value::Object(map) if map.keys().any(|k| k.starts_with('.')) => {
            for k in map.keys() {
                if k.contains('*')
                    || k == "./package.json"
                    || k.ends_with(".json")
                    || k.ends_with(".css")
                {
                    continue;
                }
                subpaths.push(k.clone());
            }
        }
        serde_json::Value::Null => subpaths.push(".".into()),
        _ => subpaths.push(".".into()),
    }
    subpaths.sort_by_key(|k| (k != ".", k.clone()));
    subpaths
        .into_iter()
        .map(|k| {
            if k == "." {
                name.to_string()
            } else {
                format!("{name}/{}", k.trim_start_matches("./"))
            }
        })
        .collect()
}

fn module_format(file: &Path, stop: &Path) -> &'static str {
    match file.extension().and_then(|e| e.to_str()) {
        Some("mjs") => return "esm",
        Some("cjs") => return "cjs",
        _ => {}
    }
    let mut dir = file.parent();
    while let Some(d) = dir {
        let pj = d.join("package.json");
        if let Ok(text) = std::fs::read_to_string(&pj)
            && let Ok(v) = serde_json::from_str::<serde_json::Value>(&text)
        {
            return if v["type"] == "module" { "esm" } else { "cjs" };
        }
        if d == stop {
            break;
        }
        dir = d.parent();
    }
    "cjs"
}

fn last_json_line<T: for<'de> Deserialize<'de>>(stdout: &str) -> Option<T> {
    stdout
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| l.starts_with('{') || l.starts_with('['))
        .and_then(|l| serde_json::from_str(l).ok())
}

fn run_shape(ctx: &Ctx, file: &Path, format: &str, cwd: &Path) -> Shape {
    let o = proc::run(
        Command::new(&ctx.tools.node)
            .arg(ctx.tools.js_dir.join("shape.mjs"))
            .arg(file)
            .arg(format)
            .current_dir(cwd),
        Duration::from_secs(20).min(ctx.remaining()),
    );
    if o.timed_out {
        return Shape {
            error: Some("timeout: module did not load in 20 s".into()),
            ty: String::new(),
            keys: vec![],
        };
    }
    last_json_line(&o.stdout).unwrap_or_else(|| Shape {
        error: Some(format!(
            "no shape printed: {}",
            truncate(&o.first_line(), 200)
        )),
        ty: String::new(),
        keys: vec![],
    })
}

/// The first error line of a bundler's output.
fn build_error(o: &proc::Output) -> String {
    if o.timed_out {
        return "timeout".into();
    }
    let text = format!("{}\n{}", o.stderr, o.stdout);
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| l.contains("ERROR") || l.contains("error") || l.contains("panicked"))
        .map(String::from)
        .unwrap_or_else(|| o.first_line());
    let line = line
        .trim_start_matches(['✘', ' ', 'X'])
        .trim_start_matches("[ERROR]")
        .trim();
    if o.code.is_none() {
        format!("crashed: {line}")
    } else {
        line.to_string()
    }
}

fn shape_diff(ours: &Shape, theirs: &Shape) -> (String, String) {
    if ours.ty != theirs.ty {
        return (
            "module type differs".into(),
            format!("ours {} vs esbuild {}", ours.ty, theirs.ty),
        );
    }
    let a: BTreeMap<&str, &str> = ours
        .keys
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let b: BTreeMap<&str, &str> = theirs
        .keys
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let missing: Vec<&str> = b.keys().filter(|k| !a.contains_key(*k)).copied().collect();
    let extra: Vec<&str> = a.keys().filter(|k| !b.contains_key(*k)).copied().collect();
    let changed: Vec<String> = a
        .iter()
        .filter_map(|(k, v)| {
            b.get(k)
                .filter(|w| *w != v)
                .map(|w| format!("{k}: {v} vs {w}"))
        })
        .collect();
    let cause = if !missing.is_empty() {
        "missing exports"
    } else if !extra.is_empty() {
        "extra exports"
    } else {
        "export types differ"
    };
    let detail = format!(
        "missing [{}] extra [{}] typeof [{}]",
        truncate(&missing.join(","), 150),
        truncate(&extra.join(","), 150),
        truncate(&changed.join(", "), 200)
    );
    (cause.into(), detail)
}

fn bundle_stage(ctx: &Ctx) -> StageResult {
    let Some(install) = &ctx.install_dir else {
        return StageResult::error("setup: no install", "", "");
    };
    let pkg = install.join("node_modules").join(&ctx.name);
    let pj: serde_json::Value = match std::fs::read_to_string(pkg.join("package.json")) {
        Ok(t) => serde_json::from_str(&t).unwrap_or_default(),
        Err(e) => return StageResult::error("setup: package not installed", "", &e.to_string()),
    };
    let specs = entry_specs(&ctx.name, &pj);
    let o = proc::run(
        Command::new(&ctx.tools.node)
            .arg("--experimental-import-meta-resolve")
            .arg("--no-warnings")
            .arg(ctx.tools.js_dir.join("resolve.mjs"))
            .arg(install)
            .args(&specs),
        Duration::from_secs(30),
    );
    let resolved: Vec<Resolved> = last_json_line(&o.stdout).unwrap_or_default();
    let mut entries: Vec<(String, PathBuf, &'static str)> = Vec::new();
    for r in resolved {
        let Some(file) = r.require.or(r.import).map(PathBuf::from) else {
            continue;
        };
        let ext = file.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !matches!(ext, "js" | "cjs" | "mjs") || !file.is_file() {
            continue;
        }
        if entries.iter().any(|(_, f, _)| *f == file) {
            continue;
        }
        let format = module_format(&file, install);
        entries.push((r.spec, file, format));
        if entries.len() >= MAX_ENTRIES {
            break;
        }
    }
    if entries.is_empty() {
        return StageResult::skip("no JavaScript entry point");
    }

    let out_dir = ctx.tmp.join("bundle-out");
    let _ = std::fs::create_dir_all(&out_dir);
    let mut r = StageResult::default();
    let mut oracle_failed = 0u32;
    let mut sizes: BTreeMap<&str, f64> = BTreeMap::new();
    for (i, (spec, file, format)) in entries.iter().enumerate() {
        if ctx.over_budget() {
            r.note = format!(
                "soft time budget reached after {i} of {} entries",
                entries.len()
            );
            break;
        }
        eprintln!("at {spec}");
        let ext = if *format == "esm" { "mjs" } else { "cjs" };
        let direct = run_shape(ctx, file, format, &out_dir);
        let mut entry_failed = false;
        for minify in [false, true] {
            let tag = if minify { "min" } else { "dev" };
            let esb_out = out_dir.join(format!("{i}-esbuild-{tag}.{ext}"));
            let tsr_out = out_dir.join(format!("{i}-ts-r-{tag}.{ext}"));
            let esb = build(
                ctx,
                &ctx.tools.esbuild,
                file,
                format,
                minify,
                &esb_out,
                install,
                false,
            );
            if !esb.ok() || !esb_out.is_file() {
                oracle_failed += 1;
                break;
            }
            let esb_shape = run_shape(ctx, &esb_out, format, &out_dir);
            if esb_shape.error.is_some() {
                oracle_failed += 1;
                break;
            }
            if !minify {
                r.checked += 1;
            }
            let prefix = if minify { "minified: " } else { "" };
            let tsr = build(
                ctx,
                &ctx.tools.ts_r_build,
                file,
                format,
                minify,
                &tsr_out,
                install,
                true,
            );
            if !tsr.ok() || !tsr_out.is_file() {
                if !entry_failed {
                    r.fail(
                        format!("{prefix}build error: {}", signature(&build_error(&tsr))),
                        spec.clone(),
                        truncate(&build_error(&tsr), 400),
                    );
                }
                entry_failed = true;
                break;
            }
            if minify {
                let a = std::fs::metadata(&tsr_out).map(|m| m.len()).unwrap_or(0) as f64;
                let b = std::fs::metadata(&esb_out).map(|m| m.len()).unwrap_or(0) as f64;
                *sizes.entry("min_bytes_ts_r").or_default() += a;
                *sizes.entry("min_bytes_esbuild").or_default() += b;
            } else {
                let a = std::fs::metadata(&tsr_out).map(|m| m.len()).unwrap_or(0) as f64;
                let b = std::fs::metadata(&esb_out).map(|m| m.len()).unwrap_or(0) as f64;
                *sizes.entry("dev_bytes_ts_r").or_default() += a;
                *sizes.entry("dev_bytes_esbuild").or_default() += b;
            }
            let tsr_shape = run_shape(ctx, &tsr_out, format, &out_dir);
            if let Some(err) = &tsr_shape.error {
                if !entry_failed {
                    r.fail(
                        format!("{prefix}runtime error: {}", signature(err)),
                        spec.clone(),
                        truncate(err, 400),
                    );
                }
                entry_failed = true;
                break;
            }
            if tsr_shape != esb_shape && tsr_shape != direct {
                let (cause, detail) = shape_diff(&tsr_shape, &esb_shape);
                if !entry_failed {
                    r.fail(format!("{prefix}{cause}"), spec.clone(), detail);
                }
                entry_failed = true;
                break;
            }
        }
        let _ = entry_failed;
    }
    for (k, v) in sizes {
        r.metrics.insert(k.into(), v);
    }
    r.metrics.insert("entries".into(), entries.len() as f64);
    if oracle_failed > 0 {
        r.metrics
            .insert("esbuild_failed_entries".into(), f64::from(oracle_failed));
    }
    r.finish();
    if r.status == Status::Skip {
        r.note = "esbuild's own bundles fail to build or load".into();
    }
    r
}

#[allow(clippy::too_many_arguments)]
fn build(
    ctx: &Ctx,
    tool: &Path,
    entry: &Path,
    format: &str,
    minify: bool,
    out: &Path,
    cwd: &Path,
    ours: bool,
) -> proc::Output {
    let mut cmd = Command::new(tool);
    cmd.arg(entry)
        .arg("--bundle")
        .arg("--platform=node")
        .arg(format!("--format={format}"))
        .arg(format!("--outfile={}", out.display()))
        .arg("--log-level=error")
        .current_dir(cwd);
    if format == "esm" && !ours {
        // CommonJS dependencies inside esbuild's ESM bundles call require() for Node
        // built-ins, which needs this usual banner. ts-r-build adds its own `require` shim
        // for ESM on Node (and skips it when a banner declares `require`).
        cmd.arg("--banner:js=import { createRequire as __tsrEcoRequire } from 'module'; const require = __tsrEcoRequire(import.meta.url);");
    }
    if minify {
        cmd.arg("--minify");
    }
    if ours {
        cmd.arg("--no-cache")
            .arg("--threads=1")
            .env("TS_R_THREADS", "1");
    } else {
        cmd.env("GOMAXPROCS", "1");
    }
    proc::run(&mut cmd, Duration::from_secs(60).min(ctx.remaining()))
}

// ---------------------------------------------------------------------------------------
// check

fn has_types(pkg: &Path, pj: &serde_json::Value) -> bool {
    if pj.get("types").is_some() || pj.get("typings").is_some() {
        return true;
    }
    // A TypeScript `main` (some packages and the .ts repros): tsc type checks it too.
    if pj
        .get("main")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|m| [".ts", ".mts", ".cts"].iter().any(|e| m.ends_with(e)))
    {
        return true;
    }
    walk(pkg, &["ts", "mts", "cts"]).iter().any(|f| is_dts(f))
}

type DiagKey = (String, u32, u32, u32);

fn check_stage(ctx: &Ctx) -> StageResult {
    let Some(install) = &ctx.install_dir else {
        return StageResult::error("setup: no install", "", "");
    };
    let pkg = install.join("node_modules").join(&ctx.name);
    let pj: serde_json::Value = std::fs::read_to_string(pkg.join("package.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    if !has_types(&pkg, &pj) {
        return StageResult::skip("no .d.ts");
    }
    // @types/node is part of every package install (repros may leave it out).
    let types: Vec<&str> = if install.join("node_modules/@types/node").is_dir() {
        vec!["node"]
    } else {
        vec![]
    };
    let config = serde_json::json!({
        "compilerOptions": {
            "strict": true, "skipLibCheck": false, "noEmit": true, "target": "es2022",
            "module": "preserve", "moduleResolution": "bundler", "types": types,
            "esModuleInterop": true
        },
        "files": ["index.ts"],
    });
    let name_lit = serde_json::to_string(&ctx.name).unwrap_or_default();
    // `@types/x` packages are not importable (TS6137); load them the way they are meant to be.
    let index = match ctx.name.strip_prefix("@types/") {
        Some(lib) => format!("/// <reference types=\"{lib}\" />\nexport {{}};\n"),
        None => format!("import * as m from {name_lit};\nexport {{ m }};\n"),
    };
    if std::fs::write(install.join("tsconfig.json"), config.to_string()).is_err()
        || std::fs::write(install.join("index.ts"), index).is_err()
    {
        return StageResult::error("setup: cannot write project", "", "");
    }
    eprintln!("at tsc");
    let half = ctx.remaining() / 2;
    let tsc = proc::run_capturing(
        Command::new(&ctx.tools.tsc)
            .args([
                "-p",
                "tsconfig.json",
                "--pretty",
                "false",
                "--singleThreaded",
            ])
            .current_dir(install),
        half,
        DIAG_CAPTURE,
    );
    if tsc.timed_out || tsc.code.is_none() {
        return StageResult::skip(format!(
            "tsc did not finish: {}",
            truncate(&tsc.first_line(), 120)
        ));
    }
    // Diagnostics are compared as complete sets: a cut-off stream cannot be compared.
    if !tsc.stdout_complete {
        return StageResult::skip(format!(
            "tsc output incomplete (over {} MiB or left open)",
            DIAG_CAPTURE >> 20
        ));
    }
    eprintln!("at ts-r-check");
    let mut cmd = Command::new(&ctx.tools.ts_r_check);
    cmd.args(["-p", "tsconfig.json", "--pretty", "false", "--threads", "1"])
        .current_dir(install);
    if let Some(lib) = &ctx.tools.lib_dir {
        cmd.env("TS_R_LIB_DIR", lib);
    }
    let ours = proc::run_capturing(&mut cmd, ctx.remaining(), DIAG_CAPTURE);
    if ours.timed_out {
        return StageResult::error("limit: ts-r-check timeout", "index.ts", "");
    }
    if !ours.stdout_complete {
        return StageResult::error(
            &format!(
                "limit: ts-r-check output incomplete (over {} MiB or left open)",
                DIAG_CAPTURE >> 20
            ),
            "index.ts",
            "",
        );
    }
    if !matches!(ours.code, Some(0..=2)) {
        let line = ours
            .stderr
            .lines()
            .find(|l| l.contains("panicked"))
            .map(String::from)
            .unwrap_or_else(|| ours.first_line());
        return StageResult::error(&format!("crash: {}", signature(&line)), "index.ts", &line);
    }
    // Every project includes @types/node; differences inside it are reported once, for the
    // @types/node package, instead of for every package that is checked next to it.
    let own_types_node = ctx.name == "@types/node";
    let mut ignored = 0usize;
    let mut to_map = |text: &str| -> BTreeMap<DiagKey, String> {
        crate::oracle::parse_tsc_output(text, "")
            .into_iter()
            .filter(|d| {
                let keep = own_types_node || !d.file.contains("node_modules/@types/node/");
                ignored += usize::from(!keep);
                keep
            })
            .map(|d| ((d.file, d.line, d.col, d.code), d.message))
            .collect()
    };
    let theirs = to_map(&tsc.stdout);
    let mine = to_map(&ours.stdout);
    let mut r = StageResult {
        checked: theirs
            .keys()
            .chain(mine.keys())
            .collect::<BTreeSet<_>>()
            .len()
            .max(1) as u32,
        ..Default::default()
    };
    if ignored > 0 {
        r.metrics
            .insert("ignored_in_types_node".into(), ignored as f64);
    }
    r.metrics
        .insert("tsc_diagnostics".into(), theirs.len() as f64);
    r.metrics
        .insert("ts_r_diagnostics".into(), mine.len() as f64);
    r.metrics
        .insert("tsc_ms".into(), tsc.elapsed.as_secs_f64() * 1000.0);
    r.metrics
        .insert("ts_r_ms".into(), ours.elapsed.as_secs_f64() * 1000.0);
    // Messages can name absolute paths in the temporary install directory.
    let install_str = install.to_string_lossy().into_owned();
    let clean = |m: &str| -> String {
        let first = m.lines().next().unwrap_or("");
        first
            .replace(&format!("/private{install_str}"), "<install>")
            .replace(&install_str, "<install>")
    };
    for (k, msg) in &theirs {
        if !mine.contains_key(k) {
            let first = &clean(msg);
            r.fail(
                format!("missing TS{}: {}", k.3, signature(first)),
                format!("{}:{}:{}", short_path(&k.0), k.1, k.2),
                truncate(first, 300),
            );
        }
    }
    for (k, msg) in &mine {
        if !theirs.contains_key(k) {
            let first = &clean(msg);
            r.fail(
                format!("extra TS{}: {}", k.3, signature(first)),
                format!("{}:{}:{}", short_path(&k.0), k.1, k.2),
                truncate(first, 300),
            );
        }
    }
    r.finish();
    r
}

/// `node_modules/a/b.d.ts` → `a/b.d.ts`.
fn short_path(p: &str) -> String {
    p.rsplit_once("node_modules/")
        .map_or(p, |(_, r)| r)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Syntax errors that node / tsc also reject are not failures. Needs node and the pinned
    /// tsc in bench/node_modules; skipped without them.
    #[test]
    fn parse_stage_uses_the_oracle() {
        let tools = Tools::new(Path::new("target/release"));
        let has_node = Command::new("node")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success());
        if !tools.tsc.exists() || !has_node {
            return;
        }
        let dir = std::env::temp_dir().join(format!("ts-r-eco-parse-test-{}", std::process::id()));
        let pkg = dir.join("pkg");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(pkg.join("package.json"), "{}").unwrap();
        std::fs::write(pkg.join("a.js"), "let x = ;\n").unwrap();
        std::fs::write(pkg.join("b.ts"), "let y: = 1;\n").unwrap();
        std::fs::write(pkg.join("c.mjs"), "export const a = 1;\n").unwrap();
        let ctx = Ctx {
            name: "pkg".into(),
            pkg_dir: pkg,
            install_dir: None,
            tmp: dir.join("tmp"),
            tools,
            start: Instant::now(),
        };
        let r = parse_stage(&ctx);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            (r.checked, r.failed, r.status),
            (3, 0, Status::Pass),
            "{r:?}"
        );
        assert_eq!(r.metrics.get("oracle_rejected_files"), Some(&2.0));
    }

    #[test]
    fn entry_specs_from_exports() {
        let pj = serde_json::json!({"exports": {".": "./a.js", "./b": "./b.js", "./c/*": "./c/*.js", "./package.json": "./package.json"}});
        assert_eq!(entry_specs("x", &pj), vec!["x", "x/b"]);
        let pj = serde_json::json!({"exports": {"import": "./a.mjs", "require": "./a.cjs"}});
        assert_eq!(entry_specs("@s/x", &pj), vec!["@s/x"]);
        assert_eq!(
            entry_specs("y", &serde_json::json!({"main": "i.js"})),
            vec!["y"]
        );
    }

    #[test]
    fn js_compare() {
        assert!(compare_js("export const a = 1;", "export const a = 1; // c").is_none());
        let (cause, _) = compare_js("let a = 1;", "var a = 1;").unwrap();
        assert!(cause.starts_with("emit differs"), "{cause}");
    }

    #[test]
    fn shape_diffs() {
        let s = |keys: &[(&str, &str)]| Shape {
            error: None,
            ty: "object".into(),
            keys: keys
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect(),
        };
        assert_eq!(
            shape_diff(
                &s(&[("a", "function")]),
                &s(&[("a", "function"), ("b", "object")])
            )
            .0,
            "missing exports"
        );
        assert_eq!(
            shape_diff(&s(&[("a", "object")]), &s(&[("a", "function")])).0,
            "export types differ"
        );
    }
}
