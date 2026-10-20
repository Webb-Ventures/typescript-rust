//! Compares ts-r's transpile output with tsc's for whole projects.
//!
//! ```text
//! scripts/limit.sh --mem-mb 4096 --time-s 600 -- \
//!   cargo run --release -p ts_r_transform --example project_oracle -- [--show N] <tsconfig.json>...
//! ```
//!
//! For each project, tsc emits every file (`--noCheck`, into a temporary directory, with
//! `rootDir` set to the project directory) and ts-r transpiles the same files with the
//! project's options. Outputs are compared by AST shape. Files that ts-r's output fails to
//! parse are counted separately.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use ts_r_codegen::CodegenOptions;
use ts_r_config::{RealFs, load_tsconfig};
use ts_r_transform::{TransformOptions, transpile};

ts_r_testing::cap_memory!();

#[path = "../tests/support/mod.rs"]
mod support;

fn tsc_command() -> Command {
    if let Ok(t) = std::env::var("TSC") {
        return Command::new(t);
    }
    let home = std::env::var("HOME").unwrap_or_default();
    for dir in ["1ac1eddd0355a233", "d08dbff2f92d3e31"] {
        let p = format!("{home}/.npm/_npx/{dir}/node_modules/.bin/tsc");
        if Path::new(&p).exists() {
            return Command::new(p);
        }
    }
    let mut c = Command::new("npx");
    c.args(["-y", "-p", "typescript@7.0.2", "tsc"]);
    c
}

fn main() {
    let mut configs = Vec::new();
    let mut show = 10usize;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--show" {
            show = args.next().and_then(|s| s.parse().ok()).unwrap_or(10);
        } else {
            configs.push(PathBuf::from(a));
        }
    }
    let fs = RealFs::new();
    let (mut ok, mut bad, mut missing, mut unparsable) = (0usize, 0usize, 0usize, 0usize);
    let mut shown = 0usize;
    let mut ours_time = Duration::ZERO;
    let mut bytes = 0usize;
    for cfg in &configs {
        let cfg = std::fs::canonicalize(cfg).unwrap_or(cfg.clone());
        let cfg_str = cfg.to_string_lossy().to_string();
        let parsed = load_tsconfig(&fs, &cfg_str);
        let dir = cfg.parent().unwrap_or(Path::new("/")).to_path_buf();
        let out_dir = std::env::temp_dir().join(format!(
            "ts-r-project-oracle-{}-{}",
            std::process::id(),
            support::hash_str(&cfg_str)
        ));
        let _ = std::fs::remove_dir_all(&out_dir);
        let status = tsc_command()
            .arg("-p")
            .arg(&cfg)
            .args([
                "--noEmit",
                "false",
                "--noCheck",
                "--declaration",
                "false",
                "--declarationMap",
                "false",
                "--sourceMap",
                "false",
                "--inlineSourceMap",
                "false",
                "--composite",
                "false",
                "--incremental",
                "false",
                "--emitDeclarationOnly",
                "false",
                "--noEmitOnError",
                "false",
                "--outDir",
            ])
            .arg(&out_dir)
            .arg("--rootDir")
            .arg(&dir)
            .output();
        if status.is_err() {
            eprintln!("cannot run tsc for {cfg_str}");
            continue;
        }
        let mut files = 0usize;
        for f in &parsed.file_names {
            let p = Path::new(f);
            if !support::is_fixture(p) || f.contains("/node_modules/") || f.contains("/.next/") {
                continue;
            }
            files += 1;
            let rel = p.strip_prefix(&dir).unwrap_or(p);
            let name = rel.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let js = out_dir.join(rel).with_file_name(support::js_name(name));
            let jsx = js.with_extension("jsx");
            let Ok(theirs) =
                std::fs::read_to_string(&js).or_else(|_| std::fs::read_to_string(&jsx))
            else {
                missing += 1;
                continue;
            };
            let src = std::fs::read_to_string(p).unwrap_or_default();
            bytes += src.len();
            let opts = TransformOptions::from_compiler_options(&parsed.options, f, false);
            let t = Instant::now();
            let ours = transpile(f, &src, &opts, &CodegenOptions::default());
            ours_time += t.elapsed();
            let reparsed = ts_r_parser::parse(&ours.code, ts_r_ast::SourceKind::Jsx);
            if reparsed.diagnostics.iter().any(|d| d.is_error()) {
                unparsable += 1;
            }
            match support::compare(&ours.code, &theirs) {
                None => ok += 1,
                Some(diff) => {
                    bad += 1;
                    if shown < show {
                        shown += 1;
                        println!("MISMATCH {f}\n{diff}");
                    }
                }
            }
        }
        let _ = std::fs::remove_dir_all(&out_dir);
        eprintln!("{cfg_str}: {files} files");
    }
    println!(
        "project oracle: {ok} match, {bad} mismatch, {missing} without tsc output, {unparsable} unparsable; ts-r transpile {:.1} ms for {} KiB",
        ours_time.as_secs_f64() * 1000.0,
        bytes / 1024
    );
    if bad > 0 || unparsable > 0 {
        std::process::exit(1);
    }
}
