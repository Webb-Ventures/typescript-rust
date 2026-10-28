//! Overload groups: an implementation followed later by a body-less declaration of the same
//! member is TS2391/TS2390/TS2516, and finding that next declaration is linear in the
//! number of declarations. No lib files needed (`noLib`).

use std::time::{Duration, Instant};

use ts_r_checker::{CheckRun, check_program, load_program};
use ts_r_config::{MemoryFs, load_tsconfig};

ts_r_testing::cap_memory!();

const LIB: &str = "interface Array<T> { length: number; [n: number]: T; }\ninterface Boolean {}\ninterface Function {}\ninterface CallableFunction {}\ninterface NewableFunction {}\ninterface IArguments {}\ninterface Number {}\ninterface Object {}\ninterface RegExp {}\ninterface String {}\n";

/// `line:col TScode` for `src` as `/p/a.ts`.
fn check(src: &str) -> Vec<String> {
    let mut fs = MemoryFs::new();
    fs.set_cwd("/p");
    fs.add_file("/p/a.ts", src);
    fs.add_file("/p/lib.d.ts", LIB);
    fs.add_file(
        "/p/tsconfig.json",
        r#"{"compilerOptions": {"noLib": true, "strict": true, "types": []}, "files": ["a.ts", "lib.d.ts"]}"#,
    );
    let cfg = load_tsconfig(&fs, "/p/tsconfig.json");
    let program = load_program(&fs, &cfg.file_names, &cfg.options, None, 1);
    let result = check_program(&program, CheckRun { threads: 1 });
    result
        .diagnostics
        .iter()
        .map(|d| {
            let f = &program.files[d.file.0 as usize];
            let lc = f.lines().line_col_utf16(&f.text, d.span.start);
            format!("{}:{} TS{}", lc.line + 1, lc.col + 1, d.code)
        })
        .collect()
}

/// tsc 7.0.2 (`--strict`): 2:10 TS2391, 4:10 TS2391, 8:5 TS2389, 8:5 TS2391, 9:5 TS2391,
/// 10:5 TS2391.
#[test]
fn implementations_followed_by_overloads() {
    let src = "export {};\nfunction f() {}\nconst x = 1;\nfunction f(): void;\nclass C {\n    m() {}\n    static m(): void;\n    n() {}\n    k(): void;\n    n(): void;\n}\n";
    assert_eq!(
        check(src),
        [
            "2:10 TS2391",
            "4:10 TS2391",
            "8:5 TS2389",
            "8:5 TS2391",
            "9:5 TS2391",
            "10:5 TS2391"
        ]
    );
}

/// 20,000 uniquely named implementations, then 20,000 uniquely named lone overloads
/// (TS2391 each): no declaration scans the rest of the list for a later one of its member
/// (debug build: 59 s when each did, 1.1 s now).
#[test]
fn many_declarations_are_grouped_linearly() {
    let n = 20_000;
    let mut src = String::from("export {};\n");
    for i in 0..n {
        src.push_str(&format!("function f{i}() {{}}\n"));
    }
    for i in 0..n {
        src.push_str(&format!("function g{i}(): void;\n"));
    }
    let t = Instant::now();
    let d = check(&src);
    let took = t.elapsed();
    assert_eq!(d.len(), n);
    assert!(d.iter().all(|l| l.ends_with("TS2391")), "{:?}", &d[..3]);
    assert!(took < Duration::from_secs(20), "{took:?}");
}
