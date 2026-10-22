//! Calls and property access: missing members, overload and arity reporting. Expected output
//! taken from tsc 7.0.2 on the same input (most cases reduced from tsc's conformance tests).
//! Skipped when the lib files are not available (`cargo xtask upstream sync --minimal`).

use ts_r_checker::{CheckRun, LibStore, check_program, load_program};
use ts_r_config::{MemoryFs, load_tsconfig};

ts_r_testing::cap_memory!();

/// Checks `src` as `/p/a.ts` with `options` and returns `line:col TScode: message` lines.
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
fn static_member_through_an_instance() {
    let Some(out) = check(
        STRICT,
        r#"class List<T> {
    m() { this.foo(); }
    static foo() {}
}
namespace List { export var y = 2; }
declare const l: List<number>;
l.foo;
l.y;
l.prototype;
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        vec![
            "2:16 TS2576: Property 'foo' does not exist on type 'List<T>'. Did you mean to access the static member 'List<T>.foo' instead?",
            "7:3 TS2576: Property 'foo' does not exist on type 'List<number>'. Did you mean to access the static member 'List<number>.foo' instead?",
            "8:3 TS2339: Property 'y' does not exist on type 'List<number>'.",
            "9:3 TS2339: Property 'prototype' does not exist on type 'List<number>'.",
        ]
    );
}

#[test]
fn type_argument_count_is_checked_before_argument_count() {
    let Some(out) = check(
        "\"strict\": false, \"target\": \"es2015\"",
        r#"declare function f1<A = any>(): void;
declare function f1<A, B, C, D = any>(): void;
f1<number, number>();
f1<number, number, number, number, number>();
declare function foo<T>(a: T, b: T);
declare function foo(a: {});
foo<string>("hello");
declare function g<T>(): void;
g<>();
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        vec![
            "3:4 TS2743: No overload expects 2 type arguments, but overloads do exist that expect either 1 or 3 type arguments.",
            "4:4 TS2558: Expected 4 type arguments, but got 5.",
            "7:1 TS2554: Expected 2 arguments, but got 1.",
            "9:2 TS1099: Type argument list cannot be empty.",
        ]
    );
}

#[test]
fn new_of_a_function_resolves_the_call() {
    let Some(out) = check(
        "\"strict\": false, \"target\": \"es2015\"",
        r#"declare function cb<T1, T2>(flags?: string): void;
new cb<number, string, boolean>('s');
declare function num(): number;
new num();
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        vec![
            "2:8 TS2558: Expected 2 type arguments, but got 3.",
            "4:1 TS2350: Only a void function can be called with the 'new' keyword.",
        ]
    );
}

#[test]
fn argument_count_errors_over_overloads() {
    let Some(out) = check(
        "\"strict\": false, \"target\": \"es2015\"",
        r#"declare function f2();
declare function f2(a: number, b: number);
declare function f2(a: number, b: number, c: number, d: number);
f2(1);
f2(1, 2, 3, 4, 5);
declare function f1(a: number, b: number): void;
f1(...[1], 2);
f1(...([1, 2]), 3);
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        vec![
            "4:1 TS2575: No overload expects 1 arguments, but overloads do exist that expect either 0 or 2 arguments.",
            "5:16 TS2554: Expected 0-4 arguments, but got 5.",
            "8:17 TS2554: Expected 2 arguments, but got 3.",
        ]
    );
}

#[test]
fn index_signature_method_hint() {
    let Some(out) = check(
        "\"noImplicitAny\": true, \"target\": \"es2015\"",
        r#"var c = { get: (key: string) => 'x' };
c['hello'];
({ get: (key: string) => 'x', set: (key: string, value: string) => {} })['hello'] += 1;
const o = { a: 0 };
declare const k: "a" | "b" | "c";
o[k];
enum E { a, b }
declare let e: E;
o[e];
interface M { get(key: string): string }
declare const m: { prop: M };
m.prop['a'];
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        vec![
            "2:1 TS7052: Element implicitly has an 'any' type because type '{ get: (key: string) => string; }' has no index signature. Did you mean to call 'c.get'?",
            "3:1 TS7052: Element implicitly has an 'any' type because type '{ get: (key: string) => string; set: (key: string, value: string) => void; }' has no index signature. Did you mean to call 'set'?",
            "6:1 TS7053: Element implicitly has an 'any' type because expression of type '\"a\" | \"b\" | \"c\"' can't be used to index type '{ a: number; }'.\n  Property 'b' does not exist on type '{ a: number; }'.",
            "9:1 TS7053: Element implicitly has an 'any' type because expression of type 'E' can't be used to index type '{ a: number; }'.\n  Property '[E.a]' does not exist on type '{ a: number; }'.",
            "12:1 TS7052: Element implicitly has an 'any' type because type 'M' has no index signature. Did you mean to call 'm.prop.get'?",
        ]
    );
}

