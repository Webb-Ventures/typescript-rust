//! Binds a whole repository for crash-freedom and timing: every TS/JS source outside
//! `node_modules` plus every declaration file inside it (symlinks are not followed, so pnpm
//! stores are read once). Then links the files into one program through ts_r_resolver.
//!
//! ```text
//! cargo build --release -j 4 -p ts_r_binder --example corpus
//! scripts/limit.sh --mem-mb 4096 --time-s 600 -- /usr/bin/time -l \
//!     target/release/examples/corpus ../private-app
//! ```
//!
//! Prints file counts, parse/bind/link times, files/sec, binder diagnostics by code (in
//! project sources they are likely false positives, since the project compiles) and any
//! panics. `--list N` prints the first N project diagnostics.

ts_r_testing::cap_memory!();

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use ts_r_ast::{Ast, SourceKind};
use ts_r_binder::program::{ModuleTarget, Program, ProgramFile};
use ts_r_binder::{BindOptions, BoundFile, bind};
use ts_r_config::{CompilerOptions, RealFs};
use ts_r_resolver::{Resolver, ResolverOptions};
use ts_r_span::LineIndex;

const THREADS: usize = 4;

fn is_decl(name: &str) -> bool {
    name.ends_with(".d.ts") || name.ends_with(".d.mts") || name.ends_with(".d.cts")
}

fn is_source(name: &str) -> bool {
    [".ts", ".tsx", ".mts", ".cts", ".js", ".jsx", ".mjs", ".cjs"]
        .iter()
        .any(|e| name.ends_with(e))
}

/// Iterative walk (no recursion, no symlinks).
fn collect(root: &Path) -> Vec<(PathBuf, bool)> {
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), false)];
    while let Some((dir, in_nm)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            let name = e.file_name().to_string_lossy().into_owned();
            if ft.is_dir() {
                if matches!(name.as_str(), ".git" | ".turbo" | ".next" | ".cache") {
                    continue;
                }
                stack.push((e.path(), in_nm || name == "node_modules"));
            } else if ft.is_file()
                && (if in_nm {
                    is_decl(&name)
                } else {
                    is_source(&name)
                })
            {
                out.push((e.path(), in_nm));
            }
        }
    }
    out.sort();
    out
}

struct Done {
    path: String,
    in_nm: bool,
    src: String,
    ast: Ast,
    bound: BoundFile,
    parse: Duration,
    bind: Duration,
}

