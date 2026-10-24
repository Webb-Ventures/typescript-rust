//! End-to-end checks of small programs (expected output taken from tsc 7.0.2).
//! Skipped when the lib files are not available (`cargo xtask upstream sync --minimal`).

use ts_r_checker::{
    CheckRun, LibStore, check_program, check_program_through_syntax_errors, load_program,
};
use ts_r_config::{MemoryFs, load_tsconfig};

ts_r_testing::cap_memory!();

/// Checks `src` as `/p/a.ts` with `options` (tsconfig `compilerOptions` members) and
/// returns `line:col TScode: message` lines.
fn check(options: &str, src: &str) -> Option<Vec<String>> {
    let libs = LibStore::locate()?;
    let mut fs = MemoryFs::new();
    fs.set_cwd("/p");
    fs.add_file("/p/a.ts", src);
    fs.add_file(
        "/p/tsconfig.json",
        &format!("{{\"compilerOptions\": {{{options}}}, \"files\": [\"a.ts\"]}}"),
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
                format!("{}:{} TS{}: {}", lc.line + 1, lc.col + 1, d.code, d.message)
            })
            .collect(),
    )
}

const STRICT: &str = "\"strict\": true, \"target\": \"es2022\"";

#[test]
fn import_trailing_comma_skips_lexer_trivia() {
    let mut src = String::from("declare const path: string;\n");
    for newline in ["\n", "\r", "\r\n", "\u{2028}", "\u{2029}"] {
        src.push_str(&format!("import(path, // comment{newline});\n"));
    }
    for trivia in ["\u{a0}", "\u{feff}", "/* comment */"] {
        src.push_str(&format!("import(path{trivia},{trivia});\n"));
    }
    src.push_str("import(path /* no comma */);\n");
    for (module, expected) in [("commonjs", 8), ("esnext", 0)] {
        let mut fs = MemoryFs::new();
        fs.set_cwd("/p");
        fs.add_file("/p/a.ts", &src);
        fs.add_file(
            "/p/tsconfig.json",
            &format!(
                "{{\"compilerOptions\": {{{STRICT}, \"module\": \"{module}\", \"noLib\": true}}, \"files\": [\"a.ts\"]}}"
            ),
        );
        let cfg = load_tsconfig(&fs, "/p/tsconfig.json");
        let program = load_program(&fs, &cfg.file_names, &cfg.options, None, 1);
        let result = check_program(&program, CheckRun { threads: 1 });
        let commas: Vec<_> = result
            .diagnostics
            .iter()
            .filter(|d| d.code == 1009)
            .collect();
        assert_eq!(commas.len(), expected, "{module}: {commas:#?}");
        for diagnostic in commas {
            let file = &program.files[diagnostic.file.0 as usize];
            assert_eq!(
                file.text
                    .get(diagnostic.span.start as usize..diagnostic.span.end as usize),
                Some(",")
            );
        }
    }
}

#[test]
fn assignability_and_calls() {
    let Some(out) = check(
        STRICT,
        r#"let a: number = "x";
const b = [1, 2, 3].map(x => x * 2);
const c: string = b[0];
const arr: string[] = [];
arr.push(1);
Math.max(1, "2");
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        vec![
            "1:5 TS2322: Type 'string' is not assignable to type 'number'.",
            "3:7 TS2322: Type 'number' is not assignable to type 'string'.",
            "5:10 TS2345: Argument of type 'number' is not assignable to parameter of type 'string'.",
            "6:13 TS2345: Argument of type 'string' is not assignable to parameter of type 'number'.",
        ]
    );
}

#[test]
fn narrowing() {
    let Some(out) = check(
        STRICT,
        r#"function f(x: string | number) {
    if (typeof x === "string") {
        return x.length;
    }
    return x.toFixed(2);
}
const d: boolean = f(1);
type U = { kind: "a"; a: number } | { kind: "b"; b: string };
function g(u: U) {
    switch (u.kind) {
        case "a": return u.a;
        case "b": return u.b.length;
    }
}
function h(s?: string) { return s.length; }
let q: number;
q;
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        vec![
            "7:7 TS2322: Type 'string | number' is not assignable to type 'boolean'.\n  Type 'string' is not assignable to type 'boolean'.",
            "15:33 TS18048: 's' is possibly 'undefined'.",
            "17:1 TS2454: Variable 'q' is used before being assigned.",
        ]
    );
}

#[test]
fn objects_and_classes() {
    let Some(out) = check(
        STRICT,
        r#"interface P { name: string; age?: number }
const p: P = { name: "a", agee: 1 };
let z: Foo;
class K { x: number; constructor() {} m() { return this.x; } }
const k = new K();
k.y;
const o = { a: 1 } as const;
o.a = 2;
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        vec![
            "2:27 TS2561: Object literal may only specify known properties, but 'agee' does not exist in type 'P'. Did you mean to write 'age'?",
            "3:8 TS2304: Cannot find name 'Foo'.",
            "4:11 TS2564: Property 'x' has no initializer and is not definitely assigned in the constructor.",
            "6:3 TS2339: Property 'y' does not exist on type 'K'.",
            "8:3 TS2540: Cannot assign to 'a' because it is a read-only property.",
        ]
    );
}

#[test]
fn comment_directives() {
    let Some(out) = check(
        STRICT,
        r#"let x: number = 1;
// @ts-ignore
x.foo;

// @ts-ignore

x.bar;
// @ts-expect-error
let ok = 1;
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        vec!["8:1 TS2578: Unused '@ts-expect-error' directive."]
    );
}

