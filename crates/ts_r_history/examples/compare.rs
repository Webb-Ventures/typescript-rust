//! Layered vs flat check of a real project: `compare <tsconfig.json> [hot file...]`.
//!
//! Checks the project with `check_program` and with shared levels (no hot file, then each
//! given hot file) and prints the differences. Exit status 1 when any differ.

use std::process::ExitCode;
use std::time::Instant;

use ts_r_checker::{CheckRun, LibStore, ProgramFiles, check_program, load_program};
use ts_r_config::{RealFs, load_tsconfig};
use ts_r_diagnostics::Diagnostic;

fn lines(program: &ProgramFiles, diags: &[Diagnostic]) -> Vec<String> {
    let path = |f: ts_r_diagnostics::FileId| {
        program
            .files
            .get(f.0 as usize)
            .map_or("<global>", |f| f.path.as_str())
            .to_owned()
    };
    let mut v: Vec<String> = diags
        .iter()
        .map(|d| {
            format!(
                "{}@{}:{} TS{} {}",
                path(d.file),
                d.span.start,
                d.span.end,
                d.code,
                d.message
            )
        })
        .collect();
    v.sort();
    v
}

fn main() -> ExitCode {
    ts_r_testing::cap_memory!();
    let args: Vec<String> = std::env::args().collect();
    let Some(config) = args.get(1) else {
        eprintln!("usage: compare <tsconfig.json> [hot file...]");
        return ExitCode::from(2);
    };
    let fs = RealFs::new();
    let parsed = load_tsconfig(&fs, config);
    let Some(libs) = LibStore::locate() else {
        eprintln!("lib files not found");
        return ExitCode::from(2);
    };
    let program = load_program(&fs, &parsed.file_names, &parsed.options, Some(&libs), 4);
    let run = CheckRun { threads: 4 };
    let t = Instant::now();
    let flat = check_program(&program, run);
    let want = lines(&program, &flat.diagnostics);
    eprintln!(
        "flat: {} files, {} diagnostics, {:.1} ms",
        program.files.len(),
        want.len(),
        t.elapsed().as_secs_f64() * 1e3
    );
    let mut hots: Vec<Vec<usize>> = vec![Vec::new()];
    for h in &args[2..] {
        match program.file_index(h) {
            Some(i) => hots.push(vec![i]),
            None => eprintln!("{h}: not in the program"),
        }
    }
    let mut ok = true;
    for hot in hots {
        let t = Instant::now();
        let (p, r) = ts_r_history::check_layered(&program, &hot, run);
        let got = lines(&p, &r.diagnostics);
        let ms = t.elapsed().as_secs_f64() * 1e3;
        if got == want {
            eprintln!("layered (hot {hot:?}): same, {ms:.1} ms");
        } else {
            ok = false;
            eprintln!("layered (hot {hot:?}): DIFFERENT, {ms:.1} ms");
            for l in want.iter().filter(|l| !got.contains(l)) {
                eprintln!("  - {l}");
            }
            for l in got.iter().filter(|l| !want.contains(l)) {
                eprintln!("  + {l}");
            }
        }
    }
    if std::env::var_os("COMPARE_FILES").is_some() {
        per_file_times(&program, &args[2..]);
    }
    eprintln!("live levels: {:?}", ts_r_history::live_layers());
    let misses: Vec<u64> = ts_r_types::PUBLISH_MISSES
        .iter()
        .map(|c| c.load(std::sync::atomic::Ordering::Relaxed))
        .collect();
    eprintln!("publish misses: {misses:?}");
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// Per-file check times, flat vs layered with the first hot file (slowest differences).
fn per_file_times(program: &ProgramFiles, hot: &[String]) {
    use ts_r_checker::{Checker, Ck};
    use ts_r_diagnostics::FileId;
    let time_all = |p: &ProgramFiles, c: &Checker<'_>| -> Vec<(String, f64)> {
        let mut out = Vec::new();
        for i in 0..p.files.len() {
            if !ts_r_checker::should_check(p, i) {
                continue;
            }
            let t = Instant::now();
            let mut ck = Ck::new(c, FileId(i as u32));
            let _ = ck.check_source_file(FileId(i as u32));
            out.push((p.files[i].path.clone(), t.elapsed().as_secs_f64() * 1e3));
        }
        out
    };
    let flat_c = Checker::new(program);
    let flat = time_all(program, &flat_c);
    let h: Vec<usize> = hot
        .iter()
        .filter_map(|h| program.file_index(h))
        .take(1)
        .collect();
    let mut p = ts_r_history::levels::with_files(program, program.files.clone());
    let a = ts_r_history::arrange(&mut p, &h);
    let c = Checker::layered(&p, a.parent.as_ref()).unwrap_or_else(|_| Checker::new(&p));
    let lay = time_all(&p, &c);
    if let Some(one) = std::env::var_os("COMPARE_ONE") {
        let one = one.to_string_lossy().into_owned();
        if let Some(i) = p.file_index(&one) {
            for _ in 0..3 {
                let t = Instant::now();
                let mut ck = Ck::new(&c, FileId(i as u32));
                let _ = ck.check_source_file(FileId(i as u32));
                eprintln!("again {one}: {:.1} ms", t.elapsed().as_secs_f64() * 1e3);
            }
        }
    }
    let mut rows: Vec<(String, f64, f64)> = flat
        .iter()
        .filter_map(|(f, t)| {
            lay.iter()
                .find(|(g, _)| g == f)
                .map(|(_, u)| (f.clone(), *t, *u))
        })
        .collect();
    rows.sort_by(|a, b| (b.2 - b.1).total_cmp(&(a.2 - a.1)));
    eprintln!("levels {:?}", a.ends);
    if let Ok(v) = std::env::var("COMPARE_SHOW") {
        for part in v.split(',') {
            let mut it = part.split(':');
            let (Some(f), Some(n)) = (it.next(), it.next()) else {
                continue;
            };
            let (Ok(f), Ok(n)) = (f.parse::<usize>(), n.parse::<u32>()) else {
                continue;
            };
            let file = &p.files[f];
            let Some(node) = ts_r_ast::NodeId::from_raw(n) else {
                continue;
            };
            let sp = file.ast.span(node);
            let end = (sp.start as usize + 160).min(file.text.len());
            eprintln!(
                "{f}:{n} {} :: {}",
                file.path,
                &file.text[sp.start as usize..end]
            );
        }
    }
    for (f, t, u) in rows.iter().take(10) {
        eprintln!("  {f}: flat {t:.1} ms, layered {u:.1} ms");
    }
}