#[test]
fn extends_any_constrains_like_unknown() {
    let Some(out) = check(
        "\"strict\": false, \"target\": \"es2015\"",
        r#"function f<T extends any>(x: T) {
    x.children;
    x();
    new x();
    x.toString;
}
function g<T extends any>(): T { return null as never; }
function h<T extends any>(t: T) { t = null; t = 1; }
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        vec![
            "2:7 TS2339: Property 'children' does not exist on type 'T'.",
            "3:5 TS2349: This expression is not callable.\n  Type '{}' has no call signatures.",
            "4:9 TS2351: This expression is not constructable.\n  Type '{}' has no construct signatures.",
            "8:45 TS2322: Type 'number' is not assignable to type 'T'.\n  'number' is assignable to the constraint of type 'T', but 'T' could be instantiated with a different subtype of constraint 'unknown'.",
        ]
    );
}

#[test]
fn destructured_catch_variables() {
    let Some(out) = check(
        STRICT,
        r#"try {} catch ({ x }) { x }
try {} catch ([ x ]) { x }
try {} catch ({ x }: any) { x }
try {} catch (e) { e(); }
try {} catch (x) { let x: string; }
try {} catch (e: number) { e.toLowerCase(); }
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        vec![
            "1:17 TS2339: Property 'x' does not exist on type 'unknown'.",
            "2:15 TS2488: Type 'unknown' must have a '[Symbol.iterator]()' method that returns an iterator.",
            "4:20 TS18046: 'e' is of type 'unknown'.",
            "5:24 TS2492: Cannot redeclare identifier 'x' in catch clause.",
            "6:18 TS1196: Catch clause variable type annotation must be 'any' or 'unknown' if specified.",
        ]
    );
}

#[test]
fn iterator_result_without_value_and_non_method_return() {
    let Some(out) = check(
        STRICT,
        r#"class A {
    next() { return ""; }
    [Symbol.iterator]() { return this; }
}
for (const v of new A) { v; }
class B {
    next() { return { done: false, value: "" }; }
    return = 0;
    [Symbol.iterator]() { return this; }
}
for (const v of new B) { v; }
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        vec![
            "5:17 TS2490: The type returned by the 'next()' method of an iterator must have a 'value' property.",
            "11:17 TS2767: The 'return' property of an iterator must be a method.",
        ]
    );
}

#[test]
fn rest_parameter_pattern_counts_its_elements() {
    let Some(out) = check(
        "\"strict\": false, \"target\": \"es2015\"",
        r#"function f(...[a, b]) { }
f(1);
function g(...[a, b = 1]) { }
g();
function h(...[a, ...b]) { }
h();
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        vec![
            "2:1 TS2554: Expected 2 arguments, but got 1.",
            "4:1 TS2554: Expected 1-2 arguments, but got 0.",
            "6:1 TS2555: Expected at least 1 arguments, but got 0.",
        ]
    );
}

#[test]
fn super_in_a_class_extending_null() {
    let Some(out) = check(
        STRICT,
        r#"class C1 extends null {
    static m() { super.oops; }
    m() { super.oops; }
}
class C2 extends null {
    constructor() {
        super();
        return Object.create(null);
    }
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        vec![
            "2:18 TS2531: Object is possibly 'null'.",
            "3:11 TS2531: Object is possibly 'null'.",
            "7:9 TS17005: A constructor cannot contain a 'super' call when its class extends 'null'.",
        ]
    );
}

#[test]
fn non_array_rest_parameter_takes_a_tuple() {
    let Some(out) = check(
        STRICT,
        r#"declare function n(...args: never): void;
n();
n(1);
declare function u(...args: [string] | [number, boolean]): void;
u(10);
u("");
u(1, true);
class E<ET> {
    off<K extends keyof ET>(...args: [K, number] | [unknown, string]): void {}
}
function once<ET>(e: E<ET>, name: keyof ET) {
    e.off(name, 0);
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        vec![
            "2:1 TS2345: Argument of type '[]' is not assignable to parameter of type 'never'.",
            "3:3 TS2345: Argument of type '[number]' is not assignable to parameter of type 'never'.",
            "5:3 TS2345: Argument of type '[10]' is not assignable to parameter of type '[string] | [number, boolean]'.\n  Type '[10]' is not assignable to type '[string]'.\n    Type 'number' is not assignable to type 'string'.",
        ]
    );
}
