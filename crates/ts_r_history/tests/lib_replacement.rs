//! `libReplacement`: a lib whose `@typescript/lib-*` package resolves is replaced by it
//! (`ts_r_checker/tests/parity/lib_replacement_package.ts`). A replacement that appears,
//! changes or goes away between runs changes the program's globals, so the persisted
//! history answers as a fresh check does. Needs the lib files.

use std::path::PathBuf;

use ts_r_checker::{CheckRun, LibStore, check_program, load_program};
use ts_r_config::{MemoryFs, load_tsconfig};
use ts_r_history::persist;

ts_r_testing::cap_memory!();

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ts-r-libreplace-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn project(replacement: Option<&str>, sibling: bool) -> MemoryFs {
    let mut fs = MemoryFs::new();
    fs.set_cwd("/p");
    fs.add_file(
        "/p/tsconfig.json",
        r#"{"compilerOptions": {"target": "es2015", "libReplacement": true, "noEmit": true, "strict": true, "types": []}, "files": ["index.ts"]}"#,
    );
    fs.add_file(
        "/p/index.ts",
        "/// <reference lib=\"dom\" />\nexport const a: ABC = { abc: \"x\" };\n",
    );
    if let Some(text) = replacement {
        fs.add_file("/p/node_modules/@typescript/lib-dom/index.d.ts", text);
    }
    if sibling {
        fs.add_file(
            "/p/node_modules/@typescript/lib-dom/defs.d.ts",
            "interface ABC { abc: string }\n",
        );
    }
    fs
}

fn codes(d: &[ts_r_diagnostics::Diagnostic]) -> Vec<u32> {
    d.iter().map(|d| d.code).collect()
}

fn fresh(fs: &MemoryFs, libs: &LibStore) -> Vec<u32> {
    let cfg = load_tsconfig(fs, "/p/tsconfig.json");
    let program = load_program(fs, &cfg.file_names, &cfg.options, Some(libs), 1);
    codes(&check_program(&program, CheckRun { threads: 1 }).diagnostics)
}

#[test]
fn replacement_libraries_invalidate_the_history() {
    let Some(libs) = LibStore::locate() else {
        eprintln!("skipped: no lib files");
        return;
    };
    let dir = temp("history");
    let cache = ts_r_cache::Cache::open_in(&dir, persist::NAMESPACE, persist::FORMAT_VERSION)
        .expect("cache");
    let steps = [
        ("no replacement", project(None, false)),
        ("unchanged", project(None, false)),
        (
            "replacement appears",
            project(Some("interface ABC { abc: string }\n"), false),
        ),
        (
            "unchanged with it",
            project(Some("interface ABC { abc: string }\n"), false),
        ),
        (
            "replacement changes",
            project(Some("interface ABC { abc: number }\n"), false),
        ),
        ("replacement goes away", project(None, false)),
    ];
    for (what, fs) in &steps {
        let cfg = load_tsconfig(fs, "/p/tsconfig.json");
        let out = persist::check(fs, Some(&cache), &cfg, Some(&libs), 1);
        assert_eq!(
            codes(&out.diagnostics),
            fresh(fs, &libs),
            "{what} ({:?})",
            out.mode
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
