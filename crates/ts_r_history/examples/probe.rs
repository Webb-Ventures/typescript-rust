//! Per-edit cost probe: `probe <tsconfig.json> <file> [iterations] [flat|layered]`.
//!
//! Loads the project, then repeatedly appends a line to `<file>`, re-parses it and re-checks
//! it the way the editor does, printing phase timings (p50 over the iterations). `layered`
//! (the default) arranges the program on shared levels with `<file>` hot, as the editor does.

use std::sync::Arc;
use std::time::Instant;

use ts_r_checker::program::bind_options_for;
use ts_r_checker::{Checker, Ck, LibStore, SourceFile, load_program};
use ts_r_config::{RealFs, load_tsconfig};
use ts_r_diagnostics::FileId;

fn main() {
    ts_r_testing::cap_memory!();
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: probe <tsconfig.json> <file> [iterations] [flat|layered]");
        return;
    }
    let (config, file) = (&args[1], &args[2]);
    let iters: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(20);
    let layered = args.get(4).is_none_or(|m| m != "flat");
    let fs = RealFs::new();
    let parsed = load_tsconfig(&fs, config);
    let Some(libs) = LibStore::locate() else {
        eprintln!("lib files not found");
        return;
    };
    let t = Instant::now();
    let mut program = load_program(&fs, &parsed.file_names, &parsed.options, Some(&libs), 4);
    eprintln!(
        "load: {} files in {:.1} ms",
        program.files.len(),
        t.elapsed().as_secs_f64() * 1e3
    );
    let Some(mut i) = program.file_index(file) else {
        eprintln!("{file} is not part of the program");
        return;
    };
    let base_text = program.files[i].text.clone();
    let (mut parse, mut arrange, mut link, mut check) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut keep = None;
    for n in 0..iters {
        let text = format!("{base_text}\n__bench_{n};\n");
        let t = Instant::now();
        let opts = bind_options_for::<RealFs>(file, &parsed.options, None);
        let f = SourceFile::new(file.clone(), text, false, &opts);
        program.files[i] = Arc::new(f);
        parse.push(t.elapsed().as_secs_f64() * 1e3);
        let t = Instant::now();
        let parent = if layered {
            let a = ts_r_history::arrange(&mut program, &[i]);
            i = a.perm[i] as usize;
            if n == 0 {
                let tg = Instant::now();
                let g = ts_r_history::levels::Graph::build(&program);
                let gms = tg.elapsed().as_secs_f64() * 1e3;
                let tl = Instant::now();
                let _ = ts_r_history::levels::plan(&program, &g, &[i]);
                eprintln!(
                    "levels: {:?} (reused {}, built {}) in {:.1} ms; graph {gms:.1} ms, plan {:.1} ms",
                    a.ends,
                    a.reused,
                    a.built,
                    t.elapsed().as_secs_f64() * 1e3,
                    tl.elapsed().as_secs_f64() * 1e3
                );
            }
            a.parent
        } else {
            None
        };
        arrange.push(t.elapsed().as_secs_f64() * 1e3);
        let t = Instant::now();
        let checker = match &parent {
            Some(p) => Checker::layered(&program, Some(p)).unwrap_or_else(|e| {
                eprintln!("layered checker refused: {e:?}");
                Checker::new(&program)
            }),
            None => Checker::new(&program),
        };
        link.push(t.elapsed().as_secs_f64() * 1e3);
        let t = Instant::now();
        let mut ck = Ck::new(&checker, FileId(i as u32));
        let d = ck.check_source_file(FileId(i as u32));
        check.push(t.elapsed().as_secs_f64() * 1e3);
        if n == 0 {
            eprintln!("diagnostics: {}", d.len());
        }
        drop(ck);
        drop(checker);
        keep = parent;
    }
    drop(keep);
    let p50 = |v: &mut Vec<f64>| {
        v.sort_by(|a, b| a.total_cmp(b));
        v[v.len() / 2]
    };
    eprintln!(
        "p50 parse {:.2} ms, arrange {:.2} ms, link {:.2} ms, check {:.2} ms",
        p50(&mut parse),
        p50(&mut arrange),
        p50(&mut link),
        p50(&mut check)
    );
}
