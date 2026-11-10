//! Declaration diagnostics carried by the history (record 0012, "Declaration diagnostics
//! under `--noEmit`"): after each edit, those computed fresh must equal those a carried
//! [`History`] keeps and those `persist::check_with` stores (a full run, then the unchanged
//! fast path), including a file whose own text never changes but whose declarations depend
//! on an edited import.

use std::path::Path;

use ts_r_checker::{CheckRun, Checker, LibStore, ProgramFiles, load_program};
use ts_r_config::{RealFs, load_tsconfig};
use ts_r_diagnostics::{Diagnostic, FileId};
use ts_r_history::incremental::History;
use ts_r_history::persist;

ts_r_testing::cap_memory!();

fn line(path: &str, d: &Diagnostic) -> String {
    format!("{path}({}): TS{} {}", d.span.start, d.code, d.message)
}

fn lines(program: &ProgramFiles, diags: &[Diagnostic]) -> Vec<String> {
    diags
        .iter()
        .map(|d| {
            let path = program
                .files
                .get(d.file.0 as usize)
                .map_or("<global>", |f| f.path.as_str());
            line(path.rsplit('/').next().unwrap_or(path), d)
        })
        .collect()
}

fn write(p: &Path, text: &str) {
    std::fs::write(p, text).expect("write");
}

#[test]
fn declaration_diagnostics_follow_edits() {
    let Some(libs) = LibStore::locate() else {
        return;
    };
    ts_r_history::stat::set_racy_window(std::time::Duration::ZERO);
    let dir = std::env::temp_dir().join(format!("ts-r-history-decls-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let cache_dir = dir.with_extension("cache");
    let _ = std::fs::remove_dir_all(&cache_dir);
    let cache = ts_r_cache::Cache::open_in(&cache_dir, persist::NAMESPACE, persist::FORMAT_VERSION)
        .expect("cache");
    write(
        &dir.join("tsconfig.json"),
        r#"{ "compilerOptions": { "declaration": true, "noEmit": true, "strict": true } }"#,
    );
    let private = "export function make() { return class { private p = 1; }; }\n";
    let plain = "export function make() { return { p: 1 }; }\n";
    write(&dir.join("a.ts"), private);
    write(
        &dir.join("b.ts"),
        "import { make } from './a';\nexport const c = make();\n",
    );
    let config = dir
        .join("tsconfig.json")
        .to_string_lossy()
        .replace('\\', "/");
    let mut history = History::default();
    let mut counts = Vec::new();
    for (step, text) in [Some(private), None, Some(plain), None, Some(private)]
        .into_iter()
        .enumerate()
    {
        if let Some(text) = text {
            write(&dir.join("a.ts"), text);
        }
        let fs = RealFs::new();
        let cfg = load_tsconfig(&fs, &config);
        let options = &cfg.options;
        let hook =
            |c: &Checker<'_>, f: FileId| ts_r_emit::declaration_diagnostics_of(c, f, options, None);
        let program = load_program(&fs, &cfg.file_names, options, Some(&libs), 1);
        let fresh = {
            let checker = Checker::new(&program);
            let out = program
                .files
                .iter()
                .enumerate()
                .filter(|(_, f)| ts_r_emit::has_declarations(f))
                .flat_map(|(i, _)| hook(&checker, FileId(i as u32)))
                .collect();
            ts_r_checker::sort_and_dedup(&program, out)
        };
        let want = lines(&program, &fresh);
        let run = CheckRun { threads: 1 };
        history.check_with(&program, run, Checker::new, Some(&hook));
        let kept = lines(&program, &history.declaration_diagnostics(&program));
        assert_eq!(kept, want, "step {step}: History::check_with");
        let saved = persist::check_with(&fs, Some(&cache), &cfg, Some(&libs), 1, Some(&hook));
        let got: Vec<String> = saved
            .declarations
            .iter()
            .map(|d| match saved.sources.get(d.file.0 as usize) {
                Some(Some((p, _))) => line(p.rsplit('/').next().unwrap_or(p), d),
                _ => line("<other>", d),
            })
            .collect();
        assert_eq!(
            got, want,
            "step {step}: persist::check_with ({:?})",
            saved.mode
        );
        counts.push(want.len());
    }
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&cache_dir);
    // a.ts and b.ts both report TS4094 while the member is private, none once it returns an object:
    // b.ts is unchanged throughout.
    assert!(counts[0] >= 2, "{counts:?}");
    assert_eq!(counts[0], counts[1]);
    assert_eq!(counts[2], 0);
    assert_eq!(counts[3], 0);
    assert_eq!(counts[4], counts[0]);
}
