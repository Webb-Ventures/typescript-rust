//! Reachability work is shared across statements and reset with worker diagnostics.

use ts_r_checker::{Checker, Ck, LibStore, load_program};
use ts_r_config::{MemoryFs, load_tsconfig};
use ts_r_diagnostics::FileId;

ts_r_testing::cap_memory!();

#[test]
fn unreachable_runs_are_reported_after_worker_reuse() {
    let Some(libs) = LibStore::locate() else {
        return;
    };
    let mut fs = MemoryFs::new();
    fs.set_cwd("/p");
    fs.add_file(
        "/p/a.ts",
        "declare function fail(): never; function a() { fail(); 1; }",
    );
    fs.add_file("/p/b.ts", "export const b = 1;");
    fs.add_file(
        "/p/tsconfig.json",
        r#"{"compilerOptions": {"strict": true, "target": "es2022", "allowUnreachableCode": false}, "files": ["a.ts", "b.ts"]}"#,
    );
    let cfg = load_tsconfig(&fs, "/p/tsconfig.json");
    let program = load_program(&fs, &cfg.file_names, &cfg.options, Some(&libs), 1);
    let a = FileId(program.file_index("/p/a.ts").unwrap() as u32);
    let b = FileId(program.file_index("/p/b.ts").unwrap() as u32);
    let checker = Checker::new(&program);
    let mut ck = Ck::new(&checker, a);
    let first = ck.check_source_file(a);
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].code, 7027);
    for file in [a, b, a] {
        let diagnostics = ck.check_source_file(file);
        if file == a {
            assert_eq!(diagnostics.len(), 1);
            assert_eq!(diagnostics[0].code, first[0].code);
            assert_eq!(diagnostics[0].span, first[0].span);
        } else {
            assert!(diagnostics.is_empty(), "{diagnostics:?}");
        }
    }
}

#[test]
fn long_call_chain_reuses_statement_reachability() {
    let Some(libs) = LibStore::locate() else {
        return;
    };
    let mut src = String::from("declare function f(): void; function a() {\n");
    src.push_str(&"f();\n".repeat(50_000));
    src.push_str("}\n");
    let mut fs = MemoryFs::new();
    fs.set_cwd("/p");
    fs.add_file("/p/a.ts", &src);
    fs.add_file(
        "/p/tsconfig.json",
        r#"{"compilerOptions": {"strict": true, "target": "es2022", "allowUnreachableCode": false}, "files": ["a.ts"]}"#,
    );
    let cfg = load_tsconfig(&fs, "/p/tsconfig.json");
    let program = load_program(&fs, &cfg.file_names, &cfg.options, Some(&libs), 1);
    let file = FileId(program.file_index("/p/a.ts").unwrap() as u32);
    let checker = Checker::new(&program);
    let start = std::time::Instant::now();
    let diagnostics = Ck::new(&checker, file).check_source_file(file);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert!(
        start.elapsed().as_secs() < 10,
        "slow statement reachability"
    );
}