/// A `.json` file in `include` (with `resolveJsonModule`) is a JSON value, not statements,
/// and is never type-checked: date-fns includes its `package.json` (tsc 7.0.2 reports
/// nothing; broken JSON gets only a syntax error).
#[test]
fn json_files_in_include() {
    let Some(libs) = LibStore::locate() else {
        return;
    };
    let run = |json: &str| {
        let mut fs = MemoryFs::new();
        fs.set_cwd("/p");
        fs.add_file("/p/a.ts", "export const x = 1;\n");
        fs.add_file("/p/package.json", json);
        fs.add_file(
            "/p/tsconfig.json",
            "{\"compilerOptions\": {\"strict\": true, \"module\": \"nodenext\", \"resolveJsonModule\": true}, \"include\": [\"*.ts\", \"package.json\"]}",
        );
        let cfg = load_tsconfig(&fs, "/p/tsconfig.json");
        assert!(cfg.file_names.iter().any(|f| f.ends_with("package.json")));
        let program = load_program(&fs, &cfg.file_names, &cfg.options, Some(&libs), 1);
        let result = check_program(&program, CheckRun { threads: 1 });
        result
            .diagnostics
            .iter()
            .map(|d| {
                let f = &program.files[d.file.0 as usize];
                let lc = f.lines().line_col_utf16(&f.text, d.span.start);
                format!("{} {}:{} TS{}", f.path, lc.line + 1, lc.col + 1, d.code)
            })
            .collect::<Vec<_>>()
    };
    let ok = "{\n  \"name\": \"x\",\n  \"a\": [1, 2, {\"b\": null}],\n  \"a\": true\n}\n";
    assert_eq!(run(ok), Vec::<String>::new());
    let broken = run("{\n  \"name\": \"x\",\n  \"v\": [1, 2,\n}\n");
    assert_eq!(broken.len(), 1, "{broken:?}");
    assert!(
        broken[0].starts_with("/p/package.json 4:1 TS1"),
        "{broken:?}"
    );
    // A value only TypeScript accepts is still a JSON syntax error (tsc 7).
    assert_eq!(run("{\"a\": undefined}\n"), ["/p/package.json 1:7 TS1328"]);
}

/// An unresolved generic type's name is printed with its arguments; nesting that doubles
/// it with every alias must stay bounded (`A30` would print 2^30 `Missing`s).
#[test]
fn unresolved_type_names_stay_bounded() {
    let mut src = String::from("type A0 = Missing;\n");
    for i in 1..=30 {
        src.push_str(&format!("type A{i} = Missing<A{0}, A{0}>;\n", i - 1));
    }
    src.push_str("let x: A30 = 1;\nlet y: number = x;\n");
    let Some(out) = check(STRICT, &src) else {
        return;
    };
    assert_eq!(out.len(), 31, "{out:?}");
    assert!(
        out.iter()
            .all(|d| d.contains("TS2304: Cannot find name 'Missing'."))
    );
}

/// Parentheses around an evolving array's `push`, `length` or index-assignment target keep
/// it evolving (tsc 7.0: no errors).
#[test]
fn parenthesized_evolving_array_operations() {
    let Some(out) = check(
        STRICT,
        "export function f() { let a = []; (a).push(1); return a; }\n\
         export function g() { let b = []; ((b)).length; (b)[0] = 1; return b; }\n",
    ) else {
        return;
    };
    assert_eq!(out, Vec::<String>::new());
}

/// A level built on a parent level that holds a re-exported interface and its augmentation
/// (both below): the redirect belongs to the parent, so the child level is built without
/// regrouping it (it used to index the parent's files from the child's file range). With the
/// augmentation above the parent the merge crosses levels, which is reported.
#[test]
fn level_above_an_augmented_reexport() {
    let Some(libs) = LibStore::locate() else {
        return;
    };
    let mut fs = MemoryFs::new();
    fs.set_cwd("/p");
    fs.add_file("/p/b.ts", "export interface Foo { a: number }\n");
    fs.add_file("/p/a.ts", "export { Foo } from \"./b\";\n");
    fs.add_file(
        "/p/c.ts",
        "import \"./a\";\ndeclare module \"./a\" { interface Foo { x: number } }\nexport {};\n",
    );
    fs.add_file(
        "/p/d.ts",
        "import type { Foo } from \"./a\";\ndeclare const f: Foo;\nexport const n: string = f.x;\n",
    );
    fs.add_file(
        "/p/tsconfig.json",
        r#"{"compilerOptions": {"strict": true, "target": "es2022"}, "files": ["b.ts", "a.ts", "c.ts", "d.ts"]}"#,
    );
    let cfg = load_tsconfig(&fs, "/p/tsconfig.json");
    let program = load_program(&fs, &cfg.file_names, &cfg.options, Some(&libs), 1);
    let d = program.file_index("/p/d.ts").unwrap();
    assert_eq!(d, program.files.len() - 1);
    let below = |end: usize| ts_r_checker::ProgramFiles {
        files: program.files[..end].to_vec(),
        resolutions: program.resolutions.clone(),
        mode_resolutions: program.mode_resolutions.clone(),
        options: program.options.clone(),
        roots: program.roots.clone(),
        missing_libs: Vec::new(),
        missing_types: Vec::new(),
        missing_type_references: Vec::new(),
        ambiguous_roots: Vec::new(),
        deps: program.deps.clone(),
    };
    let parent = ts_r_checker::Checker::new(&below(d)).top_layer().clone();
    assert!(ts_r_checker::Checker::layered(&program, Some(&parent)).is_ok());
    let c = program.file_index("/p/c.ts").unwrap();
    let parent = ts_r_checker::Checker::new(&below(c)).top_layer().clone();
    assert_eq!(
        ts_r_checker::Checker::layered(&program, Some(&parent)).err(),
        Some(ts_r_checker::LayerError::CrossLevelMerge)
    );
}

