//! TS2688 for `/// <reference types>` directives the loader could not resolve, reported per
//! file from `ProgramFiles::missing_type_references_by_file` (grouped once per check, not
//! scanned per file). No lib files needed (`noLib`).

use ts_r_checker::{CheckRun, check_program, load_program};
use ts_r_config::{MemoryFs, load_tsconfig};

ts_r_testing::cap_memory!();

#[test]
fn missing_type_references_are_grouped_by_file() {
    let mut fs = MemoryFs::new();
    fs.set_cwd("/p");
    let n = 300;
    let mut files = Vec::new();
    for i in 0..n {
        let name = format!("f{i:03}.ts");
        let text = if i % 3 == 0 {
            format!(
                "/// <reference types=\"nope{i}\" />\n/// <reference types=\"other\" />\nexport {{}};\n"
            )
        } else {
            "export {};\n".to_owned()
        };
        fs.add_file(&format!("/p/{name}"), &text);
        files.push(format!("\"{name}\""));
    }
    fs.add_file(
        "/p/tsconfig.json",
        &format!(
            r#"{{"compilerOptions": {{"noLib": true, "types": []}}, "files": [{}]}}"#,
            files.join(",")
        ),
    );
    let cfg = load_tsconfig(&fs, "/p/tsconfig.json");
    let program = load_program(&fs, &cfg.file_names, &cfg.options, None, 1);
    let by_file = program.missing_type_references_by_file();
    assert_eq!(by_file.len(), n / 3);
    assert_eq!(by_file["/p/f003.ts"], ["nope3", "other"]);
    assert!(!by_file.contains_key("/p/f001.ts"));
    let result = check_program(&program, CheckRun { threads: 1 });
    let ts2688: Vec<(String, String)> = result
        .diagnostics
        .iter()
        .filter(|d| d.code == 2688)
        .map(|d| {
            let f = &program.files[d.file.0 as usize];
            (f.path.clone(), d.message.clone())
        })
        .collect();
    assert_eq!(
        ts2688.len(),
        2 * n / 3,
        "{:?}",
        &ts2688[..4.min(ts2688.len())]
    );
    assert_eq!(
        ts2688[0],
        (
            "/p/f000.ts".to_owned(),
            "Cannot find type definition file for 'nope0'.".to_owned()
        )
    );
    assert_eq!(ts2688[1].1, "Cannot find type definition file for 'other'.");
}

/// tsc 7 drops the include diagnostics of a file it skips checking: an unresolved
/// `/// <reference types>` in a declaration file under `skipLibCheck` is no TS2688
/// (`@testing-library/jest-dom`'s `types/jest.d.ts` without `@types/jest`).
#[test]
fn skip_lib_check_drops_declaration_file_type_references() {
    for (skip, expected) in [
        (true, vec!["/p/a.ts"]),
        (false, vec!["/p/a.ts", "/p/lib.d.ts"]),
    ] {
        let mut fs = MemoryFs::new();
        fs.set_cwd("/p");
        fs.add_file("/p/a.ts", "/// <reference types=\"nope\" />\nexport {};\n");
        fs.add_file(
            "/p/lib.d.ts",
            "/// <reference types=\"jest\" />\nexport {};\n",
        );
        fs.add_file(
            "/p/tsconfig.json",
            &format!(
                r#"{{"compilerOptions": {{"noLib": true, "types": [], "skipLibCheck": {skip}}}, "files": ["a.ts", "lib.d.ts"]}}"#
            ),
        );
        let cfg = load_tsconfig(&fs, "/p/tsconfig.json");
        let program = load_program(&fs, &cfg.file_names, &cfg.options, None, 1);
        let result = check_program(&program, CheckRun { threads: 1 });
        let mut files: Vec<&str> = result
            .diagnostics
            .iter()
            .filter(|d| d.code == 2688)
            .map(|d| program.files[d.file.0 as usize].path.as_str())
            .collect();
        files.sort_unstable();
        assert_eq!(files, expected, "skipLibCheck: {skip}");
    }
}
