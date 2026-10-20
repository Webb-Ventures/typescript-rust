//! Instantiation-query names follow types shared between workers and program levels.

use ts_r_checker::{CheckRun, Checker, Ck, LibStore, check_program, load_program};
use ts_r_config::{MemoryFs, load_tsconfig};
use ts_r_diagnostics::FileId;

ts_r_testing::cap_memory!();

#[test]
fn cached_instantiation_query_names() {
    let Some(libs) = LibStore::locate() else {
        return;
    };
    let mut fs = MemoryFs::new();
    fs.set_cwd("/p");
    fs.add_file(
        "/p/a.ts",
        "export declare function f<T>(x: T): T;\nexport declare const g: typeof f<string>;\n",
    );
    fs.add_file(
        "/p/b.ts",
        "import { g } from './a';\nconst n: number = g;\n",
    );
    fs.add_file(
        "/p/tsconfig.json",
        r#"{"compilerOptions": {"strict": true, "target": "es2022"}, "files": ["a.ts", "b.ts"]}"#,
    );
    let cfg = load_tsconfig(&fs, "/p/tsconfig.json");
    let program = load_program(&fs, &cfg.file_names, &cfg.options, Some(&libs), 1);
    let a = FileId(program.file_index("/p/a.ts").unwrap() as u32);
    let b = FileId(program.file_index("/p/b.ts").unwrap() as u32);
    let expected = "Type 'typeof f<string>' is not assignable to type 'number'.";
    let checker = Checker::new(&program);
    assert!(Ck::new(&checker, a).check_source_file(a).is_empty());
    // A fresh worker must print the cached type's name without checking its declaration.
    let diagnostics = Ck::new(&checker, b).check_source_file(b);
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].code, 2322);
    assert_eq!(diagnostics[0].message, expected);

    let child = Checker::layered(&program, Some(checker.top_layer())).unwrap();
    let diagnostics = Ck::new(&child, b).check_source_file(b);
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].message, expected);

    for threads in [1, 4] {
        let result = check_program(&program, CheckRun { threads });
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(result.diagnostics[0].message, expected);
    }
}