#[test]
fn doubling_tuple_spreads_are_bounded() {
    // Each alias doubles the previous tuple: 2^30 elements without a limit. Like tsc, ts-r
    // reports TS2799 at T14, the first past 10,000 elements. T15 spreads two errors, so it is
    // a tuple with a rest element and each later alias spreads two rests (TS1265, as tsc).
    let mut src = String::from("type T0 = [0];\n");
    for n in 1..=30 {
        src.push_str(&format!("type T{n} = [...T{}, ...T{}];\n", n - 1, n - 1));
    }
    src.push_str("let x: T30;\nexport {};\n");
    let Some(out) = check(STRICT, &src) else {
        return;
    };
    let mut expected = vec![
        "15:12 TS2799: Type produces a tuple type that is too large to represent.".to_string(),
    ];
    for line in 17..=31 {
        expected.push(format!(
            "{line}:21 TS1265: A rest element cannot follow another rest element."
        ));
    }
    assert_eq!(out, expected);
}

#[test]
fn circular_union_constraint_terminates() {
    // tsc also reports TS2339 for `x.foo`; ts-r gives up on the constraint instead.
    let Some(out) = check(
        STRICT,
        "function f<T extends T | string>(x: T) { x.foo; }\n",
    ) else {
        return;
    };
    assert_eq!(
        out,
        vec!["1:22 TS2313: Type parameter 'T' has a circular constraint."]
    );
}

