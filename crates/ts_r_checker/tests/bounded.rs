//! Inputs that used to make the checker's work grow exponentially (found on trpc, rxjs and
//! playwright): speculative checks of callbacks re-checking everything under them, and
//! structural relations of big generic interfaces. Each must finish quickly with tsc's
//! diagnostics (expected output taken from tsc 7.0.2).
//! Skipped when the lib files are not available (`cargo xtask upstream sync --minimal`).

use ts_r_checker::{CheckRun, LibStore, check_program, load_program};
use ts_r_config::{MemoryFs, load_tsconfig};

ts_r_testing::cap_memory!();

/// Checks `src` as `/p/a.ts` under `strict` and returns `line:col TScode` lines.
fn check(src: &str) -> Option<Vec<String>> {
    let libs = LibStore::locate()?;
    let mut fs = MemoryFs::new();
    fs.set_cwd("/p");
    fs.add_file("/p/a.ts", src);
    fs.add_file(
        "/p/tsconfig.json",
        "{\"compilerOptions\": {\"strict\": true, \"target\": \"es2022\"}, \"files\": [\"a.ts\"]}",
    );
    let cfg = load_tsconfig(&fs, "/p/tsconfig.json");
    let program = load_program(&fs, &cfg.file_names, &cfg.options, Some(&libs), 1);
    let result = check_program(&program, CheckRun { threads: 1 });
    Some(
        result
            .diagnostics
            .iter()
            .map(|d| {
                let f = &program.files[d.file.0 as usize];
                let lc = f.lines().line_col_utf16(&f.text, d.span.start);
                format!("{}:{} TS{}", lc.line + 1, lc.col + 1, d.code)
            })
            .collect(),
    )
}

/// `run(<callback>)` whose body is a chain of `n` locals, each built from the one before.
fn local_chain(param: bool, n: usize) -> String {
    let mut s = String::new();
    if param {
        s.push_str("declare function run<T>(f: (x: number) => T): T;\nrun((x) => {\n");
        s.push_str("  const a0 = { v: x };\n");
    } else {
        s.push_str("declare function run<T>(f: () => T): T;\nrun(() => {\n");
        s.push_str("  const a0 = { v: 1 };\n");
    }
    for i in 1..=n {
        s.push_str(&format!("  const a{i} = {{ v: a{} }};\n", i - 1));
    }
    s.push_str(&format!("  return a{n};\n}});\n"));
    s
}

#[test]
fn locals_of_a_speculative_callback_are_typed_once() {
    for param in [false, true] {
        let Some(out) = check(&local_chain(param, 40)) else {
            return;
        };
        assert!(out.is_empty(), "{out:?}");
    }
}

#[test]
fn large_object_literal_against_union() {
    let mut numbers = String::from("interface Numbers {\n");
    let mut strings = String::from("interface Strings {\n");
    let mut value = String::from("const value: Numbers | Strings = {\n");
    for i in 0..1000 {
        numbers.push_str(&format!("p{i}: number;\n"));
        strings.push_str(&format!("p{i}: string;\n"));
        value.push_str(&format!("p{i}: 1,\n"));
    }
    let src = format!("{numbers}}}\n{strings}}}\n{value}}};\n");
    let Some(out) = check(&src) else {
        return;
    };
    assert!(out.is_empty(), "{out:?}");
}

#[test]
fn wide_array_of_context_sensitive_callbacks() {
    let mut src = String::from(
        "declare function f(fs: ((x: number) => number)[]): void;\n\
         declare function f(fs: ((x: string) => string)[]): void;\nf([\n",
    );
    for _ in 0..2048 {
        src.push_str("x => x,\n");
    }
    src.push_str("]);\n");
    let Some(out) = check(&src) else {
        return;
    };
    assert!(out.is_empty(), "{out:?}");
}

#[test]
fn locals_used_twice_in_nested_callbacks() {
    let mut s = String::from(
        "declare function run<T>(f: (x: number) => T): T;\nrun((x) => {\n  const a0 = { v: x };\n",
    );
    for i in 1..=40 {
        s.push_str(&format!(
            "  const a{i} = [a{0}, a{0}].map((y) => ({{ v: y }}))[0];\n",
            i - 1
        ));
    }
    s.push_str("  return a40;\n});\n");
    let Some(out) = check(&s) else {
        return;
    };
    assert!(out.is_empty(), "{out:?}");
}

#[test]
fn deeply_nested_generic_callbacks() {
    let mut e = String::from("0");
    for i in 0..60 {
        e = format!("g((x{i}) => {e})");
    }
    let src =
        format!("declare function g<T>(f: (x: number) => T): T;\nexport const r: number = {e};\n");
    let Some(out) = check(&src) else {
        return;
    };
    assert!(out.is_empty(), "{out:?}");
}

