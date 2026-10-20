//! Large parameter lists and object literals whose grammar checks must stay bounded.

use ts_r_checker::{CheckRun, check_program, load_program};
use ts_r_config::{MemoryFs, load_tsconfig};

ts_r_testing::cap_memory!();

fn check(src: &str) -> Vec<u32> {
    let mut fs = MemoryFs::new();
    fs.set_cwd("/p");
    fs.add_file("/p/a.ts", src);
    fs.add_file(
        "/p/tsconfig.json",
        r#"{"compilerOptions": {"strict": true, "target": "es2022", "noLib": true, "types": []}, "files": ["a.ts"]}"#,
    );
    let cfg = load_tsconfig(&fs, "/p/tsconfig.json");
    let program = load_program(&fs, &cfg.file_names, &cfg.options, None, 1);
    check_program(&program, CheckRun { threads: 1 })
        .diagnostics
        .iter()
        .map(|d| d.code)
        // `noLib`: the missing core global types (TS2318) are not what these tests check.
        .filter(|&c| c != 2318)
        .collect()
}

#[test]
fn repeated_outer_references_in_a_parameter_initializer() {
    for shadowed in [false, true] {
        let mut src = String::from("const x = 1; function f(p = [");
        for _ in 0..30_000 {
            src.push_str("x,");
        }
        src.push_str("]) {");
        if shadowed {
            src.push_str("var x;");
        }
        src.push('}');
        let out = check(&src);
        assert!(out.is_empty(), "{out:?}");
    }
}

#[test]
fn repeated_object_spreads_after_many_keys() {
    let mut src = String::from("declare const a: any; const o = {");
    for i in 0..30_000 {
        src.push_str(&format!("p{i}: 1,"));
    }
    for _ in 0..30_000 {
        src.push_str("...a,");
    }
    src.push_str("p0: 2,p0: 3};");
    assert_eq!(check(&src), vec![1117, 1117]);
}