#[test]
fn circular_thenable_terminates() {
    // tsc reports TS1062 for the `await`; ts-r gives up on the awaited type instead.
    let Some(out) = check(
        STRICT,
        r#"interface T { then(cb: (v: T | undefined) => unknown): unknown; }
async function f(x: T) { await x; }
async function g(p: Promise<number> | Promise<string | Promise<boolean>>) {
    const n: number = await p;
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        vec![
            "4:11 TS2322: Type 'string | number | boolean' is not assignable to type 'number'.\n  Type 'string' is not assignable to type 'number'."
        ]
    );
}

#[test]
fn tuple_rest_parameter_arity() {
    let Some(out) = check(
        STRICT,
        r#"declare function f(...args: [number, string]): void;
f();
f(1, "x", true);
f(1, "x");
declare function g(...args: [number, string?, ...boolean[]]): void;
g();
g(1, "a", true, false);
declare function h(a: number, ...rest: [string, boolean?]): void;
h(1);
h(1, "a", true, 2);
declare function k(a?: number, ...rest: [string]): void;
k();
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        vec![
            "2:1 TS2554: Expected 2 arguments, but got 0.",
            "3:11 TS2554: Expected 2 arguments, but got 3.",
            "6:1 TS2555: Expected at least 1 arguments, but got 0.",
            "9:1 TS2554: Expected 2-3 arguments, but got 1.",
            "10:17 TS2554: Expected 2-3 arguments, but got 4.",
            "12:1 TS2554: Expected 2 arguments, but got 0.",
        ]
    );
}

#[test]
fn readonly_writes_in_constructor_need_this() {
    let Some(out) = check(
        STRICT,
        r#"class C {
    readonly x: number = 0;
    constructor(other: C) {
        other.x = 1;
        this.x = 2;
        (this).x = 3;
        this["x"] = 4;
        other["x"] = 5;
    }
}
class D {
    readonly x: number = 0;
    b = ((o: D) => { o.x = 2; })(this);
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        vec![
            "4:15 TS2540: Cannot assign to 'x' because it is a read-only property.",
            "6:16 TS2540: Cannot assign to 'x' because it is a read-only property.",
            "8:15 TS2540: Cannot assign to 'x' because it is a read-only property.",
            "13:24 TS2540: Cannot assign to 'x' because it is a read-only property.",
        ]
    );
}

#[test]
fn tuple_union_order_ignores_creation_order() {
    // Tuple shapes sort by content, not by which shape was made first (which depends on
    // the checker threads when the tuples come from different files).
    let Some(out) = check(
        STRICT,
        "let a: [number, number] = [1, 2];\nlet b: [string] | [number, number] = true;\n",
    ) else {
        return;
    };
    assert_eq!(
        out,
        vec!["2:5 TS2322: Type 'boolean' is not assignable to type '[string] | [number, number]'."]
    );
}

/// Checks `src` as `/p/<name>` the way tsc's test harness does: semantic diagnostics
/// are reported through syntax errors.
fn check_through(name: &str, options: &str, src: &str) -> Option<Vec<String>> {
    let libs = LibStore::locate()?;
    let mut fs = MemoryFs::new();
    fs.set_cwd("/p");
    fs.add_file(&format!("/p/{name}"), src);
    fs.add_file(
        "/p/tsconfig.json",
        &format!("{{\"compilerOptions\": {{{options}}}, \"files\": [\"{name}\"]}}"),
    );
    let cfg = load_tsconfig(&fs, "/p/tsconfig.json");
    let program = load_program(&fs, &cfg.file_names, &cfg.options, Some(&libs), 1);
    let result = check_program_through_syntax_errors(&program, CheckRun { threads: 1 });
    Some(
        result
            .diagnostics
            .iter()
            .map(|d| {
                let f = &program.files[d.file.0 as usize];
                let lc = f.lines().line_col_utf16(&f.text, d.span.start);
                format!("{}:{} TS{}: {}", lc.line + 1, lc.col + 1, d.code, d.message)
            })
            .collect(),
    )
}

#[test]
fn semantic_checks_run_through_syntax_errors() {
    let cases: [(&str, &str, &[&str]); 9] = [
        // A constructor's or setter's annotation is no return type: no TS2355.
        (
            "a.ts",
            "class C { constructor(): number { } }\n",
            &["1:26 TS1093: Type annotation cannot appear on a constructor declaration."],
        ),
        (
            "a.ts",
            "class C { set x(v: number): number { } }\n",
            &["1:15 TS1095: A 'set' accessor cannot have a return type annotation."],
        ),
        // Semantic errors are reported next to syntax errors.
        (
            "a.ts",
            "let x: number = \"s\";\nlet y = ;\n",
            &[
                "1:5 TS2322: Type 'string' is not assignable to type 'number'.",
                "2:9 TS1109: Expression expected.",
            ],
        ),
        // A `return` outside a function: its expression is not checked.
        (
            "a.ts",
            "return foo;\n}\n",
            &["2:1 TS1128: Declaration or statement expected."],
        ),
        // `for (var in X)`: the empty declaration list is not checked, `X` and the body are
        // (tsc 7.0.2, parserForInStatement2).
        (
            "a.ts",
            "for (var in foo) { let y: number = \"s\"; }\n",
            &[
                "1:9 TS1123: Variable declaration list cannot be empty.",
                "1:13 TS2304: Cannot find name 'foo'.",
                "1:24 TS2322: Type 'string' is not assignable to type 'number'.",
            ],
        ),
        // A missing operand is not TS2357.
        (
            "a.ts",
            "let a = 1;\n++\n++a;\n",
            &["3:1 TS1109: Expression expected."],
        ),
        // A method missing its `{` has an empty body: no TS7010.
        (
            "a.ts",
            "var b = { foo(); };\n",
            &["1:16 TS1005: '{' expected."],
        ),
        // Nor TS2391 after TS1144.
        (
            "a.ts",
            "class C { x()?: number; }\n",
            &[
                "1:14 TS1144: '{' or ';' expected.",
                "1:15 TS1068: Unexpected token. A constructor, method, accessor, or property was expected.",
                "1:17 TS7008: Member 'number' implicitly has an 'any' type.",
            ],
        ),
        // In JavaScript `require("m")` is an import, not a use of a `require` value: its
        // unresolved specifier is reported (tsc 7 for `require("fs")` alone).
        (
            "a.js",
            "const S = require(\"fs\").S;\nnamespace A { const T = require(\"fs\"); }\n",
            &[
                "1:19 TS2591: Cannot find name 'fs'. Do you need to install type definitions for node? Try `npm i --save-dev @types/node` and then add 'node' to the types field in your tsconfig.",
                "2:11 TS8006: 'namespace' declarations can only be used in TypeScript files.",
                "2:33 TS2591: Cannot find name 'fs'. Do you need to install type definitions for node? Try `npm i --save-dev @types/node` and then add 'node' to the types field in your tsconfig.",
            ],
        ),
    ];
    for (name, src, want) in cases {
        let opts = if name.ends_with(".js") {
            "\"allowJs\": true, \"checkJs\": true, \"module\": \"commonjs\", \"target\": \"es2022\""
        } else {
            STRICT
        };
        let Some(out) = check_through(name, opts, src) else {
            return;
        };
        assert_eq!(out, want, "{src:?}");
    }
}

/// A body read as a destructuring pattern (`( { return 'thing'; }`) renames properties to
/// names the parser had to invent: TS2842 prints them as `(Missing)` (tsc 7.0.2,
/// parametersSyntaxErrorNoCrash2).
#[test]
fn unused_renaming_of_missing_name() {
    let Some(out) = check_through(
        "a.ts",
        STRICT,
        "export default function getThing( { return 'thing'; }\n",
    ) else {
        return;
    };
    let renamings: Vec<&String> = out.iter().filter(|l| l.contains("TS2842")).collect();
    assert_eq!(
        renamings,
        [
            "1:43 TS2842: '(Missing)' is an unused renaming of 'return'. Did you intend to use it as a type annotation?",
            "1:51 TS2842: '(Missing)' is an unused renaming of ''thing''. Did you intend to use it as a type annotation?",
        ]
    );
}

/// tsc's checker reports its grammar errors (TS1xxx) only in files without parse errors.
#[test]
fn no_checker_grammar_errors_in_files_with_syntax_errors() {
    let src = "try {} catch (e: number) {}\n";
    let Some(clean) = check_through("a.ts", STRICT, src) else {
        return;
    };
    assert!(clean.iter().any(|d| d.contains("TS1196")), "{clean:?}");
    let broken = check_through("a.ts", STRICT, &format!("{src}let x = ;\n")).unwrap();
    assert!(!broken.iter().any(|d| d.contains("TS1196")), "{broken:?}");
    assert!(broken.iter().any(|d| d.contains("TS1109")), "{broken:?}");
    // So is TS2639 for `<Upper:name>` under the React transforms.
    let jsx = "\"strict\": true, \"target\": \"es2022\", \"jsx\": \"react\"";
    let tag = "declare namespace React { function createElement(...a: any[]): any; }\nconst x = <Up:el />;\n";
    let clean = check_through("a.tsx", jsx, tag).unwrap();
    assert!(clean.iter().any(|d| d.contains("TS2639")), "{clean:?}");
    let broken = check_through("a.tsx", jsx, &format!("{tag}const w = <a :b={{1}} />;\n")).unwrap();
    assert!(!broken.iter().any(|d| d.contains("TS2639")), "{broken:?}");
    assert!(broken.iter().any(|d| d.contains("TS1003")), "{broken:?}");
    // Import attribute errors such as TS2823 are grammar errors too.
    let cjs = "\"module\": \"commonjs\", \"target\": \"es2022\"";
    let src = "import * as f from \"./b\" with { type: \"json\" };\nexport const g = f;\n";
    let clean = check_through("a.ts", cjs, src).unwrap();
    assert!(clean.iter().any(|d| d.contains("TS2823")), "{clean:?}");
    let src = "import * as f from \"./b\" with\nexport const g = f;\n";
    let broken = check_through("a.ts", cjs, src).unwrap();
    assert!(!broken.iter().any(|d| d.contains("TS2823")), "{broken:?}");
    assert!(broken.iter().any(|d| d.contains("TS1005")), "{broken:?}");
}

/// The parser's checker-grammar errors are checker diagnostics: they do not stop semantic
/// checking, and unchecked JavaScript files do not get them.
#[test]
fn parser_grammar_errors_are_checker_diagnostics() {
    let Some(out) = check(STRICT, "class C<> {}\nlet x: number = \"s\";\n") else {
        return;
    };
    assert_eq!(
        out,
        vec![
            "1:8 TS1098: Type parameter list cannot be empty.",
            "2:5 TS2322: Type 'string' is not assignable to type 'number'.",
        ]
    );
    let js = check_through("a.js", "\"allowJs\": true", "class C { set x(v = 1) {} }\n").unwrap();
    assert_eq!(js, Vec::<String>::new());
}

#[test]
fn comment_directives_suppress_parser_grammar_errors() {
    for (src, codes) in [
        ("// @ts-ignore\nconst x: number;\n", vec![]),
        ("// @ts-expect-error\nconst x: number;\n", vec![]),
        ("const x: number;\n", vec![1155]),
        ("// @ts-expect-error\nconst x: number = 1;\n", vec![2578]),
    ] {
        let mut fs = MemoryFs::new();
        fs.set_cwd("/p");
        fs.add_file("/p/a.ts", src);
        fs.add_file(
            "/p/tsconfig.json",
            r#"{"compilerOptions":{"strict":true,"noLib":true,"types":[]},"files":["a.ts"]}"#,
        );
        let cfg = load_tsconfig(&fs, "/p/tsconfig.json");
        let program = load_program(&fs, &cfg.file_names, &cfg.options, None, 1);
        for check in [check_program, check_program_through_syntax_errors] {
            let out = check(&program, CheckRun { threads: 1 });
            // `noLib`: the missing core global types (TS2318) are global, not suppressed.
            assert_eq!(
                out.diagnostics
                    .iter()
                    .map(|d| d.code)
                    .filter(|&c| c != 2318)
                    .collect::<Vec<_>>(),
                codes,
                "{src}: {:?}",
                out.diagnostics,
            );
        }
    }
}

/// `noLib` disables `/// <reference lib>` too (tsc 7.0.2, libReferenceNoLib).
#[test]
fn no_lib_ignores_reference_lib() {
    let Some(libs) = LibStore::locate() else {
        return;
    };
    let mut fs = MemoryFs::new();
    fs.set_cwd("/p");
    fs.add_file("/p/a.ts", "/// <reference lib=\"dom\" />\nexport {};\n");
    for (options, with_libs) in [("\"noLib\": true", false), ("\"lib\": [\"es5\"]", true)] {
        fs.add_file(
            "/p/tsconfig.json",
            &format!("{{\"compilerOptions\": {{{options}}}, \"files\": [\"a.ts\"]}}"),
        );
        let cfg = load_tsconfig(&fs, "/p/tsconfig.json");
        let program = load_program(&fs, &cfg.file_names, &cfg.options, Some(&libs), 1);
        let has_dom = program
            .files
            .iter()
            .any(|f| f.is_lib && f.path.contains("dom"));
        assert_eq!(has_dom, with_libs, "{options}");
    }
}

#[test]
fn optional_chain_this_receiver_preserves_nullable_members() {
    let Some(out) = check(
        STRICT,
        r#"interface I { m(this: I): void }
declare const maybe: { inner: I | undefined } | undefined;
maybe?.inner.m();
declare const nullable: { inner: I | null } | undefined;
nullable?.inner.m();
"#,
    ) else {
        return;
    };
    let this_errors: Vec<_> = out.into_iter().filter(|d| d.contains("TS2684:")).collect();
    assert_eq!(
        this_errors,
        vec![
            "3:1 TS2684: The 'this' context of type 'I | undefined' is not assignable to method's 'this' of type 'I'.\n  Type 'undefined' is not assignable to type 'I'.",
            "5:1 TS2684: The 'this' context of type 'I | null' is not assignable to method's 'this' of type 'I'.\n  Type 'null' is not assignable to type 'I'.",
        ]
    );
}

/// Names the parser could not read print as `(Missing)` in implicit-any errors (tsc's
/// conformance baselines; the tsc 7 CLI stops at the syntax error).
#[test]
fn implicit_any_missing_names() {
    let Some(out) = check_through(
        "a.ts",
        STRICT,
        "function f(`hello`);\nfunction f(x: string);\nfunction f(x: string) {\n    return x;\n}\n",
    ) else {
        return;
    };
    let ours: Vec<&String> = out.iter().filter(|d| d.contains("TS7006")).collect();
    assert_eq!(
        ours,
        ["1:12 TS7006: Parameter '(Missing)' implicitly has an 'any' type."]
    );
}

/// A keyword in a binding pattern (`{ while }`) can't be shorthand: its name is missing, and
/// an untyped pattern reports it as `(Missing)` (destructuringParameterDeclaration6).
#[test]
fn implicit_any_missing_binding_names() {
    let Some(out) = check_through(
        "a.ts",
        STRICT,
        "function a({while}) { }\nfunction c({ class }: { class: number }) { }\n",
    ) else {
        return;
    };
    let ours: Vec<&String> = out.iter().filter(|d| d.contains("TS7031")).collect();
    assert_eq!(
        ours,
        ["1:18 TS7031: Binding element '(Missing)' implicitly has an 'any' type."]
    );
    // A function with no implementation never resolves its parameter types: the renamings
    // are TS2842's alone (parametersSyntaxErrorNoCrash2).
    let Some(out) = check_through(
        "a.ts",
        STRICT,
        "export default function getThing( { return 'thing'; }\n",
    ) else {
        return;
    };
    assert!(out.iter().all(|d| !d.contains("TS7031")), "{out:?}");
}

/// A `,` where a parameter should start is no parameter: no implicit-any error for it.
#[test]
fn implicit_any_skips_comma_parameters() {
    let Some(out) = check_through("a.ts", STRICT, "class X {\n    get x(,) { return 0; }\n}\n")
    else {
        return;
    };
    assert!(out.iter().all(|d| !d.contains("TS7006")), "{out:?}");
    let Some(out) = check_through("a.ts", STRICT, "function t1(...) {}\n") else {
        return;
    };
    assert!(
        out.contains(
            &"1:13 TS7019: Rest parameter '(Missing)' implicitly has an 'any[]' type.".to_owned()
        ),
        "{out:?}"
    );
}

/// A function body the parser recovered from nothing (`function f() => 4;`) is no body:
/// the declaration is TS7010 like an overload signature.
#[test]
fn implicit_any_return_of_recovered_body() {
    let Some(out) = check_through("a.ts", STRICT, "function f() => 4;\nfunction g() {}\n") else {
        return;
    };
    let ours: Vec<&String> = out.iter().filter(|d| d.contains("TS7010")).collect();
    assert_eq!(
        ours,
        [
            "1:10 TS7010: 'f', which lacks return-type annotation, implicitly has an 'any' return type."
        ]
    );
}

/// `return e` in a constructor relates `e` to the instance type, then TS2409 on the statement.
#[test]
fn constructor_return_must_fit_the_instance_type() {
    let Some(out) = check(
        STRICT,
        "class F {\n    foo: string = \"\";\n    constructor() {\n        return { foo: 1 };\n    }\n}\nclass B<T> {\n    constructor(x: T) {\n        return x;\n    }\n}\nclass C {\n    constructor() {\n        return this;\n    }\n}\n",
    ) else {
        return;
    };
    assert_eq!(
        out,
        vec![
            "4:9 TS2409: Return type of constructor signature must be assignable to the instance type of the class.",
            "4:18 TS2322: Type 'number' is not assignable to type 'string'.",
            "9:9 TS2322: Type 'T' is not assignable to type 'B<T>'.",
            "9:9 TS2409: Return type of constructor signature must be assignable to the instance type of the class.",
        ]
    );
}

/// A class field repeated in one body, or after a getter/setter pair, must keep the earlier
/// member's type (TS2717 beside the binder's TS2300); a method in between starts over.
#[test]
fn repeated_class_fields_keep_the_first_type() {
    let Some(out) = check(
        STRICT,
        "class D {\n    c: number = 1;\n    c: string = \"\";\n    static s = 1;\n    static s: string;\n    get x() { return 10; }\n    set x(v: number) {}\n    x: string = \"\";\n    m(): void {}\n    m: number = 1;\n}\n",
    ) else {
        return;
    };
    let ours: Vec<&String> = out.iter().filter(|d| d.contains("TS2717")).collect();
    assert_eq!(
        ours,
        [
            "3:5 TS2717: Subsequent property declarations must have the same type.  Property 'c' must be of type 'number', but here has type 'string'.",
            "5:12 TS2717: Subsequent property declarations must have the same type.  Property 's' must be of type 'number', but here has type 'string'.",
            "8:5 TS2717: Subsequent property declarations must have the same type.  Property 'x' must be of type 'number', but here has type 'string'.",
        ]
    );
}

/// [`check`] for a `.tsx` file.
fn check_tsx(options: &str, src: &str) -> Option<Vec<String>> {
    let libs = LibStore::locate()?;
    let mut fs = MemoryFs::new();
    fs.set_cwd("/p");
    fs.add_file("/p/a.tsx", src);
    fs.add_file(
        "/p/tsconfig.json",
        &format!("{{\"compilerOptions\": {{{options}}}, \"files\": [\"a.tsx\"]}}"),
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
                format!("{}:{} TS{}: {}", lc.line + 1, lc.col + 1, d.code, d.message)
            })
            .collect(),
    )
}