/// typeorm's `findChangedColumns`: a long `||` chain of comparisons narrowing a `const`
/// from `find` inside a `filter` callback (TS2589 from the speculation budget before).
#[test]
fn condition_chain_in_a_speculative_callback() {
    let n = 40;
    let mut s = String::from("interface A {");
    for i in 0..n {
        s.push_str(&format!(" p{i}: string | undefined;"));
    }
    s.push_str(" }\ndeclare const xs: A[]; declare const ys: A[];\n");
    s.push_str("export const r = xs.filter((b) => {\n  const a = ys.find((c) => c.p0 === b.p0)\n");
    s.push_str("  if (!a) return false\n  const r =");
    for i in 0..n {
        let or = if i + 1 < n { " ||" } else { "" };
        s.push_str(&format!("\n    a.p{i} !== b.p{i}{or}"));
    }
    s.push_str("\n  return r\n})\n");
    let Some(out) = check(&s) else {
        return;
    };
    assert!(out.is_empty(), "{out:?}");
}

#[test]
fn relating_big_generic_interfaces_is_bounded() {
    let mut s = String::from("interface Obs<T> {\n");
    for i in 0..60 {
        s.push_str(&format!("  m{i}<U>(f: (x: T) => U): Obs<{{ k{i}: U }}>;\n"));
        s.push_str(&format!("  m{i}(): Obs<{{ k{i}: T }}>;\n"));
    }
    s.push_str("}\ninterface Conn<T> extends Obs<T> { connect(): void }\n");
    s.push_str("declare const o: Obs<number>;\n");
    s.push_str("export const f = o.m0 as (f: unknown) => Conn<number>;\n");
    let Some(out) = check(&s) else {
        return;
    };
    assert_eq!(out, vec!["125:18 TS2352"]);
}

/// A flat object pattern with defaults over an object literal: each property's contextual
/// type asks for the pattern's implied type, which is built once (5,000 properties took 99 s
/// and 3.6 GB in release before the cache, 0.1 s after).
#[test]
fn wide_object_pattern_with_defaults() {
    let n = 3000;
    let mut pattern = String::from("const { f = (x: number) => 0");
    let mut value = String::from("{ f: x => x");
    for i in 0..n {
        pattern.push_str(&format!(", p{i} = 0"));
        value.push_str(&format!(", p{i}: 0"));
    }
    let src = format!("{pattern} }} = {value} }};\nexport {{}};\n");
    let start = std::time::Instant::now();
    let Some(out) = check(&src) else {
        return;
    };
    assert!(out.is_empty(), "{out:?}");
    let took = start.elapsed();
    assert!(took.as_secs() < 30, "took {took:?}");
}

/// A JS constructor of 6,000 `this.pN = 0;` statements: whether each property is
/// definitely assigned is one walk of the constructor (cached), not one per property
/// with a budget that ran out near 3,000 and made the rest `number | undefined`. A
/// statement ahead of every `return` stays `number` (tsc 7.0.2). The 50,000-property
/// case is `ts_r_emit`'s `javascript_many_this_properties_are_linear`.
#[test]
fn many_this_properties_of_a_js_constructor_stay_definitely_assigned() {
    let Some(libs) = LibStore::locate() else {
        return;
    };
    let n = 6_000;
    let mut src =
        String::from("export class C {\n  /** @param {boolean} b */\n  constructor(b) {\n");
    for i in 0..n {
        src.push_str(&format!("    this.p{i} = 0;\n"));
    }
    src.push_str("    if (b) return;\n    this.late = 0;\n  }\n}\n");
    src.push_str(&format!(
        "/** @type {{string}} */\nvar a = new C(true).p{};\n/** @type {{string}} */\nvar b = new C(true).late;\n",
        n - 1
    ));
    let mut fs = MemoryFs::new();
    fs.set_cwd("/p");
    fs.add_file("/p/a.js", &src);
    fs.add_file(
        "/p/tsconfig.json",
        "{\"compilerOptions\": {\"strict\": true, \"allowJs\": true, \"checkJs\": true, \"target\": \"es2022\"}, \"files\": [\"a.js\"]}",
    );
    let cfg = load_tsconfig(&fs, "/p/tsconfig.json");
    let program = load_program(&fs, &cfg.file_names, &cfg.options, Some(&libs), 1);
    let start = std::time::Instant::now();
    let result = check_program(&program, CheckRun { threads: 1 });
    let took = start.elapsed();
    let msgs: Vec<&str> = result
        .diagnostics
        .iter()
        .map(|d| d.message.as_str())
        .collect();
    assert_eq!(
        msgs,
        [
            "Type 'number' is not assignable to type 'string'.",
            "Type 'number | undefined' is not assignable to type 'string'.\n  Type 'undefined' is not assignable to type 'string'.",
        ],
        "{msgs:?}"
    );
    assert!(took.as_secs() < 60, "took {took:?}");
}
