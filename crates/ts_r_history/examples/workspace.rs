//! `workspace <tsconfig>...`: loads every project in one process the way the editor daemon
//! does (arranged on shared levels, every file checked once), keeps them alive, and prints
//! the live levels, how many source files were shared, and the heap of each level. Shows
//! what all-files-open memory is made of, and checks that every project's diagnostics on the
//! shared levels equal a flat check's. Run under `scripts/limit.sh`.

use std::time::Instant;

use ts_r_checker::{
    CheckRun, Checker, LibStore, check_files, check_program, check_program_with, load_program,
    should_check,
};
use ts_r_config::{RealFs, load_tsconfig};

ts_r_testing::cap_memory!();

fn main() {
    let configs: Vec<String> = std::env::args().skip(1).collect();
    let Some(libs) = LibStore::locate() else {
        eprintln!("lib files missing");
        return;
    };
    let fs = RealFs::new();
    let mut kept = Vec::new();
    let t = Instant::now();
    let mut files = 0usize;
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for c in &configs {
        let cfg = load_tsconfig(&fs, c);
        let mut program = load_program(&fs, &cfg.file_names, &cfg.options, Some(&libs), 4);
        let shared = ts_r_history::layout::share_sources(&mut program);
        let arranged = ts_r_history::arrange(&mut program, &[]);
        files += program.files.len();
        {
            let checker = Checker::layered(&program, arranged.parent.as_ref())
                .unwrap_or_else(|_| Checker::new(&program));
            let targets: Vec<usize> = (0..program.files.len())
                .filter(|&i| should_check(&program, i))
                .collect();
            let _ = check_files(&checker, &targets, CheckRun { threads: 4 });
        }
        let run = CheckRun { threads: 4 };
        let layered = check_program_with(&program, run, |p| {
            Checker::layered(p, arranged.parent.as_ref()).unwrap_or_else(|_| Checker::new(p))
        });
        let flat = check_program(&program, run);
        if layered.diagnostics != flat.diagnostics {
            println!(
                "MISMATCH {c}: layered {} vs flat {} diagnostics",
                layered.diagnostics.len(),
                flat.diagnostics.len()
            );
        }
        let seen_before = program
            .files
            .iter()
            .filter(|f| !f.is_lib && !seen.insert(f.path.clone()))
            .count();
        println!(
            "{c}: {} files ({seen_before} loaded by an earlier project), {shared} shared, levels reused {} built {}",
            program.files.len(),
            arranged.reused,
            arranged.built
        );
        kept.push((program, arranged));
    }
    let mut by_level = [(0usize, 0usize, 0usize); 4];
    for (lv, n, bytes) in ts_r_history::live_layers() {
        let e = &mut by_level[lv as usize & 3];
        e.0 += 1;
        e.1 += n;
        e.2 += bytes;
    }
    for (i, (count, n, bytes)) in by_level.iter().enumerate() {
        println!(
            "level {i}: {count} live, {n} files, {:.1} MB",
            *bytes as f64 / 1048576.0
        );
    }
    println!(
        "{} projects, {files} program files, {:.2} s",
        configs.len(),
        t.elapsed().as_secs_f64()
    );
}