/// Attributes failing one member of `IntrinsicAttributes & P` are reported against that
/// member alone; hyphenated names (also from a spread object literal) are never excess and
/// don't trip weak type detection.
#[test]
fn jsx_attributes_fail_against_the_failing_member() {
    let Some(out) = check_tsx(
        "\"strict\": true, \"target\": \"es2022\", \"jsx\": \"preserve\"",
        "declare namespace JSX {\n    interface Element {}\n    interface IntrinsicAttributes { key?: string }\n    interface IntrinsicClassAttributes<T> { ref?: T }\n}\ndeclare function F(p: { x: number }): JSX.Element;\ndeclare function W(p: { y?: number }): JSX.Element;\nconst a = <F data-extra=\"\" />;\nconst b = <W {...{ \"data-info\": \"hi\" }} />;\n",
    ) else {
        return;
    };
    assert_eq!(
        out,
        vec![
            "8:12 TS2741: Property 'x' is missing in type '{ \"data-extra\": string; }' but required in type '{ x: number; }'."
        ]
    );
}

/// An array literal's failing literal element is reported widened unless its target takes
/// literals (`true` stays as it is); a binding pattern in a bodiless signature reports each
/// binding as implicitly `any`.
#[test]
fn array_elements_widen_and_signature_patterns_are_implicit_any() {
    let Some(out) = check(
        STRICT,
        "let a: null[] = [1, true, \"s\"];\nlet b: (1 | \"s\")[] = [1, \"s\", 2];\ninterface F {\n    m(x: number, [p, q]: any, { r, s: { t } });\n}\n",
    ) else {
        return;
    };
    assert_eq!(
        out,
        vec![
            "1:18 TS2322: Type 'number' is not assignable to type 'null'.",
            "1:21 TS2322: Type 'true' is not assignable to type 'null'.",
            "1:27 TS2322: Type 'string' is not assignable to type 'null'.",
            "2:31 TS2322: Type '2' is not assignable to type '\"s\" | 1'.",
            "4:5 TS7010: 'm', which lacks return-type annotation, implicitly has an 'any' return type.",
            "4:33 TS7031: Binding element 'r' implicitly has an 'any' type.",
            "4:41 TS7031: Binding element 't' implicitly has an 'any' type.",
        ]
    );
}

