//! Parses every TS/JS file under the given directories and reports syntax errors and speed.
//!
//! ```text
//! cargo run -p ts_r_parser --example parse_corpus --release -- <dir>... [--errors N] [--quiet]
//!     [--grammar]
//! ```
//!
//! `--grammar` also lists each file's first checker grammar error.
//!
//! Symlinks are not followed (pnpm stores are walked once). Timing covers parsing only, not
//! file reading.

// Real-world or generated input: abort instead of exhausting the machine's memory.
ts_r_testing::cap_memory!();

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ts_r_parser::ts_r_ast::SourceKind;

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
        if ft.is_symlink() {
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
    let mut show_errors = 20usize;
    let mut quiet = false;
    let mut show_grammar = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--errors" => show_errors = args.next().and_then(|n| n.parse().ok()).unwrap_or(20),
            "--quiet" => quiet = true,
            "--grammar" => show_grammar = true,
            _ => dirs.push(PathBuf::from(a)),
        }
    }
    if dirs.is_empty() {
        eprintln!("usage: parse_corpus <dir>... [--errors N] [--quiet] [--grammar]");
        std::process::exit(2);
    }
    let mut files = Vec::new();
    for d in &dirs {
        if d.is_file() {
            files.push(d.clone());
        } else {
            walk(d, &mut files);
        }
    }
    let mut total_bytes = 0u64;
    let mut parse_time = Duration::ZERO;
    let mut failed: Vec<(PathBuf, usize, String)> = Vec::new();
    let mut skipped = 0usize;
    let mut parsed = 0usize;
    let mut grammar_files = 0usize;
    let mut nodes = 0usize;
    for path in &files {
        let Ok(bytes) = std::fs::read(path) else {
            skipped += 1;
            continue;
        };
        let Ok(text) = String::from_utf8(bytes) else {
            skipped += 1;
            continue;
        };
        let name = path.to_string_lossy();
        let kind = SourceKind::from_path(&name);
        let t = Instant::now();
        let result = std::panic::catch_unwind(|| ts_r_parser::parse(&text, kind));
        parse_time += t.elapsed();
        parsed += 1;
        total_bytes += text.len() as u64;
        match result {
            Ok(r) => {
                nodes += r.ast.node_count();
                // The checker's grammar errors (`set x(v = 1) {}`) are no syntax errors:
                // tsc reports them only in checked files. Counted apart.
                if let Some(d) = r.grammar.first() {
                    grammar_files += 1;
                    if show_grammar {
                        let idx = ts_r_span::LineIndex::new(&text);
                        let lc = idx.line_col(d.span.start);
                        println!(
                            "grammar: {}:{}:{}: TS{} {}",
                            name,
                            lc.line + 1,
                            lc.col + 1,
                            d.code,
                            d.message
                        );
                    }
                }
                let errors = r.syntax_errors();
                if !errors.is_empty() {
                    let d = &errors[0];
                    let idx = ts_r_span::LineIndex::new(&text);
                    let lc = idx.line_col(d.span.start);
                    let line = idx.line_span(lc.line).map(|s| s.text(&text)).unwrap_or("");
                    let line: String = line.trim_end().chars().take(160).collect();
                    let msg = format!(
                        "{}:{}:{}: TS{} {}  ({} errors)\n    | {}",
                        name,
                        lc.line + 1,
                        lc.col + 1,
                        d.code,
                        d.message,
                        errors.len(),
                        line
                    );
                    failed.push((path.clone(), errors.len(), msg));
                }
            }
            Err(_) => failed.push((path.clone(), 1, format!("{name}: PANIC"))),
        }
    }
    if !quiet {
        for (_, _, msg) in failed.iter().take(show_errors) {
            println!("{msg}");
        }
    }
    let secs = parse_time.as_secs_f64().max(1e-9);
    println!(
        "files: {parsed} (skipped {skipped}), with errors: {} (checker grammar errors: {grammar_files}), bytes: {:.1} MB, nodes: {nodes}",
        failed.len(),
        total_bytes as f64 / 1e6
    );
    println!(
        "parse time: {:.3}s, {:.0} files/s, {:.1} MB/s",
        secs,
        parsed as f64 / secs,
        total_bytes as f64 / 1e6 / secs
    );
}
