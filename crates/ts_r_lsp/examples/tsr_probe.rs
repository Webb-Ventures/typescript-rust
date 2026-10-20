//! Probe: per-project costs of the ts-r backend's building blocks (load, link, check).
//! `cargo run --release -p ts_r_lsp --example tsr_probe -- <tsconfig.json>...`

ts_r_testing::cap_memory!();

use std::time::Instant;
use ts_r_checker::{Checker, Ck, LibStore, load_program};
use ts_r_config::{RealFs, load_tsconfig};
use ts_r_diagnostics::FileId;

fn main() {
    let fs = RealFs::new();
    let libs = LibStore::locate().expect("libs");
    for cfg in std::env::args().skip(1) {
        let t = Instant::now();
        let parsed = load_tsconfig(&fs, &cfg);
        let program = load_program(&fs, &parsed.file_names, &parsed.options, Some(&libs), 4);
        let load = t.elapsed();
        let t = Instant::now();
        let checker = Checker::new(&program);
        let link = t.elapsed();
        let mut worst = (std::time::Duration::ZERO, String::new());
        let t = Instant::now();
        let mut n = 0;
        for (i, f) in program.files.iter().enumerate() {
            if f.is_lib || f.is_declaration() {
                continue;
            }
            n += 1;
            let t1 = Instant::now();
            let mut ck = Ck::new(&checker, FileId(i as u32));
            let _ = ck.check_source_file(FileId(i as u32));
            let d = t1.elapsed();
            if d > worst.0 {
                worst = (d, f.path.clone());
            }
        }
        let check = t.elapsed();
        // Second checker: how much of a re-check is lib work.
        let t = Instant::now();
        let checker2 = Checker::new(&program);
        let link2 = t.elapsed();
        let wi = program.file_index(&worst.1).unwrap_or(0);
        let t = Instant::now();
        let mut ck = Ck::new(&checker2, FileId(wi as u32));
        let _ = ck.check_source_file(FileId(wi as u32));
        let recheck = t.elapsed();
        // `TSR_PROBE_LOOP=N`: re-check the worst file N times on fresh checkers (to profile).
        let loops: usize = std::env::var("TSR_PROBE_LOOP")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        for _ in 0..loops {
            let ch = Checker::new(&program);
            let mut ck = Ck::new(&ch, FileId(wi as u32));
            let _ = ck.check_source_file(FileId(wi as u32));
        }
        // Same file again on the first (warm) checker: what shared type caches save.
        let t = Instant::now();
        let mut ck = Ck::new(&checker, FileId(wi as u32));
        let _ = ck.check_source_file(FileId(wi as u32));
        let warm = t.elapsed();
        println!(
            "{cfg}: files {} ({} own), load {:?}, link {:?}, check all (fresh Ck each) {:?}, worst {:?} {}, relink {:?}, recheck worst on new checker {:?}, on the warm checker {warm:?}",
            program.files.len(),
            n,
            load,
            link,
            check,
            worst.0,
            worst.1,
            link2,
            recheck
        );
    }
}