/// An object literal typed by a destructuring pattern (assignment patterns, nested pattern
/// defaults, declarations) reports every property the pattern doesn't name.
#[test]
fn pattern_typed_literals_report_every_excess_property() {
    let Some(out) = check(
        STRICT,
        "let x = 0, y = 0;\n({} = { x: 0, y: 0 });\n({ x } = { x: 0, y: 0, z: 1 });\nvar { f: { g } = { h: 1 } }: any = undefined;\nlet o: any, k: any;\nlet { a } = { a: 1, ...o, [k]: 1 };\n",
    ) else {
        return;
    };
    assert_eq!(
        out,
        vec![
            "2:9 TS2353: Object literal may only specify known properties, and 'x' does not exist in type '{}'.",
            "2:15 TS2353: Object literal may only specify known properties, and 'y' does not exist in type '{}'.",
            "3:18 TS2353: Object literal may only specify known properties, and 'y' does not exist in type '{ x: number; }'.",
            "3:24 TS2353: Object literal may only specify known properties, and 'z' does not exist in type '{ x: number; }'.",
            "4:20 TS2353: Object literal may only specify known properties, and 'h' does not exist in type '{ g: any; }'.",
            "6:27 TS2353: Object literal may only specify known properties, and '[k]' does not exist in type '{ a: any; }'.",
        ]
    );
}

