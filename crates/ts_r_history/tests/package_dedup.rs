//! Package deduplication (tsc's `deduplicatePackages`, on by default) through the persisted
//! history: two identical copies of `x@1.2.3` under dependencies `a` and `b` are one file to
//! the checker, cold and on every later run, so a class with a private member is one class
//! (tsc 7.0.2: no error for `a(b())`). A minimal lib directory stands in for the bundled
//! libs, so these run without `cargo xtask upstream sync`.

use std::path::{Path, PathBuf};

use ts_r_checker::{CheckRun, LibStore, check_program, load_program};
use ts_r_config::{MemoryFs, load_tsconfig};
use ts_r_history::persist::{self, Mode};

ts_r_testing::cap_memory!();

const LIB: &str = "interface Array<T> { length: number }\ninterface Boolean {}\ninterface Function {}\ninterface CallableFunction {}\ninterface NewableFunction {}\ninterface IArguments {}\ninterface Number {}\ninterface Object {}\ninterface RegExp {}\ninterface String {}\n";

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ts-r-dedup-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// A lib directory with only `lib.es5.d.ts` (and `lib.d.ts`, the default for es5).
fn libs(dir: &Path) -> LibStore {
    let libs = dir.join("libs");
    std::fs::create_dir_all(&libs).expect("lib dir");
    std::fs::write(libs.join("lib.es5.d.ts"), LIB).expect("lib");
    std::fs::write(libs.join("lib.d.ts"), "/// <reference lib=\"es5\" />\n").expect("lib");
    LibStore::new(libs)
}

const X: &str = "export declare class X {\n    private p;\n}\n";
const X_PKG: &str = r#"{ "name": "x", "version": "1.2.3", "types": "index.d.ts" }"#;

fn project(extra: &[(&str, &str)]) -> MemoryFs {
    let mut fs = MemoryFs::new();
    fs.set_cwd("/p");
    fs.add_file(
        "/p/tsconfig.json",
        r#"{"compilerOptions": {"strict": true, "lib": ["es5"], "module": "commonjs", "moduleResolution": "node10", "noEmit": true, "types": []}, "files": ["main.ts"]}"#,
    );
    fs.add_file(
        "/p/main.ts",
        "import { a } from \"a\";\nimport { b } from \"b\";\na(b());\n",
    );
    fs.add_file(
        "/p/node_modules/a/package.json",
        r#"{ "name": "a", "version": "1.0.0", "types": "index.d.ts" }"#,
    );
    fs.add_file(
        "/p/node_modules/a/index.d.ts",
        "import { X } from \"x\";\nexport declare function a(x: X): void;\n",
    );
    fs.add_file("/p/node_modules/a/node_modules/x/package.json", X_PKG);
    fs.add_file("/p/node_modules/a/node_modules/x/index.d.ts", X);
    fs.add_file(
        "/p/node_modules/b/package.json",
        r#"{ "name": "b", "version": "1.0.0", "types": "index.d.ts" }"#,
    );
    fs.add_file(
        "/p/node_modules/b/index.d.ts",
        "import { X } from \"x\";\nexport declare function b(): X;\n",
    );
    fs.add_file("/p/node_modules/b/node_modules/x/package.json", X_PKG);
    fs.add_file("/p/node_modules/b/node_modules/x/index.d.ts", X);
    for (p, src) in extra {
        fs.add_file(&format!("/p/{p}"), src);
    }
    fs
}

fn codes(diags: &[ts_r_diagnostics::Diagnostic]) -> Vec<u32> {
    diags.iter().map(|d| d.code).collect()
}

/// Fresh: `load_program` + `check_program`.
fn fresh(fs: &MemoryFs, libs: &LibStore) -> Vec<u32> {
    let cfg = load_tsconfig(fs, "/p/tsconfig.json");
    let program = load_program(fs, &cfg.file_names, &cfg.options, Some(libs), 1);
    codes(&check_program(&program, CheckRun { threads: 1 }).diagnostics)
}

#[test]
fn deduplicated_packages_survive_history_replay() {
    let dir = temp("replay");
    let libs = libs(&dir);
    let cache = ts_r_cache::Cache::open_in(
        &dir.join("cache"),
        persist::NAMESPACE,
        persist::FORMAT_VERSION,
    )
    .expect("cache");
    let fs = project(&[]);
    assert_eq!(fresh(&fs, &libs), Vec::<u32>::new(), "fresh");
    let cfg = load_tsconfig(&fs, "/p/tsconfig.json");
    let mut modes = Vec::new();
    for run in 0..3 {
        let out = persist::check(&fs, Some(&cache), &cfg, Some(&libs), 1);
        modes.push(out.mode);
        assert_eq!(
            codes(&out.diagnostics),
            Vec::<u32>::new(),
            "run {run} ({:?})",
            out.mode
        );
    }
    assert_eq!(modes[0], Mode::Cold);
    // An edit to the root: the history re-checks it against the deduplicated copy.
    let fs = project(&[(
        "main.ts",
        "import { a } from \"a\";\nimport { b } from \"b\";\na(b());\nlet n: number = \"s\";\n",
    )]);
    let cfg = load_tsconfig(&fs, "/p/tsconfig.json");
    let want = fresh(&fs, &libs);
    assert_eq!(want, [2322]);
    for run in 0..2 {
        let out = persist::check(&fs, Some(&cache), &cfg, Some(&libs), 1);
        assert_eq!(
            codes(&out.diagnostics),
            want,
            "edited run {run} ({:?})",
            out.mode
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A copy that only an edit brings in: the history saw one `x`, the edited root reaches the
/// second through `b`; the run deduplicates as a fresh load does.
#[test]
fn a_duplicate_brought_in_by_an_edit_is_deduplicated() {
    let dir = temp("edit");
    let libs = libs(&dir);
    let cache = ts_r_cache::Cache::open_in(
        &dir.join("cache"),
        persist::NAMESPACE,
        persist::FORMAT_VERSION,
    )
    .expect("cache");
    let only_a = project(&[(
        "main.ts",
        "import { a } from \"a\";\ndeclare const x: Parameters<typeof a>[0];\na(x);\n",
    )]);
    let cfg = load_tsconfig(&only_a, "/p/tsconfig.json");
    for _ in 0..2 {
        let out = persist::check(&only_a, Some(&cache), &cfg, Some(&libs), 1);
        assert_eq!(codes(&out.diagnostics), fresh(&only_a, &libs));
    }
    let both = project(&[]);
    let cfg = load_tsconfig(&both, "/p/tsconfig.json");
    for run in 0..2 {
        let out = persist::check(&both, Some(&cache), &cfg, Some(&libs), 1);
        assert_eq!(
            codes(&out.diagnostics),
            Vec::<u32>::new(),
            "run {run} ({:?})",
            out.mode
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
