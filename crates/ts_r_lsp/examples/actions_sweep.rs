//! Sweep: code actions (refactors at every node, quick fixes for every diagnostic) over the
//! files of real projects. Reports panics, results that parse with more syntax errors than
//! the input, and per-request latency.
//! `cargo run --release -p ts_r_lsp --example actions_sweep -- [--max-files N] <tsconfig.json>...`

ts_r_testing::cap_memory!();

use serde_json::{Value, json};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::{Duration, Instant};
use ts_r_checker::{Checker, Ck, LibStore, load_program};
use ts_r_config::{RealFs, load_tsconfig};
use ts_r_diagnostics::FileId;
use ts_r_lsp::text::TextDoc;
use ts_r_lsp::tsr::convert::{DiagKind, LspLines, diagnostic_to_lsp};
use ts_r_lsp::tsr::features::{Ctx, actions};

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut max_files = usize::MAX;
    if let Some(i) = args.iter().position(|a| a == "--max-files") {
        max_files = args
            .get(i + 1)
            .and_then(|n| n.parse().ok())
            .unwrap_or(usize::MAX);
        args.drain(i..(i + 2).min(args.len()));
    }
    let fs = RealFs::new();
    let libs = LibStore::locate().expect("libs");
    for cfg in &args {
        let parsed = load_tsconfig(&fs, cfg);
        let program = load_program(&fs, &parsed.file_names, &parsed.options, Some(&libs), 4);
        let checker = Checker::new(&program);
        let mut times: Vec<Duration> = Vec::new();
        let (mut offered, mut panics, mut broken, mut files) = (0usize, 0usize, 0usize, 0usize);
        let mut worst = (Duration::ZERO, String::new());
        for (i, f) in program.files.iter().enumerate() {
            if f.is_lib || f.is_external || f.is_declaration() || files >= max_files {
                continue;
            }
            files += 1;
            let file = FileId(i as u32);
            let mut ck = Ck::new(&checker, file);
            let diags = ck.check_source_file(file);
            let text = f.text.as_str();
            let lines = LspLines::new(text);
            let cx = Ctx {
                program: &program,
                checker: &checker,
                file,
            };
            let uri = ts_r_lsp::uri::path_to_uri(&f.path);
            let mut requests: Vec<(u32, u32, Value)> = Vec::new();
            for n in 1..=f.ast.node_count() as u32 {
                let Some(id) = ts_r_ast::NodeId::from_raw(n) else {
                    continue;
                };
                let sp = f.ast.span(id);
                requests.push((sp.start, sp.start, json!({"diagnostics": []})));
                requests.push((
                    sp.start,
                    sp.end,
                    json!({"diagnostics": [], "only": ["refactor"]}),
                ));
            }
            let no_rel = |_: &ts_r_diagnostics::RelatedInfo| None;
            for d in diags.iter().filter(|d| d.file == file) {
                let ld = diagnostic_to_lsp(d, text, &lines, DiagKind::Semantic, &no_rel);
                requests.push((d.span.start, d.span.end, json!({"diagnostics": [ld]})));
            }
            let errs = |src: &str| ts_r_parser::parse_path(&f.path, src).diagnostics.len();
            let before = errs(text);
            for (s, e, context) in requests {
                let params = json!({
                    "textDocument": {"uri": uri},
                    "range": lines.range(text, s, e),
                    "context": context,
                });
                let t = Instant::now();
                let r = catch_unwind(AssertUnwindSafe(|| {
                    actions::code_actions(&cx, &mut ck, &diags, &params)
                }));
                let dt = t.elapsed();
                times.push(dt);
                if dt > worst.0 {
                    worst = (dt, format!("{}:{s}..{e}", f.path));
                }
                let Ok(r) = r else {
                    panics += 1;
                    eprintln!("PANIC {}:{s}..{e}", f.path);
                    continue;
                };
                for a in r.as_array().into_iter().flatten() {
                    offered += 1;
                    let Some(edits) = a["edit"]["changes"][&uri].as_array() else {
                        continue;
                    };
                    let mut doc = TextDoc::new(text.to_string());
                    let mut list: Vec<&Value> = edits.iter().collect();
                    list.sort_by_key(|x| {
                        let p = &x["range"]["start"];
                        std::cmp::Reverse((p["line"].as_u64(), p["character"].as_u64()))
                    });
                    for x in list {
                        if let Some(r) = ts_r_lsp::text::lsp_range(&x["range"]) {
                            doc.apply_change(Some(r), x["newText"].as_str().unwrap_or(""));
                        }
                    }
                    if errs(doc.text()) > before {
                        broken += 1;
                        eprintln!("BROKEN {}:{s}..{e} {}", f.path, a["title"]);
                        if std::env::var("TS_R_SWEEP_SHOW").is_ok() {
                            let after = doc.text();
                            let d = ts_r_parser::parse_path(&f.path, after).diagnostics;
                            if let Some(x) = d.first() {
                                let at = x.span.start as usize;
                                let lo = after.floor_char_boundary(at.saturating_sub(300));
                                let hi = after.ceil_char_boundary((at + 200).min(after.len()));
                                eprintln!(
                                    "  {}\n{}<<HERE>>{}",
                                    x.message,
                                    &after[lo..at],
                                    &after[at..hi]
                                );
                            }
                        }
                    }
                }
            }
        }
        times.sort();
        let pct = |p: f64| {
            times
                .get(((times.len() as f64 - 1.0) * p) as usize)
                .map_or(0.0, |d| d.as_secs_f64() * 1000.0)
        };
        println!(
            "{cfg}: {files} files, {} requests, {offered} actions, {panics} panics, {broken} broken; p50 {:.3} ms, p95 {:.3} ms, p99 {:.3} ms, max {:.1} ms ({})",
            times.len(),
            pct(0.5),
            pct(0.95),
            pct(0.99),
            worst.0.as_secs_f64() * 1000.0,
            worst.1
        );
    }
}