/// Ambient classes check their `implements` clauses too.
#[test]
fn ambient_classes_check_implements() {
    let Some(out) = check(
        STRICT,
        "interface IBuffer {\n    [index: number]: number;\n}\ndeclare class Buffer implements IBuffer {\n}\n",
    ) else {
        return;
    };
    assert_eq!(
        out,
        vec![
            "4:15 TS2420: Class 'Buffer' incorrectly implements interface 'IBuffer'.\n  Index signature for type 'number' is missing in type 'Buffer'."
        ]
    );
}

/// A literal kept by its contextual type (a type parameter with a primitive constraint)
/// is no longer fresh: a `let` reading it back keeps the literal.
#[test]
fn contextually_kept_literals_are_not_fresh() {
    let Some(out) = check(
        STRICT,
        "const R = Object.freeze({ c: '10', f: 'latest' });\nlet p = R.c;\np = R.f;\n",
    ) else {
        return;
    };
    assert_eq!(
        out,
        vec!["3:1 TS2322: Type '\"latest\"' is not assignable to type '\"10\"'."]
    );
}

/// `==` keeps the members that coerce to the compared value.
#[test]
fn loose_equality_keeps_coercible_members() {
    let Some(out) = check(
        STRICT,
        "function test(level: number | string): number {\n    if (level == +level) {\n        const q: number = level;\n    }\n    if (level === +level) {\n        const r: number = level;\n    }\n    return 0;\n}\n",
    ) else {
        return;
    };
    assert_eq!(
        out,
        vec![
            "3:15 TS2322: Type 'string | number' is not assignable to type 'number'.\n  Type 'string' is not assignable to type 'number'."
        ]
    );
}