fn main() {
    let mut root = None;
    let mut list = 0usize;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--list" {
            list = args.next().and_then(|n| n.parse().ok()).unwrap_or(20);
        } else {
            root = Some(PathBuf::from(a));
        }
    }
    let Some(root) = root else {
        eprintln!("usage: corpus [--list N] <dir>");
        std::process::exit(2);
    };
    let root = root.canonicalize().unwrap_or(root);
    let t0 = Instant::now();
    let files = collect(&root);
    let walk = t0.elapsed();

    let next = AtomicUsize::new(0);
    let done: Mutex<Vec<Done>> = Mutex::new(Vec::with_capacity(files.len()));
    let panics: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let t1 = Instant::now();
    std::thread::scope(|s| {
        for _ in 0..THREADS {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some((path, in_nm)) = files.get(i) else {
                        break;
                    };
                    let Ok(src) = std::fs::read_to_string(path) else {
                        continue;
                    };
                    let name = path.to_string_lossy().into_owned();
                    let kind = SourceKind::from_path(&name);
                    let r = std::panic::catch_unwind(|| {
                        let a = Instant::now();
                        let p = ts_r_parser::parse(&src, kind);
                        let parse = a.elapsed();
                        let b = Instant::now();
                        let bound = bind(&p.ast, &src, &BindOptions::default());
                        (p.ast, bound, parse, b.elapsed())
                    });
                    match r {
                        Ok((ast, bound, parse, bind)) => done.lock().unwrap().push(Done {
                            path: name,
                            in_nm: *in_nm,
                            src,
                            ast,
                            bound,
                            parse,
                            bind,
                        }),
                        Err(_) => panics.lock().unwrap().push(name),
                    }
                }
            });
        }
    });
    let wall = t1.elapsed();
    let mut done = done.into_inner().unwrap();
    done.sort_by(|a, b| a.path.cmp(&b.path));
    let panics = panics.into_inner().unwrap();

    let bytes: usize = done.iter().map(|d| d.src.len()).sum();
    let parse: Duration = done.iter().map(|d| d.parse).sum();
    let bindt: Duration = done.iter().map(|d| d.bind).sum();
    let n_nm = done.iter().filter(|d| d.in_nm).count();
    println!("root: {}", root.display());
    println!(
        "files: {} ({} project, {} node_modules declaration files), {:.1} MB, walk {:.0?}",
        done.len(),
        done.len() - n_nm,
        n_nm,
        bytes as f64 / 1e6,
        walk
    );
    println!(
        "parse+bind wall {:.0?} on {THREADS} threads: {:.0} files/s, {:.1} MB/s",
        wall,
        done.len() as f64 / wall.as_secs_f64(),
        bytes as f64 / 1e6 / wall.as_secs_f64()
    );
    println!(
        "cpu time: parse {:.0?}, bind {:.0?} (bind alone: {:.0} files/s/thread)",
        parse,
        bindt,
        done.len() as f64 / bindt.as_secs_f64()
    );
    let mut slow: Vec<&Done> = done.iter().collect();
    slow.sort_by(|a, b| b.bind.cmp(&a.bind));
    for d in slow.iter().take(3) {
        println!(
            "  slowest bind {:.1?} {} ({} KB)",
            d.bind,
            d.path,
            d.src.len() / 1024
        );
    }
    println!("panics: {}", panics.len());
    for p in &panics {
        println!("  PANIC {p}");
    }

    let mut by_code: BTreeMap<(bool, u32), usize> = BTreeMap::new();
    let mut listed = 0;
    for d in &done {
        for g in &d.bound.diagnostics {
            *by_code.entry((d.in_nm, g.code)).or_default() += 1;
            if !d.in_nm && listed < list {
                listed += 1;
                let lc = LineIndex::new(&d.src).line_col(g.span.start);
                println!(
                    "  {}:{}:{} TS{} {}",
                    d.path,
                    lc.line + 1,
                    lc.col + 1,
                    g.code,
                    g.message
                );
            }
        }
    }
    for ((nm, code), n) in &by_code {
        println!(
            "binder diagnostics {}: TS{code} x{n}",
            if *nm { "node_modules" } else { "project" }
        );
    }

    // Link everything into one program.
    let t2 = Instant::now();
    let fs = RealFs::new();
    let resolver = Resolver::new(
        fs,
        ResolverOptions::from_compiler_options(&CompilerOptions::new()),
    );
    let pf: Vec<ProgramFile> = done
        .iter()
        .map(|d| ProgramFile {
            path: &d.path,
            ast: &d.ast,
            src: &d.src,
            bound: &d.bound,
        })
        .collect();
    let program = Program::link_with(pf, &resolver);
    let link = t2.elapsed();
    let (mut total, mut resolved) = (0usize, 0usize);
    for l in &program.links {
        total += l.len();
        resolved += l.iter().filter(|t| **t != ModuleTarget::Unresolved).count();
    }
    println!(
        "link {:.0?}: {} globals, {} ambient modules, {}/{} module requests resolved, {} program diagnostics",
        link,
        program.globals.len(),
        program.ambient_modules.len(),
        resolved,
        total,
        program.diagnostics.len()
    );
    let mut pcodes: BTreeMap<u32, usize> = BTreeMap::new();
    let mut listed = 0;
    for d in &program.diagnostics {
        *pcodes.entry(d.code).or_default() += 1;
        let f = &done[d.file.0 as usize];
        if d.code != 2300 && d.code != 2451 && listed < list {
            listed += 1;
            let lc = LineIndex::new(&f.src).line_col(d.span.start);
            println!(
                "  {}:{}:{} TS{} {}",
                f.path,
                lc.line + 1,
                lc.col + 1,
                d.code,
                d.message
            );
        }
    }
    for (c, n) in pcodes {
        println!("program diagnostics: TS{c} x{n}");
    }
    println!("total {:.0?}", t0.elapsed());
    if !panics.is_empty() {
        std::process::exit(1);
    }
}
