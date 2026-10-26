//! Declaration checks: unused declarations, classes and members, overloads, duplicates and
//! decorators. Expected output taken from tsc 7.0.2 on the same input.
//! Skipped when the lib files are not available (`cargo xtask upstream sync --minimal`).

use ts_r_checker::{
    CheckRun, LibStore, check_program, check_program_through_syntax_errors, load_program,
};
use ts_r_config::{MemoryFs, load_tsconfig};

ts_r_testing::cap_memory!();

/// Checks `files` (`(name, source)` under `/p`) with `options` and returns
/// `file:line:col TScode: message` lines.
fn check_files(options: &str, files: &[(&str, &str)]) -> Option<Vec<String>> {
    let libs = LibStore::locate()?;
    let mut fs = MemoryFs::new();
    fs.set_cwd("/p");
    let mut names = Vec::new();
    for (n, src) in files {
        fs.add_file(&format!("/p/{n}"), src);
        names.push(format!("\"{n}\""));
    }
    fs.add_file(
        "/p/tsconfig.json",
        &format!(
            "{{\"compilerOptions\": {{{options}}}, \"files\": [{}]}}",
            names.join(",")
        ),
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
                let name = f.path.rsplit('/').next().unwrap_or("");
                format!(
                    "{name}:{}:{} TS{}: {}",
                    lc.line + 1,
                    lc.col + 1,
                    d.code,
                    d.message
                )
            })
            .collect(),
    )
}

fn check(options: &str, src: &str) -> Option<Vec<String>> {
    check_files(options, &[("a.ts", src)])
}

const UNUSED: &str = "\"strict\": true, \"target\": \"es2022\", \"noUnusedLocals\": true, \"noUnusedParameters\": true";