/// An empty binding pattern under `strictNullChecks` doesn't relate its initializer to the
/// annotation; it only asks for a value that isn't nullish.
#[test]
fn empty_annotated_patterns_only_check_nullishness() {
    let Some(out) = check(STRICT, "const {}: undefined = 1;\nconst {}: string = 1;\n") else {
        return;
    };
    assert_eq!(out, vec!["1:7 TS2532: Object is possibly 'undefined'."]);
}

/// A weak type failure suggests the call when `new`-ing the source would fit (TS2560).
#[test]
fn weak_type_suggests_construct_call() {
    let src = "interface S { timeout?: number }\n\
               interface C { new (s: string): { timeout: 1000 } }\n\
               declare const c: C;\n\
               const s: S = c;\n";
    let Some(out) = check(STRICT, src) else {
        return;
    };
    assert_eq!(
        out,
        vec![
            "4:14 TS2560: Value of type 'C' has no properties in common with type 'S'. Did you mean to call it?"
        ]
    );
}

/// The `TS2318` messages for `src` checked as `/p/a.ts` with `options`.
fn missing_globals(options: &str, src: &str) -> Option<Vec<String>> {
    let libs = LibStore::locate()?;
    let mut fs = MemoryFs::new();
    fs.set_cwd("/p");
    fs.add_file("/p/a.ts", src);
    fs.add_file(
        "/p/tsconfig.json",
        &format!("{{\"compilerOptions\": {{{options}}}, \"files\": [\"a.ts\"]}}"),
    );
    let cfg = load_tsconfig(&fs, "/p/tsconfig.json");
    let program = load_program(&fs, &cfg.file_names, &cfg.options, Some(&libs), 1);
    let result = check_program(&program, CheckRun { threads: 1 });
    let mut out: Vec<String> = result
        .diagnostics
        .iter()
        .filter(|d| d.code == 2318)
        .map(|d| d.message.to_string())
        .collect();
    out.sort();
    Some(out)
}

/// Global types tsc looks up only when a construct needs them report `TS2318` at that point
/// (tsc 7.0.2: sourceMapValidationDestructuringForArrayBindingPattern and the generator and
/// `strictBindCallApply` global type setup).
#[test]
fn lazily_looked_up_global_types_report_when_used() {
    let es5 = "\"target\": \"es2015\", \"lib\": [\"es5\"]";
    let Some(none) = missing_globals(es5, "let [...a] = x;\ndeclare const x: number[];\n") else {
        return;
    };
    // A rest-only pattern initialised by an identifier never asks for `Iterable`.
    assert_eq!(none, Vec::<String>::new());
    let cannot = |n: &str| format!("Cannot find global type '{n}'.");
    let rest = missing_globals(es5, "let [...a] = [1, 2];\n").unwrap();
    assert_eq!(rest, vec![cannot("Iterable")]);
    let empty = missing_globals(es5, "let [] = [1];\n").unwrap();
    assert_eq!(empty, vec![cannot("Iterable")]);
    let named = missing_globals(es5, "let [a] = [1];\n").unwrap();
    assert_eq!(named, Vec::<String>::new());
    let generator = missing_globals(es5, "function* g() { yield 1; }\n").unwrap();
    assert_eq!(generator, vec![cannot("IterableIterator")]);
    let annotated = missing_globals(es5, "function* g(): any { yield 1; }\n").unwrap();
    assert_eq!(annotated, Vec::<String>::new());
    // `CallableFunction` and `NewableFunction` are only needed under `strictBindCallApply`.
    let loose = missing_globals("\"noLib\": true, \"strictBindCallApply\": false", "").unwrap();
    assert!(!loose.contains(&cannot("CallableFunction")), "{loose:?}");
    let strict = missing_globals("\"noLib\": true, \"strictBindCallApply\": true", "").unwrap();
    assert!(strict.contains(&cannot("CallableFunction")), "{strict:?}");
    assert!(strict.contains(&cannot("NewableFunction")), "{strict:?}");
}

/// Below ES2022 the emit reserves `Reflect` in scopes holding a `super` access from a static
/// initializer (TS2818) and `WeakMap`/`WeakSet` in scopes holding a class with private names
/// (TS18027). tsc 7.0.2 output.
#[test]
fn downlevel_reserved_names() {
    let Some(out) = check(
        "\"strict\": true, \"target\": \"es2015\", \"module\": \"esnext\", \"outDir\": \"out\"",
        r#"export {};
declare class B { static w(): number; }
var Reflect = null;
class C extends B {
    static x = () => super.w();
}
function f() {
    let WeakMap;
    class D { #p = 1; }
}
function g() {
    let Reflect;
    class E extends B { m() { return super.w(); } }
}
let WeakSet = 1;
class F { #q = 1; }
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        vec![
            "3:5 TS2818: Duplicate identifier 'Reflect'. Compiler reserves name 'Reflect' when emitting 'super' references in static initializers.",
            "8:9 TS18027: Compiler reserves name 'WeakMap' when emitting private identifier downlevel.",
            "13:44 TS2576: Property 'w' does not exist on type 'B'. Did you mean to access the static member 'B.w' instead?",
            "15:5 TS18027: Compiler reserves name 'WeakSet' when emitting private identifier downlevel.",
        ]
    );
}
