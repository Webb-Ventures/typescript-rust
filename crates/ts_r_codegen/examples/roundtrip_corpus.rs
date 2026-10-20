//! Round-trip check over directories: parse → print → parse must give the same AST shape, in
//! both layouts.
//!
//! ```text
//! scripts/limit.sh -- cargo run --release -p ts_r_codegen --example roundtrip_corpus -- <dir>... [--show N]
//! ```

use std::path::{Path, PathBuf};

use ts_r_codegen::shape::{first_difference, shape};
use ts_r_codegen::{CodegenOptions, Layout};

ts_r_testing::cap_memory!();

const EXTENSIONS: &[&str] = &["ts", "tsx", "mts", "cts", "js", "mjs", "cjs", "jsx"];

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let Ok(ft) = e.file_type() else { continue };
        let path = e.path();
        if ft.is_symlink() || e.file_name() == "node_modules" || e.file_name() == ".next" {
            continue;
        }
        if ft.is_dir() {
            walk(&path, out);
        } else if ft.is_file() {
            let ext = path.extension().and_then(|x| x.to_str()).unwrap_or("");
            if EXTENSIONS.contains(&ext) {
                out.push(path);
            }
        }
    }
}

fn main() {
    let mut dirs = Vec::new();
    let mut show = 10usize;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--show" {
            show = args.next().and_then(|n| n.parse().ok()).unwrap_or(10);
        } else {
            dirs.push(PathBuf::from(a));
        }
    }
    let mut files = Vec::new();
    for d in &dirs {
        if d.is_file() {
            files.push(d.clone());
        } else {
            walk(d, &mut files);
        }
    }
    let (mut ok, mut skipped, mut bad) = (0usize, 0usize, 0usize);
    let mut shown = 0;
    let start = std::time::Instant::now();
    for f in &files {
        let Ok(src) = std::fs::read_to_string(f) else {
            skipped += 1;
            continue;
        };
        let path = f.to_string_lossy();
        if std::env::var_os("VERBOSE").is_some() {
            eprintln!("{path}");
        }
        let parsed = ts_r_parser::parse_path(&path, &src);
        if !parsed.diagnostics.is_empty() {
            skipped += 1;
            continue;
        }
        let want = shape(&parsed.ast);
        let mut failed = None;
        for layout in [Layout::Readable, Layout::Compact] {
            let opts = CodegenOptions {
                layout,
                ..CodegenOptions::default()
            };
            let out = ts_r_codegen::print(&parsed.ast, &src, &opts).code;
            let re = ts_r_parser::parse(&out, parsed.ast.source_kind);
            let got = shape(&re.ast);
            if !re.diagnostics.is_empty() {
                failed = Some(format!(
                    "{layout:?}: reparse error: {} at {}\n{}",
                    re.diagnostics[0].message,
                    re.diagnostics[0].span.start,
                    context(&out, re.diagnostics[0].span.start as usize)
                ));
                break;
            }
            if let Some(d) = first_difference(&want, &got) {
                failed = Some(format!("{layout:?}: shape differs\n{d}"));
                break;
            }
        }
        match failed {
            None => ok += 1,
            Some(msg) => {
                bad += 1;
                if shown < show {
                    shown += 1;
                    println!("FAIL {path}\n{msg}");
                }
            }
        }
    }
    println!(
        "round-trip: {} files, {ok} ok, {bad} mismatches, {skipped} skipped (parse errors), {:.1?}",
        files.len(),
        start.elapsed()
    );
    if bad > 0 {
        std::process::exit(1);
    }
}

fn context(s: &str, pos: usize) -> String {
    let mut a = pos.saturating_sub(80);
    while !s.is_char_boundary(a) {
        a -= 1;
    }
    let mut b = (pos + 80).min(s.len());
    while !s.is_char_boundary(b) {
        b += 1;
    }
    format!(
        "  ...{}<<HERE>>{}...",
        &s[a..pos.min(s.len())],
        &s[pos.min(s.len())..b]
    )
}
