//! `@ts-ignore` / `@ts-expect-error` over program-level (link) diagnostics: a global merge's
//! TS2300 under a directive is dropped, as tsc 7 applies the directives to every semantic
//! error of a file (@colors/colors). The filter reads each file's comments once, so many
//! merge errors in a file with many comments stay linear. No lib files needed (`noLib`).

use ts_r_checker::driver::{LinkDiagnosticFilter, link_diagnostic_reported};
use ts_r_checker::{CheckRun, ProgramFiles, check_program, load_program};
use ts_r_config::{MemoryFs, load_tsconfig};

ts_r_testing::cap_memory!();

const LIB: &str = "interface Array<T> {}\ninterface Boolean {}\ninterface Function {}\ninterface CallableFunction {}\ninterface NewableFunction {}\ninterface IArguments {}\ninterface Number {}\ninterface Object {}\ninterface RegExp {}\ninterface String {}\n";

/// `a.ts` and `b.ts` as global scripts, with `noLib` and the minimal `lib.d.ts` above.
fn program(a: &str, b: &str) -> ProgramFiles {
    let mut fs = MemoryFs::new();
    fs.set_cwd("/p");
    fs.add_file("/p/a.ts", a);
    fs.add_file("/p/b.ts", b);
    fs.add_file("/p/lib.d.ts", LIB);
    fs.add_file(
        "/p/tsconfig.json",
        r#"{"compilerOptions": {"noLib": true, "types": []}, "files": ["a.ts", "b.ts", "lib.d.ts"]}"#,
    );
    let cfg = load_tsconfig(&fs, "/p/tsconfig.json");
    load_program(&fs, &cfg.file_names, &cfg.options, None, 1)
}

fn lines(program: &ProgramFiles, diags: &[ts_r_diagnostics::Diagnostic]) -> Vec<String> {
    diags
        .iter()
        .map(|d| {
            let f = &program.files[d.file.0 as usize];
            let lc = f.lines().line_col_utf16(&f.text, d.span.start);
            format!("{}:{}:{} TS{}", f.path, lc.line + 1, lc.col + 1, d.code)
        })
        .collect()
}

/// A method in one file and a property in another of the same global interface: TS2300 at
/// both, except the one a `// @ts-ignore` governs (a documentation comment does not).
#[test]
fn directives_drop_link_diagnostics_they_govern() {
    let a = "interface G {\n    m(): void;\n    n(): void;\n}\n";
    let b = "interface G {\n    /** Documented. */\n    m: number;\n    // @ts-ignore\n    n: number;\n}\n";
    let p = program(a, b);
    let result = check_program(&p, CheckRun { threads: 1 });
    assert_eq!(
        lines(&p, &result.diagnostics),
        [
            "/p/a.ts:2:5 TS2300",
            "/p/a.ts:3:5 TS2300",
            "/p/b.ts:3:5 TS2300"
        ]
    );
}

/// The filter answers as the one-off function does, reading each file's directives once:
/// 20,000 clashes, each property under a documentation comment (`link_diagnostic_reported`
/// per diagnostic read all 20,000 comments each time: 400 million comment visits).
#[test]
fn filter_matches_the_one_off_check_and_stays_linear() {
    let n = 20_000;
    let mut a = String::from("interface G {\n");
    let mut b = String::from("interface G {\n");
    for i in 0..n {
        a.push_str(&format!("    m{i}(): void;\n"));
        if i % 1000 == 0 {
            b.push_str("    // @ts-expect-error\n");
        } else {
            b.push_str("    /** Documented. */\n");
        }
        b.push_str(&format!("    m{i}: number;\n"));
    }
    a.push_str("}\n");
    b.push_str("}\n");
    let p = program(&a, &b);
    let result = check_program(&p, CheckRun { threads: 1 });
    // Every method's, and every property's but the 20 under a directive.
    assert_eq!(result.diagnostics.len(), 2 * n - 20);
    // A sample through the one-off function agrees with the cached filter.
    let checker = ts_r_checker::Checker::new(&p);
    let mut filter = LinkDiagnosticFilter::new(&p);
    let link = &checker.link.diagnostics;
    let kept = link.iter().filter(|d| filter.reported(d)).count();
    assert_eq!(kept, 2 * n - 20);
    for d in link.iter().step_by(997) {
        assert_eq!(filter.reported(d), link_diagnostic_reported(&p, d));
    }
}
