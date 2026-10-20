//! Indexes every TS/JS source outside `node_modules` and every declaration file inside it
//! (symlinks not followed): crash-freedom, timing, explicitness and encoded size.
//!
//! ```text
//! cargo build --release -j 4 -p ts_r_decls --example corpus
//! scripts/limit.sh --mem-mb 4096 --time-s 600 -- target/release/examples/corpus ../private-app
//! ```

ts_r_testing::cap_memory!();

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use ts_r_ast::SourceKind;
use ts_r_binder::{BindOptions, bind};

const THREADS: usize = 4;

fn wanted(name: &str, in_nm: bool) -> bool {
    if in_nm {
        name.ends_with(".d.ts") || name.ends_with(".d.mts") || name.ends_with(".d.cts")
    } else {
        [".ts", ".tsx", ".mts", ".cts", ".js", ".jsx", ".mjs", ".cjs"]
            .iter()
            .any(|e| name.ends_with(e))
    }
}

fn collect(root: &Path) -> Vec<PathBuf> {
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
                if !matches!(name.as_str(), ".git" | ".turbo" | ".next" | ".cache") {
                    stack.push((e.path(), in_nm || name == "node_modules"));
                }
            } else if ft.is_file() && wanted(&name, in_nm) {
                out.push(e.path());
            }
        }
    }
    out.sort();
    out
}

#[derive(Default)]
struct Totals {
    files: usize,
    entries: usize,
    explicit_entries: usize,
    explicit_files: usize,
    encoded: usize,
    index_time: Duration,
    panics: Vec<String>,
}

fn main() {
    let Some(root) = std::env::args().nth(1).map(PathBuf::from) else {
        eprintln!("usage: corpus <dir>");
        std::process::exit(2);
    };
    let files = collect(&root);
    let next = AtomicUsize::new(0);
    let totals = Mutex::new(Totals::default());
    let t = Instant::now();
    std::thread::scope(|s| {
        for _ in 0..THREADS {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(path) = files.get(i) else { break };
                    let Ok(src) = std::fs::read_to_string(path) else {
                        continue;
                    };
                    let name = path.to_string_lossy().into_owned();
                    let r = std::panic::catch_unwind(|| {
                        let p = ts_r_parser::parse(&src, SourceKind::from_path(&name));
                        let b = bind(&p.ast, &src, &BindOptions::default());
                        let a = Instant::now();
                        let idx = ts_r_decls::index(&p.ast, &b);
                        let took = a.elapsed();
                        let enc = idx.encode();
                        assert_eq!(ts_r_decls::DeclIndex::decode(&enc).as_ref(), Some(&idx));
                        (idx, enc.len(), took)
                    });
                    let mut t = totals.lock().unwrap();
                    match r {
                        Ok((idx, len, took)) => {
                            t.files += 1;
                            t.entries += idx.entries.len();
                            t.explicit_entries += idx.entries.iter().filter(|e| e.explicit).count();
                            t.explicit_files += usize::from(idx.is_explicit());
                            t.encoded += len;
                            t.index_time += took;
                        }
                        Err(_) => t.panics.push(name),
                    }
                }
            });
        }
    });
    let wall = t.elapsed();
    let t = totals.into_inner().unwrap();
    println!(
        "{} files in {:.0?} wall (parse+bind+index, {THREADS} threads): {:.0} files/s",
        t.files,
        wall,
        t.files as f64 / wall.as_secs_f64()
    );
    println!(
        "index cpu {:.0?}; {} entries, {} explicit ({:.1}%); {} fully explicit files; {:.1} MB encoded",
        t.index_time,
        t.entries,
        t.explicit_entries,
        100.0 * t.explicit_entries as f64 / t.entries.max(1) as f64,
        t.explicit_files,
        t.encoded as f64 / 1e6
    );
    println!("panics: {}", t.panics.len());
    for p in &t.panics {
        println!("  PANIC {p}");
    }
    if !t.panics.is_empty() {
        std::process::exit(1);
    }
}
