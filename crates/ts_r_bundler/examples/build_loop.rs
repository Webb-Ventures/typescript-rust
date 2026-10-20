//! Repeats a cold (or warm) bundle in one process, for profiling and in-process timing.
//!
//! `cargo run --release -p ts_r_bundler --example build_loop -- <working-dir> <entry> [runs] [--warm] [--minify] [--edit FILE] [--format=esm|cjs|iife] [--loader:.EXT=LOADER]...`
//!
//! `--edit FILE` (relative to the working directory) is the incremental benchmark: one
//! initial build, then `runs` times append a comment to FILE, `invalidate` it and rebuild,
//! like a watch-mode rebuild after a save. FILE is restored afterwards.

ts_r_testing::cap_memory!();

use std::sync::Arc;
use std::time::Instant;

use ts_r_bundler::{BuildContext, BuildOptions, EntryPoint, Format, Loader};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(wd), Some(entry)) = (args.first(), args.get(1)) else {
        eprintln!("usage: build_loop <working-dir> <entry> [runs] [--warm] [--minify]");
        std::process::exit(2);
    };
    let runs: usize = args.get(2).and_then(|r| r.parse().ok()).unwrap_or(10);
    let warm = args.iter().any(|a| a == "--warm");
    let minify = args.iter().any(|a| a == "--minify");
    let edit = args
        .iter()
        .position(|a| a == "--edit")
        .and_then(|i| args.get(i + 1))
        .map(|f| (format!("{}/{f}", wd.trim_end_matches('/')), f.clone()));
    // esbuild's spelling: `--loader:.woff2=file`, `--format=esm`.
    let mut loaders = rustc_hash::FxHashMap::default();
    let mut format = None;
    for a in &args {
        if let Some((ext, loader)) = a.strip_prefix("--loader:").and_then(|l| l.split_once('=')) {
            let Some(loader) = Loader::parse(loader) else {
                eprintln!("unknown loader {loader}");
                std::process::exit(2);
            };
            loaders.insert(ext.to_string(), loader);
        } else if let Some(f) = a.strip_prefix("--format=") {
            format = Format::parse(f);
            if format.is_none() {
                eprintln!("unknown format {f}");
                std::process::exit(2);
            }
        }
    }
    let opts = BuildOptions {
        entry_points: vec![EntryPoint {
            path: entry.clone(),
            name: None,
        }],
        working_dir: wd.clone(),
        bundle: true,
        outfile: Some("out.js".into()),
        write: false,
        minify_whitespace: minify,
        minify_identifiers: minify,
        minify_syntax: minify,
        loaders,
        format,
        jsx: ts_r_bundler::JsxOptions {
            mode: Some("automatic".into()),
            ..Default::default()
        },
        ..BuildOptions::default()
    };
    let fs: Arc<dyn ts_r_config::FileSystem> = Arc::new(ts_r_config::RealFs::new());
    let mut ctx = BuildContext::new(opts.clone(), Arc::clone(&fs)).expect("options");
    let mut times = Vec::new();
    if let Some((file, rel)) = edit {
        let original = std::fs::read_to_string(&file).expect("read the edited file");
        let first = ctx.build();
        check(&first);
        for i in 0..runs {
            std::fs::write(&file, format!("{original}\n// edit {i}\n")).expect("write");
            let t = Instant::now();
            ctx.invalidate(std::slice::from_ref(&rel));
            let r = ctx.build();
            times.push(t.elapsed().as_secs_f64() * 1000.0);
            check(&r);
        }
        std::fs::write(&file, original).expect("restore");
        report(runs, times);
        return;
    }
    for _ in 0..runs {
        if !warm {
            ctx = BuildContext::new(opts.clone(), Arc::clone(&fs)).expect("options");
        }
        let t = Instant::now();
        let r = ctx.build();
        times.push(t.elapsed().as_secs_f64() * 1000.0);
        check(&r);
    }
    report(runs, times);
}

fn check(r: &ts_r_bundler::BuildResult) {
    if r.has_errors() {
        for m in r.errors() {
            eprintln!("{m}");
        }
        std::process::exit(1);
    }
}

fn report(runs: usize, mut times: Vec<f64>) {
    if times.is_empty() {
        return;
    }
    times.sort_by(f64::total_cmp);
    println!(
        "runs {runs}: min {:.1}ms median {:.1}ms",
        times[0],
        times[times.len() / 2]
    );
}