#[test]
fn unused_locals_and_parameters() {
    let Some(out) = check_files(
        UNUSED,
        &[
            (
                "a.ts",
                r#"function f(a: number, b: string) { return a; }
function rec() { rec(); }
let w = 1; w = 2;
let rw = 1; rw++;
import { x } from "./b";
type T = number;
interface I { }
class C { private p = 1; private q = 2; m() { return this.q; } private static s() {} #h = 1; }
const { d, e } = { d: 1, e: 2 }; d;
for (const k of [1]) {}
export {};
"#,
            ),
            ("b.ts", "export const x = 1;\n"),
        ],
    ) else {
        return;
    };
    assert_eq!(
        out,
        vec![
            "a.ts:1:10 TS6133: 'f' is declared but its value is never read.",
            "a.ts:1:23 TS6133: 'b' is declared but its value is never read.",
            "a.ts:2:10 TS6133: 'rec' is declared but its value is never read.",
            "a.ts:3:5 TS6133: 'w' is declared but its value is never read.",
            "a.ts:5:10 TS6133: 'x' is declared but its value is never read.",
            "a.ts:6:6 TS6196: 'T' is declared but never used.",
            "a.ts:7:11 TS6196: 'I' is declared but never used.",
            "a.ts:8:7 TS6196: 'C' is declared but never used.",
            "a.ts:8:19 TS6133: 'p' is declared but its value is never read.",
            "a.ts:8:79 TS6133: 's' is declared but its value is never read.",
            "a.ts:8:86 TS6133: '#h' is declared but its value is never read.",
            "a.ts:9:12 TS6133: 'e' is declared but its value is never read.",
            "a.ts:10:12 TS6133: 'k' is declared but its value is never read.",
        ]
    );
}

#[test]
fn unused_groups_and_exemptions() {
    let Some(out) = check_files(
        UNUSED,
        &[
            (
                "a.ts",
                r#"function f1<T, U>(_a: number, b: string, ...rest: number[]) { }
const _u = 1;
const { a1, b1 } = { a1: 1, b1: 2 };
const [_c2, d2] = [1, 2];
let v1, v2;
import { x, y } from "./b";
import * as ns from "./b";
import * as _ns from "./b";
class K { constructor(private pp: number, public qq: number) { } private get g() { return 1; } private set g(v) { } }
const { o1: { i1, i2 }, o2 } = { o1: { i1: 1, i2: 2 }, o2: 1 };
const { a, ...rest } = { a: 1, b: 2 }; rest;
let [[n1, n2]] = [[1, 2]];
type L<T> = T extends ArrayLike<infer U> ? number : never;
export {};
"#,
            ),
            ("b.ts", "export const x = 1;\nexport const y = 1;\n"),
        ],
    ) else {
        return;
    };
    assert_eq!(
        out,
        vec![
            "a.ts:1:10 TS6133: 'f1' is declared but its value is never read.",
            "a.ts:1:12 TS6205: All type parameters are unused.",
            "a.ts:1:31 TS6133: 'b' is declared but its value is never read.",
            "a.ts:1:45 TS6133: 'rest' is declared but its value is never read.",
            "a.ts:2:7 TS6133: '_u' is declared but its value is never read.",
            "a.ts:3:7 TS6198: All destructured elements are unused.",
            "a.ts:4:13 TS6133: 'd2' is declared but its value is never read.",
            "a.ts:5:1 TS6199: All variables are unused.",
            "a.ts:6:1 TS6192: All imports in import declaration are unused.",
            "a.ts:7:13 TS6133: 'ns' is declared but its value is never read.",
            "a.ts:9:7 TS6196: 'K' is declared but never used.",
            "a.ts:9:31 TS6138: Property 'pp' is declared but its value is never read.",
            "a.ts:9:78 TS6133: 'g' is declared but its value is never read.",
            "a.ts:9:110 TS6133: 'v' is declared but its value is never read.",
            "a.ts:10:7 TS6198: All destructured elements are unused.",
            "a.ts:12:6 TS6198: All destructured elements are unused.",
            "a.ts:13:6 TS6196: 'L' is declared but never used.",
            "a.ts:13:39 TS6196: 'U' is declared but never used.",
        ]
    );
}

#[test]
fn unused_type_parameter_merged_with_parameter() {
    // `T` names both a type parameter and a parameter (one symbol): a read as a type only
    // uses the type parameter, any other read only the parameter.
    let Some(out) = check(
        UNUSED,
        "export function a<T>(T: number) { return T; }\nexport function b<T>(T: T) {}\nexport function c<T>(T: T) { return T; }\n",
    ) else {
        return;
    };
    assert_eq!(
        out,
        vec![
            "a.ts:1:19 TS6196: 'T' is declared but never used.",
            "a.ts:2:22 TS6133: 'T' is declared but its value is never read.",
        ]
    );
}

#[test]
fn unused_import_of_unresolved_module() {
    // An unused import is reported even when its module does not resolve.
    let Some(out) = check(
        UNUSED,
        "import Foo from \"nope\";\nimport Bar from \"nope2\";\nexport const x: typeof Foo = 1;\n",
    ) else {
        return;
    };
    assert!(
        out.contains(
            &"a.ts:2:8 TS6133: 'Bar' is declared but its value is never read.".to_string()
        ),
        "{out:?}"
    );
    assert!(!out.iter().any(|d| d.contains("'Foo'")), "{out:?}");
}

#[test]
fn private_members_read_through_literal_element_access() {
    let Some(out) = check(
        UNUSED,
        r#"export class C { private x = 1; private y = 2; }
new C()["x"];
class D { private z = 1; m(o: any) { return o["y"]; } }
new D();
export class G<T> { private x?: T; private get g() { return 1; } private w = 1; }
const v = new G<number>();
v["x"];
v["g"];
v["w"] = 2;
export class H { private q = 1; }
declare const o: { q: number };
o["q"];
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        vec![
            "a.ts:1:41 TS6133: 'y' is declared but its value is never read.",
            "a.ts:3:19 TS6133: 'z' is declared but its value is never read.",
            "a.ts:5:74 TS6133: 'w' is declared but its value is never read.",
            "a.ts:10:26 TS6133: 'q' is declared but its value is never read.",
        ]
    );
}

#[test]
fn private_members_read_through_typed_element_keys() {
    let Some(out) = check(
        UNUSED,
        r#"export class C {
  private a = 1; private b = 2; private c = 3;
  m(k: "a" | "b") { return this[k]; }
}
export class D { private e = 1; m(k: any) { return this[k]; } }
const x: unique symbol = Symbol();
const y: unique symbol = Symbol();
export class E {
  private [x]!: number; private [y]!: number;
  m() { this[x] = 0; return this[y]; }
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        vec![
            "a.ts:2:41 TS6133: 'c' is declared but its value is never read.",
            "a.ts:5:52 TS7053: Element implicitly has an 'any' type because expression of type 'any' can't be used to index type 'D'.",
            "a.ts:9:11 TS6133: '[x]' is declared but its value is never read.",
        ]
    );
}

#[test]
fn setter_parameter_typed_from_getter() {
    let Some(out) = check(
        "\"strict\": true, \"target\": \"es2022\"",
        r#"class A { get g() { return 1; } set g(v) { } }
class B { set s(v) { } }
export {};
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        vec![
            "a.ts:2:15 TS7032: Property 's' implicitly has type 'any', because its set accessor lacks a parameter type annotation.",
            "a.ts:2:17 TS7006: Parameter 'v' implicitly has an 'any' type.",
        ]
    );
}

/// isolatedDeclarationErrorsClasses: a setter with a computed name that is no literal is
/// an accessor of its own, named by its source text; a getter elsewhere does not type it.
#[test]
fn setter_with_unbindable_computed_name() {
    let Some(out) = check(
        "\"strict\": true, \"target\": \"es2022\"",
        r#"let k: string = "k";
class A { get [k]() { return 0; } set [k](v) { } }
export {};
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        vec![
            "a.ts:2:39 TS7032: Property '[k]' implicitly has type 'any', because its set accessor lacks a parameter type annotation.",
            "a.ts:2:43 TS7006: Parameter 'v' implicitly has an 'any' type.",
        ]
    );
}

/// `a.ts(L,C): error TSxxxx: message` lines, as `tsc --pretty false` prints them.
fn tsc_format(out: &[String]) -> String {
    out.iter()
        .map(|l| {
            let (pos, rest) = l.split_once(' ').unwrap_or((l, ""));
            let mut parts = pos.splitn(3, ':');
            let (f, line, col) = (
                parts.next().unwrap_or(""),
                parts.next().unwrap_or(""),
                parts.next().unwrap_or(""),
            );
            format!("{f}({line},{col}): error {rest}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn heritage_compatibility() {
    let Some(out) = check(
        "\"strict\": true, \"target\": \"es2022\"",
        r#"class A { x: number = 1; m(a: number): string { return ""; } static s = 1; }
class B extends A { x: string = ""; }
class B2 extends A { m(a: string): string { return ""; } }
interface I { y: number; f(): void; }
class C implements I { y = ""; f() {} }
class D implements I { y = 1; }
interface J extends I { y: string; }
class P { private p = 1; protected q = 1; }
class Q extends P { private p = 2; }
class R extends P { p = 2; }
class S implements P { p = 1; q = 1; }
interface I2 { a: number }
interface I3 { a: string }
interface I4 extends I2, I3 { }
class E extends A { protected x = 2; }
class F extends A { static s = ""; }
class G extends P { q = 2; }
class H { #h = 1; }
class K extends H { #h = ""; }
type U = { a: number } | { b: string };
class L implements U { a = 1; }
export {};
"#,
    ) else {
        return;
    };
    assert_eq!(
        tsc_format(&out),
        r#"a.ts(2,21): error TS2416: Property 'x' in type 'B' is not assignable to the same property in base type 'A'.
  Type 'string' is not assignable to type 'number'.
a.ts(3,22): error TS2416: Property 'm' in type 'B2' is not assignable to the same property in base type 'A'.
  Type '(a: string) => string' is not assignable to type '(a: number) => string'.
    Types of parameters 'a' and 'a' are incompatible.
      Type 'number' is not assignable to type 'string'.
a.ts(5,24): error TS2416: Property 'y' in type 'C' is not assignable to the same property in base type 'I'.
  Type 'string' is not assignable to type 'number'.
a.ts(6,7): error TS2420: Class 'D' incorrectly implements interface 'I'.
  Property 'f' is missing in type 'D' but required in type 'I'.
a.ts(7,11): error TS2430: Interface 'J' incorrectly extends interface 'I'.
  Types of property 'y' are incompatible.
    Type 'string' is not assignable to type 'number'.
a.ts(9,7): error TS2415: Class 'Q' incorrectly extends base class 'P'.
  Types have separate declarations of a private property 'p'.
a.ts(10,7): error TS2415: Class 'R' incorrectly extends base class 'P'.
  Property 'p' is private in type 'P' but not in type 'R'.
a.ts(11,7): error TS2720: Class 'S' incorrectly implements class 'P'. Did you mean to extend 'P' and inherit its members as a subclass?
  Property 'p' is private in type 'P' but not in type 'S'.
a.ts(14,11): error TS2320: Interface 'I4' cannot simultaneously extend types 'I2' and 'I3'.
  Named property 'a' of types 'I2' and 'I3' are not identical.
a.ts(15,7): error TS2415: Class 'E' incorrectly extends base class 'A'.
  Property 'x' is protected in type 'E' but public in type 'A'.
a.ts(16,7): error TS2417: Class static side 'typeof F' incorrectly extends base class static side 'typeof A'.
  Types of property 's' are incompatible.
    Type 'string' is not assignable to type 'number'.
a.ts(21,20): error TS2422: A class can only implement an object type or intersection of object types with statically known members."#
    );
}

#[test]
fn index_signature_constraints() {
    let Some(out) = check(
        "\"strict\": true, \"target\": \"es2022\"",
        r#"interface A { [k: string]: number; x: string; y: number; 1: boolean; }
interface B { [k: number]: string; 0: number; z: boolean; "2": number; }
class C { [k: string]: number; p: string = ""; static q: string = ""; m(): void {} }
interface D { [k: string]: number; }
interface E extends D { e: string; }
type T = { [k: string]: number; t: string; };
interface F { [k: string]: string; [n: number]: number; }
interface G { [k: string]: number; readonly r?: number; o?: number; }
class K { [k: string]: number; constructor(public pp: string) {} }
interface L extends D { }
interface L { l: string; }
interface M { [k: string]: number; f(): void; f(x: number): void; }
export {};
"#,
    ) else {
        return;
    };
    assert_eq!(
        tsc_format(&out),
        r#"a.ts(1,36): error TS2411: Property 'x' of type 'string' is not assignable to 'string' index type 'number'.
a.ts(1,58): error TS2411: Property '1' of type 'boolean' is not assignable to 'string' index type 'number'.
a.ts(2,36): error TS2411: Property '0' of type 'number' is not assignable to 'number' index type 'string'.
a.ts(2,59): error TS2411: Property '"2"' of type 'number' is not assignable to 'number' index type 'string'.
a.ts(3,32): error TS2411: Property 'p' of type 'string' is not assignable to 'string' index type 'number'.
a.ts(3,71): error TS2411: Property 'm' of type '() => void' is not assignable to 'string' index type 'number'.
a.ts(5,25): error TS2411: Property 'e' of type 'string' is not assignable to 'string' index type 'number'.
a.ts(6,33): error TS2411: Property 't' of type 'string' is not assignable to 'string' index type 'number'.
a.ts(7,36): error TS2413: 'number' index type 'number' is not assignable to 'string' index type 'string'.
a.ts(8,45): error TS2411: Property 'r' of type 'number | undefined' is not assignable to 'string' index type 'number'.
a.ts(8,57): error TS2411: Property 'o' of type 'number | undefined' is not assignable to 'string' index type 'number'.
a.ts(9,44): error TS2411: Property 'pp' of type 'string' is not assignable to 'string' index type 'number'.
a.ts(11,15): error TS2411: Property 'l' of type 'string' is not assignable to 'string' index type 'number'.
a.ts(12,36): error TS2411: Property 'f' of type '{ (): void; (x: number): void; }' is not assignable to 'string' index type 'number'."#
    );
}

#[test]
fn redeclarations_agree_on_types() {
    let Some(out) = check(
        "\"strict\": true, \"target\": \"es2022\"",
        r#"var a: number;
var a: string;
var b = 1;
var b: number;
var b = "";
var c: { x: number };
var c: { x: number };
for (var i = 0; ; ) { break; }
for (var i in {}) { }
function f(p: number) { var p: string; }
var d: any;
var d: number;
var e;
var e: string;
interface I { x: number; }
interface I { x: string; }
interface J { y: number; }
interface J { y: number; }
var g: string[];
var g: Array<string>;
let h = 1;
let h = 2;
class K {}
class K {}
function k() {}
var k = 1;
enum En { A }
var En = 2;
export {};
"#,
    ) else {
        return;
    };
    assert_eq!(
        tsc_format(&out),
        r#"a.ts(2,5): error TS2403: Subsequent variable declarations must have the same type.  Variable 'a' must be of type 'number', but here has type 'string'.
a.ts(5,5): error TS2403: Subsequent variable declarations must have the same type.  Variable 'b' must be of type 'number', but here has type 'string'.
a.ts(9,10): error TS2403: Subsequent variable declarations must have the same type.  Variable 'i' must be of type 'number', but here has type 'string'.
a.ts(10,29): error TS2403: Subsequent variable declarations must have the same type.  Variable 'p' must be of type 'number', but here has type 'string'.
a.ts(12,5): error TS2403: Subsequent variable declarations must have the same type.  Variable 'd' must be of type 'any', but here has type 'number'.
a.ts(14,5): error TS2403: Subsequent variable declarations must have the same type.  Variable 'e' must be of type 'any', but here has type 'string'.
a.ts(16,15): error TS2717: Subsequent property declarations must have the same type.  Property 'x' must be of type 'number', but here has type 'string'.
a.ts(21,5): error TS2451: Cannot redeclare block-scoped variable 'h'.
a.ts(22,5): error TS2451: Cannot redeclare block-scoped variable 'h'.
a.ts(23,7): error TS2300: Duplicate identifier 'K'.
a.ts(24,7): error TS2300: Duplicate identifier 'K'.
a.ts(25,10): error TS2300: Duplicate identifier 'k'.
a.ts(26,5): error TS2300: Duplicate identifier 'k'.
a.ts(27,6): error TS2567: Enum declarations can only merge with namespace or other enum declarations.
a.ts(28,5): error TS2567: Enum declarations can only merge with namespace or other enum declarations."#
    );
}

#[test]
fn decorator_placement() {
    let Some(out) = check(
        "\"strict\": true, \"target\": \"es2022\"",
        r#"declare function dec(...a: any[]): any;
@dec
var x: number;
@dec
function F() {}
@dec
enum E { A }
@dec
interface I {}
@dec
type T = number;
@dec
namespace N { }
var v = @dec class C { static p = 1 };
abstract class A {
    @dec abstract f1: number;
    @dec abstract m(): void;
    @dec declare d1: number;
    @dec static declare d2: number;
    constructor(@dec p: number) {}
    @dec constructor2() {}
    @dec accessor q = 1;
    @dec abstract accessor aq: number;
}
class B {
    @dec constructor() {}
    m(@dec p: number) {}
    @dec ["c"]: number = 1;
    @dec #priv = 1;
    @dec static { }
}
export {};
"#,
    ) else {
        return;
    };
    assert_eq!(
        tsc_format(&out),
        r#"a.ts(2,1): error TS1206: Decorators are not valid here.
a.ts(4,1): error TS1206: Decorators are not valid here.
a.ts(6,1): error TS1206: Decorators are not valid here.
a.ts(8,1): error TS1206: Decorators are not valid here.
a.ts(10,1): error TS1206: Decorators are not valid here.
a.ts(12,1): error TS1206: Decorators are not valid here.
a.ts(16,5): error TS1206: Decorators are not valid here.
a.ts(17,5): error TS1249: A decorator can only decorate a method implementation, not an overload.
a.ts(18,5): error TS1206: Decorators are not valid here.
a.ts(19,5): error TS1206: Decorators are not valid here.
a.ts(20,17): error TS1206: Decorators are not valid here.
a.ts(23,5): error TS1206: Decorators are not valid here.
a.ts(26,5): error TS1206: Decorators are not valid here.
a.ts(27,7): error TS1206: Decorators are not valid here.
a.ts(30,5): error TS1206: Decorators are not valid here."#
    );
}

#[test]
fn overload_sets() {
    let Some(out) = check(
        "\"strict\": true, \"target\": \"es2022\"",
        r#"function f1(x: number): void;
function f2(x: number): void;
function f2(x: string): void { }
function f3(x: number): void;
var sep = 1;
function f3(x: any) { }
function f4(x: number): void;
function g4(x: any) { }
function f5() { }
function f5() { }
export function f6(x: number): void;
function f6(x: any) { }
declare function f7(x: number): void;
function f7(x: any) { }
class C {
    m1(): void;
    m2(x: number): void;
    m2(x: any) { }
    m3(): void;
    n3() { }
    private m4(x: number): void;
    public m4(x: any) { }
    static m5(): void;
    m5() { }
    m6?(): void;
    m6() { }
    m7() { }
    m7() { }
    constructor();
}
abstract class D {
    abstract m(): void;
    abstract m(x: number): void;
    abstract n(): void;
    n(x?: any) { }
    o(): void;
    abstract o(x?: any): void;
}
function f8(x: number): string;
function f8(x: any): number { return 1; }
function f9<T>(x: T): T;
function f9(x: any) { return x; }
function f10(x: number, y: number): void;
function f10(x: number) { }
export {};
"#,
    ) else {
        return;
    };
    assert_eq!(
        tsc_format(&out),
        r#"a.ts(1,10): error TS2391: Function implementation is missing or not immediately following the declaration.
a.ts(2,10): error TS2394: This overload signature is not compatible with its implementation signature.
a.ts(4,10): error TS2391: Function implementation is missing or not immediately following the declaration.
a.ts(8,10): error TS2389: Function implementation name must be 'f4'.
a.ts(9,10): error TS2393: Duplicate function implementation.
a.ts(10,10): error TS2393: Duplicate function implementation.
a.ts(11,17): error TS2383: Overload signatures must all be exported or non-exported.
a.ts(13,18): error TS2384: Overload signatures must all be ambient or non-ambient.
a.ts(16,5): error TS2391: Function implementation is missing or not immediately following the declaration.
a.ts(20,5): error TS2389: Function implementation name must be 'm3'.
a.ts(21,13): error TS2385: Overload signatures must all be public, private or protected.
a.ts(24,5): error TS2387: Function overload must be static.
a.ts(25,5): error TS2386: Overload signatures must all be optional or required.
a.ts(27,5): error TS2393: Duplicate function implementation.
a.ts(28,5): error TS2393: Duplicate function implementation.
a.ts(29,5): error TS2390: Constructor implementation is missing.
a.ts(34,14): error TS2512: Overload signatures must all be abstract or non-abstract.
a.ts(37,14): error TS2512: Overload signatures must all be abstract or non-abstract.
a.ts(39,10): error TS2394: This overload signature is not compatible with its implementation signature."#
    );
}

#[test]
fn abstract_classes_and_members() {
    let Some(out) = check(
        "\"strict\": true, \"target\": \"es2022\"",
        r#"abstract class A {
    abstract m(): void;
    abstract p: number;
    abstract get g(): number;
    n() { }
}
class B extends A { }
class B2 extends A { m() { } }
class B3 extends A { m() { } p = 1; get g() { return 1; } }
abstract class B4 extends A { }
class B5 extends B4 { }
const E = class extends A { };
class C {
    abstract x(): void;
    abstract y: number;
}
abstract class D {
    abstract m() { }
    private abstract pm(): void;
    static abstract sm(): void;
    abstract async am(): Promise<void>;
    abstract override om(): void;
}
abstract class F { abstract m(): void; foo() { } }
class G extends A {
    m() { super.m(); }
    p = 1;
    get g() { return super.g; }
}
new A();
abstract class H { abstract q(): void; }
class I extends H { }
class J extends A { declare p: number; m() {} get g() { return 1; } }
abstract class K { abstract a(): void; abstract b(): void; abstract c(): void; abstract d(): void; abstract e(): void; abstract f(): void; }
class L extends K { }
class M extends G { t() { super.m(); super["m"](); } }
export {};
"#,
    ) else {
        return;
    };
    assert_eq!(
        tsc_format(&out),
        r#"a.ts(7,7): error TS2654: Non-abstract class 'B' is missing implementations for the following members of 'A': 'm', 'p', 'g'.
a.ts(8,7): error TS2654: Non-abstract class 'B2' is missing implementations for the following members of 'A': 'p', 'g'.
a.ts(11,7): error TS2654: Non-abstract class 'B5' is missing implementations for the following members of 'B4': 'm', 'p', 'g'.
a.ts(12,11): error TS2656: Non-abstract class expression is missing implementations for the following members of 'A': 'm', 'p', 'g'.
a.ts(14,5): error TS1244: Abstract methods can only appear within an abstract class.
a.ts(15,5): error TS1253: Abstract properties can only appear within an abstract class.
a.ts(18,14): error TS1245: Method 'm' cannot have an implementation because it is marked abstract.
a.ts(19,13): error TS1243: 'private' modifier cannot be used with 'abstract' modifier.
a.ts(20,12): error TS1243: 'static' modifier cannot be used with 'abstract' modifier.
a.ts(21,14): error TS1243: 'async' modifier cannot be used with 'abstract' modifier.
a.ts(22,23): error TS4112: This member cannot have an 'override' modifier because its containing class 'D' does not extend another class.
a.ts(26,17): error TS2513: Abstract method 'm' in class 'A' cannot be accessed via super expression.
a.ts(28,28): error TS2513: Abstract method 'g' in class 'A' cannot be accessed via super expression.
a.ts(30,1): error TS2511: Cannot create an instance of an abstract class.
a.ts(32,7): error TS2515: Non-abstract class 'I' does not implement inherited abstract member q from class 'H'.
a.ts(35,7): error TS2655: Non-abstract class 'L' is missing implementations for the following members of 'K': 'a', 'b', 'c', 'd' and 2 more."#
    );
}

#[test]
fn abstract_modifier_conflicts_ignore_comments() {
    let Some(out) = check(
        "\"strict\": true, \"target\": \"es2022\"",
        r#"abstract class A {
    abstract /* static */ m(): void;
    /* private */ abstract n(): void;
    /* abstract */ private abstract p(): void;
}
export { A };
"#,
    ) else {
        return;
    };
    assert_eq!(
        tsc_format(&out),
        "a.ts(4,28): error TS1243: 'private' modifier cannot be used with 'abstract' modifier."
    );
}

#[test]
fn override_modifiers() {
    let Some(out) = check(
        "\"strict\": true, \"target\": \"es2022\", \"noImplicitOverride\": true",
        r#"class A {
    m() { }
    p = 1;
    get g() { return 1; }
    static s() { }
    foo() { }
}
abstract class AB { abstract am(): void; abstract ap: number; }
class B extends A {
    m() { }
    override p = 2;
    override q = 3;
    get g() { return 2; }
    static s() { }
    static override t() { }
    override fooo() { }
    constructor(public p2: number, override readonly p3: number) { super(); }
    n() { }
}
class C {
    override m() { }
}
class D extends AB {
    am() { }
    ap = 1;
}
class E extends A {
    override ["m"]() { }
    override [Symbol.iterator]() { }
}
class F extends A {
    constructor(public m: any) { super(); }
}
interface I { i(): void; }
class G implements I {
    override i() { }
}
declare const Mixin: new () => { mx(): void };
class H extends Mixin {
    mx() { }
}
const X = class { override z() { } };
declare class Amb extends A { m(): void; }
class K extends A { declare p: number; }
export {};
"#,
    ) else {
        return;
    };
    assert_eq!(
        tsc_format(&out),
        r#"a.ts(10,5): error TS4114: This member must have an 'override' modifier because it overrides a member in the base class 'A'.
a.ts(12,14): error TS4113: This member cannot have an 'override' modifier because it is not declared in the base class 'A'.
a.ts(13,9): error TS4114: This member must have an 'override' modifier because it overrides a member in the base class 'A'.
a.ts(14,12): error TS4114: This member must have an 'override' modifier because it overrides a member in the base class 'A'.
a.ts(15,21): error TS4113: This member cannot have an 'override' modifier because it is not declared in the base class 'A'.
a.ts(16,14): error TS4117: This member cannot have an 'override' modifier because it is not declared in the base class 'A'. Did you mean 'foo'?
a.ts(17,36): error TS4113: This member cannot have an 'override' modifier because it is not declared in the base class 'A'.
a.ts(21,14): error TS4112: This member cannot have an 'override' modifier because its containing class 'C' does not extend another class.
a.ts(29,14): error TS4113: This member cannot have an 'override' modifier because it is not declared in the base class 'A'.
a.ts(32,17): error TS4115: This parameter property must have an 'override' modifier because it overrides a member in base class 'A'.
a.ts(36,14): error TS4112: This member cannot have an 'override' modifier because its containing class 'G' does not extend another class.
a.ts(40,5): error TS4114: This member must have an 'override' modifier because it overrides a member in the base class '{ mx(): void; }'.
a.ts(42,28): error TS4112: This member cannot have an 'override' modifier because its containing class 'X' does not extend another class."#
    );
}

#[test]
fn accessor_and_property_overrides() {
    let Some(out) = check(
        "\"strict\": true, \"target\": \"es2022\"",
        r#"declare class Amb { p: number; get g(): number; set g(v: number); }
class D1 extends Amb {
    get p() { return 1; }
    set p(v) { }
    g = 1;
}
class B2 { get a() { return 1; } get b() { return 1; } p = 1; static s = 1; static get t() { return 1; } q?: number; }
class D2 extends B2 {
    declare a: number;
    declare q: number;
}
class D3 extends B2 {
    set p(v: number) { }
    get p() { return 1; }
    static get s() { return 1; }
    static t = 1;
}
abstract class B4 { abstract p: number; abstract get g(): number; }
class D4 extends B4 {
    get p() { return 1; }
    g = 1;
}
interface B5 { p: number; }
class B5 { get g() { return 1; } }
class D5 extends B5 {
    get p() { return 1; }
    set p(v) { }
    g = 2;
}
class B6 { accessor x = 1; }
class D6 extends B6 { x = 2; }
class D7 extends B2 { b!: number; }
export {};
"#,
    ) else {
        return;
    };
    assert_eq!(
        tsc_format(&out),
        r#"a.ts(3,9): error TS2611: 'p' is defined as a property in class 'Amb', but is overridden here in 'D1' as an accessor.
a.ts(5,5): error TS2610: 'g' is defined as an accessor in class 'Amb', but is overridden here in 'D1' as an instance property.
a.ts(9,13): error TS2610: 'a' is defined as an accessor in class 'B2', but is overridden here in 'D2' as an instance property.
a.ts(13,9): error TS2611: 'p' is defined as a property in class 'B2', but is overridden here in 'D3' as an accessor.
a.ts(28,5): error TS2610: 'g' is defined as an accessor in class 'B5', but is overridden here in 'D5' as an instance property.
a.ts(31,23): error TS2610: 'x' is defined as an accessor in class 'B6', but is overridden here in 'D6' as an instance property.
a.ts(32,23): error TS2610: 'b' is defined as an accessor in class 'B2', but is overridden here in 'D7' as an instance property."#
    );
}

#[test]
fn properties_used_before_initialization() {
    let Some(out) = check(
        "\"strict\": true, \"target\": \"es2015\"",
        r#"class A1 { a: number; b = this.a; constructor() { this.a = 1; } }
class A3 { a?: number; b = this.a; c!: number; d = this.c; }
class A6 { declare a: number; b = this.a; }
class A7 { b = this.a; constructor(public a: number) { } }
class A8 { b = this.m; c = this.g; m() { } get g() { return 1; } }
class A9 { b = this.a; a!: number; }
class A10 { b = () => this.a; c = function (this: A10) { return this.a; }; a = 1; }
class A12 { static b = this.a; static a = 1; static c = A12.d; static d = 1; }
class A13 { b = A13.s; static s = 1; static t = new A13().u; u = 1; }
class A14 { static { A14.x; this.x; } static x = 1; }
class A17 { b = { x: this.a, [this.a]: 1, ...this.o }; a = 1; o = {}; }
class A18 { p!: A18; b = this.p.z; c = new A18().z; z = 1; }
class A19 { static t = { [A19.w]: 1, get [A19.w]() { return 1; } }; static w = "a"; }
class A20 { static { this.a = 1; } static a: number; static b = this.a; }
class B1 { x = 0; }
class B2 extends B1 { old = this.x; x = 1; }
class C1 { static A = class extends C1.B { }; static B = class { }; }
export {};
"#,
    ) else {
        return;
    };
    assert_eq!(
        tsc_format(&out),
        r#"a.ts(1,32): error TS2729: Property 'a' is used before its initialization.
a.ts(3,40): error TS2729: Property 'a' is used before its initialization.
a.ts(6,21): error TS2729: Property 'a' is used before its initialization.
a.ts(8,29): error TS2729: Property 'a' is used before its initialization.
a.ts(8,61): error TS2729: Property 'd' is used before its initialization.
a.ts(9,59): error TS2729: Property 'u' is used before its initialization.
a.ts(10,26): error TS2729: Property 'x' is used before its initialization.
a.ts(10,34): error TS2729: Property 'x' is used before its initialization.
a.ts(11,27): error TS2729: Property 'a' is used before its initialization.
a.ts(11,36): error TS2729: Property 'a' is used before its initialization.
a.ts(11,51): error TS2729: Property 'o' is used before its initialization.
a.ts(12,50): error TS2729: Property 'z' is used before its initialization.
a.ts(13,31): error TS2729: Property 'w' is used before its initialization.
a.ts(14,27): error TS2729: Property 'a' is used before its initialization.
a.ts(17,40): error TS2729: Property 'B' is used before its initialization."#
    );
}

#[test]
fn property_initialization_names_and_error_types() {
    let Some(out) = check(
        "\"target\": \"es2015\"",
        r#"class C {
    [x: number]: Date;
    [x: string]: Object;
    1: Date;
    'a': {}
    b: number;
    #p: number;
    fn() {
        return this;
    }
}
class D<T> {
    x: C<number>;
    y: Missing;
    z: { a: Missing };
    w: D;
    v: D<string>;
    #q: string;
    constructor() { this.#q = ""; }
}
export {};
"#,
    ) else {
        return;
    };
    assert_eq!(
        tsc_format(&out),
        r#"a.ts(6,5): error TS2564: Property 'b' has no initializer and is not definitely assigned in the constructor.
a.ts(7,5): error TS2564: Property '#p' has no initializer and is not definitely assigned in the constructor.
a.ts(13,8): error TS2315: Type 'C' is not generic.
a.ts(14,8): error TS2304: Cannot find name 'Missing'.
a.ts(15,5): error TS2564: Property 'z' has no initializer and is not definitely assigned in the constructor.
a.ts(15,13): error TS2304: Cannot find name 'Missing'.
a.ts(16,8): error TS2314: Generic type 'D<T>' requires 1 type argument(s).
a.ts(17,5): error TS2564: Property 'v' has no initializer and is not definitely assigned in the constructor."#
    );
}

#[test]
fn modifier_order_and_repetition() {
    let Some(out) = check(
        "\"strict\": true, \"target\": \"es2022\"",
        r#"class B { m() { } p = 1; q = 1; r = 1; static s() { } }
abstract class C extends B {
    static readonly public a = 1;
    override readonly public p = 1;
    static async public d() { }
    abstract override public q: number;
    static accessor public e = 1;
    readonly accessor f = 1;
    public public g = 1;
    public private h = 1;
    readonly readonly j = 1;
    override readonly static r = 1;
    public /* static */ k = 1;
    static
    public l = 1;
    declare public m2: number;
    public declare m3: number;
    static declare m4: number;
    declare static m5: number;
    declare readonly m6: number;
    readonly declare m7: number;
}
export declare const v2: number;
export default abstract class E { }
export {};
"#,
    ) else {
        return;
    };
    assert_eq!(
        tsc_format(&out),
        r#"a.ts(3,21): error TS1029: 'public' modifier must precede 'static' modifier.
a.ts(4,23): error TS1029: 'public' modifier must precede 'override' modifier.
a.ts(5,18): error TS1029: 'public' modifier must precede 'static' modifier.
a.ts(6,23): error TS1029: 'public' modifier must precede 'override' modifier.
a.ts(7,21): error TS1029: 'public' modifier must precede 'static' modifier.
a.ts(8,14): error TS1243: 'accessor' modifier cannot be used with 'readonly' modifier.
a.ts(9,12): error TS1028: Accessibility modifier already seen.
a.ts(10,12): error TS1028: Accessibility modifier already seen.
a.ts(11,14): error TS1030: 'readonly' modifier already seen.
a.ts(12,23): error TS1029: 'static' modifier must precede 'readonly' modifier.
a.ts(12,30): error TS4113: This member cannot have an 'override' modifier because it is not declared in the base class 'B'.
a.ts(15,5): error TS1029: 'public' modifier must precede 'static' modifier."#
    );
}

/// TS2814 at a function merged with a non-ambient class in another file, TS2320 naming a
/// symbol-keyed property `[Symbol.toStringTag]` and checking an interface merged with a
/// class against the class's base too, and TS2844 for a constructor local read by a
/// `typeof` in a property type. tsc 7.0.2 output.
#[test]
fn class_merges_and_property_type_queries() {
    let Some(out) = check_files(
        "\"strict\": true, \"target\": \"es2015\"",
        &[
            (
                "a.ts",
                r#"class C2 { }
function f() { }
interface I1 { [Symbol.toStringTag]: string; bar: number; }
interface I2 { [Symbol.toStringTag]: "x"; }
interface I3 extends I1, I2 { }
abstract class Base { abstract bar: number; }
class Merged extends Base { }
interface Other { bar: string; }
interface Merged extends Other { }
class K {
    a: typeof x;
    b: typeof y;
    constructor(x: number, public y: string) { }
}
"#,
            ),
            ("b.ts", "function C2() { }\nclass f { }\n"),
        ],
    ) else {
        return;
    };
    assert_eq!(
        tsc_format(&out),
        r#"a.ts(1,7): error TS2813: Class declaration cannot implement overload list for 'C2'.
a.ts(2,10): error TS2814: Function with bodies can only merge with classes that are ambient.
a.ts(5,11): error TS2320: Interface 'I3' cannot simultaneously extend types 'I1' and 'I2'.
  Named property '[Symbol.toStringTag]' of types 'I1' and 'I2' are not identical.
a.ts(9,11): error TS2320: Interface 'Merged' cannot simultaneously extend types 'Base' and 'Other'.
  Named property 'bar' of types 'Base' and 'Other' are not identical.
a.ts(11,15): error TS2844: Type of instance member variable 'a' cannot reference identifier 'x' declared in the constructor.
a.ts(12,15): error TS2844: Type of instance member variable 'b' cannot reference identifier 'y' declared in the constructor.
b.ts(1,10): error TS2814: Function with bodies can only merge with classes that are ambient.
b.ts(2,7): error TS2813: Class declaration cannot implement overload list for 'f'."#
    );
}

/// A method whose body the parser made up after a missing `{` has no implementation, so
/// its parameter initializers are TS2371 (callSignaturesWithParameterInitializers2; tsc
/// prints them only without syntax errors, so this checks the semantic list directly).
#[test]
fn initializers_in_made_up_bodies() {
    let Some(libs) = LibStore::locate() else {
        return;
    };
    let mut fs = MemoryFs::new();
    fs.set_cwd("/p");
    fs.add_file(
        "/p/a.ts",
        "var b = {\n    foo(x = 1),\n    bar(y = 1) { },\n};\n",
    );
    fs.add_file(
        "/p/tsconfig.json",
        r#"{"compilerOptions": {"strict": false, "target": "es2015"}, "files": ["a.ts"]}"#,
    );
    let cfg = load_tsconfig(&fs, "/p/tsconfig.json");
    let program = load_program(&fs, &cfg.file_names, &cfg.options, Some(&libs), 1);
    let result = check_program_through_syntax_errors(&program, CheckRun { threads: 1 });
    let semantic: Vec<String> = result
        .diagnostics
        .iter()
        .filter(|d| d.code == 2371)
        .map(|d| {
            let f = &program.files[d.file.0 as usize];
            let lc = f.lines().line_col_utf16(&f.text, d.span.start);
            format!("{}:{} TS{}", lc.line + 1, lc.col + 1, d.code)
        })
        .collect();
    assert_eq!(semantic, ["2:9 TS2371"]);
}
