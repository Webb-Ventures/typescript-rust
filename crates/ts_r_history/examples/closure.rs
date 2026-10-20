//! `closure <tsconfig.json> [file...]`: what re-checking one file needs. Loads the project,
//! times the lib files and the whole load, then per file (default: every checked file)
//! prints how many files (and bytes) its uses reach, and the time to parse and bind just
//! those. Run under `scripts/limit.sh`.

use std::time::Instant;

use ts_r_checker::program::bind_options_for;
use ts_r_checker::{LibStore, SourceFile, load_program, should_check};
use ts_r_config::{RealFs, load_tsconfig};
use ts_r_history::shape::ProgramShapes;

ts_r_testing::cap_memory!();

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(config) = args.first() else {
        eprintln!("usage: closure <tsconfig.json> [file...]");
        return;
    };
    let fs = RealFs::new();
    let parsed = load_tsconfig(&fs, config);
    let Some(libs) = LibStore::locate() else {
        eprintln!("lib files not found");
        return;
    };
    let t = Instant::now();
    let warm_libs = LibStore::new(libs.dir());
    let _ = load_program(&fs, &[], &parsed.options, Some(&warm_libs), 4);
    let lib_ms = t.elapsed().as_secs_f64() * 1e3;
    let t = Instant::now();
    let program = load_program(&fs, &parsed.file_names, &parsed.options, Some(&libs), 4);
    let load_ms = t.elapsed().as_secs_f64() * 1e3;
    let non_lib = program.files.iter().filter(|f| !f.is_lib).count();
    let bytes: usize = program
        .files
        .iter()
        .filter(|f| !f.is_lib)
        .map(|f| f.text.len())
        .sum();
    eprintln!(
        "libs {lib_ms:.1} ms; load {load_ms:.1} ms: {non_lib} files, {} KB",
        bytes / 1024
    );
    // Parse + bind of each lib file alone, one thread.
    for f in program.files.iter().filter(|f| f.is_lib) {
        let one = [(f.path.as_str(), f.text.as_str())];
        let t = Instant::now();
        let o = bind_options_for::<RealFs>(&f.path, &parsed.options, None);
        let _ = SourceFile::new(one[0].0.to_owned(), one[0].1.to_owned(), true, &o);
        eprintln!(
            "lib {}: {} KB, {:.1} ms",
            f.path.rsplit('/').next().unwrap_or(""),
            f.text.len() / 1024,
            t.elapsed().as_secs_f64() * 1e3
        );
    }
    // Parse + bind of every non-lib file, 4 threads, no resolution.
    let all: Vec<(&str, &str)> = program
        .files
        .iter()
        .filter(|f| !f.is_lib)
        .map(|f| (f.path.as_str(), f.text.as_str()))
        .collect();
    eprintln!(
        "parse+bind all non-lib (4 threads): {:.1} ms",
        parse_ms(&all, &parsed.options)
    );
    {
        // Verification parts, one thread.
        use ts_r_checker::program::{reference_target, resolve_request};
        use ts_r_config::FileSystem;
        let resolver = ts_r_resolver::Resolver::new(
            &fs,
            ts_r_resolver::ResolverOptions::from_compiler_options(&parsed.options),
        );
        let allow_js = parsed.options.effective().allow_js();
        let files: Vec<_> = program.files.iter().filter(|f| !f.is_lib).collect();
        let t = Instant::now();
        for f in &files {
            let _ = std::fs::metadata(&f.path);
        }
        let stat = t.elapsed().as_secs_f64() * 1e3;
        let t = Instant::now();
        for f in &files {
            let text = fs.read_file(&f.path).unwrap_or_default();
            let _ = blake3::hash(text.as_bytes());
        }
        let read = t.elapsed().as_secs_f64() * 1e3;
        let t = Instant::now();
        for f in &files {
            let _ = bind_options_for(&f.path, &parsed.options, Some(&resolver));
        }
        let bind = t.elapsed().as_secs_f64() * 1e3;
        let t = Instant::now();
        let mut n = 0;
        for f in &files {
            for req in &f.bound.module_requests {
                n += 1;
                let _ = resolve_request(
                    &resolver,
                    allow_js,
                    &f.path,
                    f.ast.atom(req.specifier),
                    req.kind,
                );
            }
        }
        let res = t.elapsed().as_secs_f64() * 1e3;
        let t = Instant::now();
        for f in &files {
            for (k, v) in &f.references {
                let _ = reference_target(&fs, &resolver, &f.path, k, v);
            }
        }
        let refs = t.elapsed().as_secs_f64() * 1e3;
        eprintln!(
            "verify, 1 thread: stat {stat:.1}, read+hash {read:.1}, bind {bind:.1}, resolve {res:.1} ({n} requests), references {refs:.1} ms"
        );
    }
    let shapes = ProgramShapes::build(&program, 4);
    let deep = shapes.deep();
    let targets: Vec<&str> = if args.len() > 1 {
        args[1..].iter().map(String::as_str).collect()
    } else {
        (0..program.files.len())
            .filter(|&i| should_check(&program, i))
            .map(|i| program.files[i].path.as_str())
            .collect()
    };
    for t in targets {
        let Some(s) = shapes.shapes.get(t) else {
            eprintln!("{t}: not in the program");
            continue;
        };
        let reached = deep.files_reached(s.uses.iter());
        let files: Vec<(&str, &str)> = program
            .files
            .iter()
            .filter(|f| !f.is_lib && (reached.contains(f.path.as_str()) || f.path == t))
            .map(|f| (f.path.as_str(), f.text.as_str()))
            .collect();
        let kb: usize = files.iter().map(|(_, s)| s.len()).sum::<usize>() / 1024;
        eprintln!(
            "{t}: reaches {} files, {kb} KB, parse {:.1} ms",
            files.len(),
            parse_ms(&files, &parsed.options)
        );
    }
}

fn parse_ms(files: &[(&str, &str)], options: &ts_r_config::CompilerOptions) -> f64 {
    let t = Instant::now();
    let next = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|s| {
        for _ in 0..4 {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some((p, text)) = files.get(i) else { break };
                    let o = bind_options_for::<RealFs>(p, options, None);
                    let _ = SourceFile::new((*p).to_owned(), (*text).to_owned(), false, &o);
                }
            });
        }
    });
    t.elapsed().as_secs_f64() * 1e3
}
