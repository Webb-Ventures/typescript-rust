//! A generic alias that recurses with growing arguments (`type Grow<T> = Array<T | Grow<T[]>>`)
//! resolves to the same types whichever instantiation is resolved first: a forward
//! reference created past the linking depth is linked when it is met again. No lib files
//! needed (`noLib`).

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

#[test]
fn growing_alias_arguments_do_not_depend_on_resolution_order() {
    let deep = format!("number{}", "[]".repeat(17));
    let alias = "type Grow<T> = Array<T | Grow<T[]>>;\n";
    let deep_first = check(&format!(
        "{alias}export const b: Grow<{deep}> = [\"x\"];\nexport declare const a: Grow<number>;\n"
    ));
    let shallow_first = check(&format!(
        "{alias}export declare const a: Grow<number>;\nexport const b: Grow<{deep}> = [\"x\"];\n"
    ));
    // The string is neither the deep array nor a `Grow` of it (tsc 7.0.2, both orders:
    // `error TS2322: Type 'string' is not assignable to type 'number[]...[] |
    // Grow<number[]...[][]>'`, at 2:67 and 3:67).
    assert_eq!(deep_first, ["2:67 TS2322"], "deep first");
    assert_eq!(shallow_first, ["3:67 TS2322"], "shallow first");
}

/// Relating values far past the linking depth stays bounded: each unlinked forward met is
/// linked at most to the depth limit, and then reads as linked.
#[test]
fn growing_alias_relations_past_the_depth_limit_terminate() {
    let mut nested = String::from("1");
    for _ in 0..40 {
        nested = format!("[{nested}, {nested}]");
        if nested.len() > 1_000 {
            break;
        }
    }
    let mut src = String::from("type Grow<T> = Array<T | Grow<T[]>>;\n");
    for i in 0..10 {
        src.push_str(&format!("export const v{i}: Grow<number> = {nested};\n"));
    }
    let t = std::time::Instant::now();
    let out = check(&src);
    eprintln!("relations: {:?}", t.elapsed());
    assert!(t.elapsed().as_secs() < 60, "{:?}", t.elapsed());
    assert!(out.len() <= 10, "{out:?}");
}
