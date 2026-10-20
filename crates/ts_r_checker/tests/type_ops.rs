//! Mapped, conditional and template literal types, the lib utility types and the
//! instantiation limits (expected output taken from tsc 7.0.2).
//! Skipped when the lib files are not available (`cargo xtask upstream sync --minimal`).

use ts_r_checker::{CheckRun, LibStore, check_program, load_program};
use ts_r_config::{MemoryFs, load_tsconfig};

ts_r_testing::cap_memory!();

/// Checks `src` as `/p/a.ts` under `strict` and returns `line:col TScode: message` lines.
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
                format!("{}:{} TS{}: {}", lc.line + 1, lc.col + 1, d.code, d.message)
            })
            .collect(),
    )
}

#[test]
fn mapped_and_utility_types() {
    let Some(out) = check(
        r#"interface Foo { a: string; readonly b?: number; c: boolean }
const p1: Partial<Foo> = {};
const p2: Partial<Foo> = { a: 1 };
const r1: Required<Foo> = { a: "", c: true };
const ro: Readonly<Foo> = { a: "", c: true };
ro.a = "x";
const pk: Pick<Foo, "a" | "c"> = { a: "", c: 1 };
const om: Omit<Foo, "a"> = { c: true, a: "" };
const rc: Record<"x" | "y", number> = { x: 1 };
type Getters<T> = { [K in keyof T as `get${Capitalize<string & K>}`]: () => T[K] };
declare const gt: Getters<Foo>;
const g1: number = gt.getA();
type Mut<T> = { -readonly [K in keyof T]-?: T[K] };
declare const mu: Mut<Foo>;
mu.b = undefined;
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        vec![
            "3:28 TS2322: Type 'number' is not assignable to type 'string'.",
            "4:7 TS2741: Property 'b' is missing in type '{ a: string; c: true; }' but required in type 'Required<Foo>'.",
            "6:4 TS2540: Cannot assign to 'a' because it is a read-only property.",
            "7:43 TS2322: Type 'number' is not assignable to type 'boolean'.",
            "8:39 TS2353: Object literal may only specify known properties, and 'a' does not exist in type 'Omit<Foo, \"a\">'.",
            "9:7 TS2741: Property 'y' is missing in type '{ x: number; }' but required in type 'Record<\"x\" | \"y\", number>'.",
            "12:7 TS2322: Type 'string' is not assignable to type 'number'.",
            "15:1 TS2322: Type 'undefined' is not assignable to type 'number'.",
        ]
    );
}

#[test]
fn conditional_types_and_infer() {
    let Some(out) = check(
        r#"declare function fn(a: string, b?: number): boolean;
const rt: ReturnType<typeof fn> = "x";
const ps: Parameters<typeof fn> = [1];
const nn: NonNullable<string | null | undefined> = null;
const aw: Awaited<Promise<Promise<number>>> = "x";
const ex: Exclude<"a" | "b" | "c", "a"> = "a";
const xt: Extract<string | number, number> = "x";
type IsStr<T> = T extends string ? true : false;
const d1: IsStr<"a" | 1> = true;
type Box<T> = [T] extends [string] ? "yes" : "no";
const d2: Box<"a" | 1> = "yes";
type Elem<T> = T extends (infer U)[] ? U : never;
const e1: Elem<number[]> = "x";
type FirstStr<T> = T extends [infer S extends string, ...unknown[]] ? S : never;
const f1: FirstStr<["a", 1]> = "b";
const f2: FirstStr<[1, "a"]> = "b";
function g<T>(x: T): T extends string ? "s" : "n" { return null!; }
const g1: "n" = g("a");
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        vec![
            "2:7 TS2322: Type 'string' is not assignable to type 'boolean'.",
            "3:36 TS2322: Type 'number' is not assignable to type 'string'.",
            "4:7 TS2322: Type 'null' is not assignable to type 'string'.",
            "5:7 TS2322: Type 'string' is not assignable to type 'number'.",
            "6:7 TS2322: Type '\"a\"' is not assignable to type '\"b\" | \"c\"'.",
            "7:7 TS2322: Type 'string' is not assignable to type 'number'.",
            "11:7 TS2322: Type '\"yes\"' is not assignable to type '\"no\"'.",
            "13:7 TS2322: Type 'string' is not assignable to type 'number'.",
            "15:7 TS2322: Type '\"b\"' is not assignable to type '\"a\"'.",
            "16:7 TS2322: Type '\"b\"' is not assignable to type 'never'.",
            "18:7 TS2322: Type '\"s\"' is not assignable to type '\"n\"'.",
        ]
    );
}

#[test]
fn template_literal_types() {
    let Some(out) = check(
        r#"type Px = `${number}px`;
const px1: Px = "10px";
const px2: Px = "tenpx";
type Ev<T extends string> = `on${Capitalize<T>}`;
const ev: Ev<"click" | "focus"> = "onBlur";
const up: Uppercase<"abc"> = "abc";
const lo: Lowercase<"ABC"> = "abc";
const un: Uncapitalize<"Abc"> = "Abc";
type Split<S> = S extends `${infer H}.${infer R}` ? [H, ...Split<R>] : [S];
const sp: Split<"a.b.c"> = ["a", "b"];
type ToNum<S> = S extends `${infer N extends number}` ? N : never;
const tn: ToNum<"42"> = 43;
declare let s: string;
const t1: `a${string}` = s;
const t2: `a${string}` = `a${s}`;
declare const n: number;
const x1: "a1" = `a${1}`;
const x2: `a${number}` = `a${n}`;
const x3 = `a${n}` as const;
const x4: "q" = x3;
declare function pick<S extends "a" | "b", E extends "x" | "y">(p: `${S}:${E}`): [S, E];
function h<S extends "a" | "b", E extends "x" | "y">(s: S, e: E) { return pick(`${s}:${e}`); }
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        vec![
            "3:7 TS2322: Type '\"tenpx\"' is not assignable to type '`${number}px`'.",
            "5:7 TS2322: Type '\"onBlur\"' is not assignable to type '\"onClick\" | \"onFocus\"'.",
            "6:7 TS2322: Type '\"abc\"' is not assignable to type '\"ABC\"'.",
            "8:7 TS2322: Type '\"Abc\"' is not assignable to type '\"abc\"'.",
            "10:7 TS2322: Type '[\"a\", \"b\"]' is not assignable to type '[\"a\", \"b\", \"c\"]'.\n  Source has 2 element(s) but target requires 3.",
            "12:7 TS2322: Type '43' is not assignable to type '42'.",
            "14:7 TS2322: Type 'string' is not assignable to type '`a${string}`'.",
            "20:7 TS2322: Type '`a${number}`' is not assignable to type '\"q\"'.",
        ]
    );
}

#[test]
fn generic_mapped_apparent_types_and_this_type() {
    let Some(out) = check(
        r#"declare class Component<P> {
    readonly props: Readonly<{ children?: unknown }> & Readonly<P>;
}
interface AnchorProps { anchorRef?: (a: number) => void }
function decorate<P>() {
    return class extends Component<AnchorProps & P> {
        go() { const r: string = this.props.anchorRef; }
    };
}
type Methods<D> = { m(): void; w?: { k(): void } } & ThisType<D & { extra: number }>;
declare function define<D>(o: { data: D } & Methods<D>): D;
define({
    data: { count: 1 },
    m() { const c: string = this.count; },
    w: { k() { const e: string = this.extra; } },
});
declare function plain(o: { w?: { k(): void } }): void;
plain({ w: { k() { const p: number = this; } } });
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        vec![
            "7:22 TS2322: Type '((a: number) => void) | undefined' is not assignable to type 'string'.\n  Type 'undefined' is not assignable to type 'string'.",
            "14:17 TS2322: Type 'number' is not assignable to type 'string'.",
            "15:22 TS2322: Type 'number' is not assignable to type 'string'.",
            "18:26 TS2322: Type '{ k(): void; }' is not assignable to type 'number'.",
        ]
    );
}

/// Tuples over the length limit, unions over the size limit and deep recursion produce
/// diagnostics, not hangs or memory blowups.
#[test]
fn pathological_types() {
    let Some(out) = check(
        r#"type Double<T extends unknown[]> = [...T, ...T];
type D2 = Double<[1, 1]>;
type D4 = Double<D2>; type D8 = Double<D4>; type D16 = Double<D8>; type D32 = Double<D16>;
type D64 = Double<D32>; type D128 = Double<D64>; type D256 = Double<D128>; type D512 = Double<D256>;
type D1k = Double<D512>; type D2k = Double<D1k>; type D4k = Double<D2k>; type D8k = Double<D4k>;
type D16k = Double<D8k>;
declare const big: D16k;
type Dg = "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9";
type B5 = `${Dg}${Dg}${Dg}${Dg}${Dg}`;
type B4 = `${Dg}${Dg}${Dg}${Dg}`;
type Deep<T> = { [K in keyof T]: Deep<T[K]> };
declare const dp: Deep<{ a: { b: { c: number } } }>;
const dc: string = dp.a.b.c;
const o1: { v: number } = 2;
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        vec![
            "5:85 TS2799: Type produces a tuple type that is too large to represent.",
            "9:11 TS2590: Expression produces a union type that is too complex to represent.",
            "13:7 TS2322: Type 'number' is not assignable to type 'string'.",
            "14:7 TS2322: Type 'number' is not assignable to type '{ v: number; }'.",
        ]
    );
}

#[test]
fn recursive_conditional_types_stop_with_ts2589() {
    // tsc reports at the recursive reference in the alias (1:29); we report at the use.
    let Some(out) = check(
        r#"type Inf<T> = T extends T ? Inf<[T]> : never;
type Inf1 = Inf<1>;
type GetChars<S, Acc = never> = S extends `${infer C}${infer R}` ? GetChars<R, C | Acc> : Acc;
const ch: GetChars<"abc"> = "d";
type TupleOf<N extends number, A extends unknown[] = []> =
    A["length"] extends N ? A : TupleOf<N, [0, ...A]>;
const t5: TupleOf<5>["length"] = 4;
"#,
    ) else {
        return;
    };
    assert!(out.iter().any(|l| l.contains("TS2589")), "{out:?}");
    assert!(
        out.iter()
            .any(|l| l.starts_with("4:7 TS2322: Type '\"d\"' is not assignable")),
        "{out:?}"
    );
    assert!(
        out.iter()
            .any(|l| l == "7:7 TS2322: Type '4' is not assignable to type '5'."),
        "{out:?}"
    );
}

#[test]
fn inference_through_resolved_type_operators() {
    // Real-world shapes (Pulumi `Output<T>`, `Promise.resolve`, `Record` defaults,
    // `flatMap`, `new Map(xs.map(...))`) that only type-check once the inference around
    // the now-resolved conditional and mapped types matches tsc.
    let Some(out) = check(
        r#"interface OutputInstance<T> { apply<U>(f: (t: T) => U): OutputInstance<U>; }
type Lifted<T> = T extends string ? {} : T extends object ? { readonly [P in keyof T]: OutputInstance<T[P]> } : {};
type Output<T> = OutputInstance<T> & Lifted<T>;
declare function invokeOutput<T>(tok: string): Output<T>;
interface R { readonly a: string; readonly b: number }
export function f(): Output<R> { return invokeOutput("x"); }
declare function k<T>(): { z: T };
export const h1: { z: R } = k();
interface Up { url: string; method: 'PUT' }
export async function up(): Promise<Up> { return Promise.resolve({ url: 'a', method: 'PUT' }); }
export const p: Promise<Up> = Promise.resolve({ url: 'a', method: 'PUT' });
declare function cond<T>(x: T): T extends string ? T : number;
export const c1: 'a' = cond('a');
declare function two<T>(a: T, b: T): T;
export const d5: { x: 'a' | 'b' } = two({ x: 'a' }, { x: 'c' });
type V = 'primary' | 'secondary';
const variants: Record<V, string> = { primary: 'a', secondary: 'b' };
export function bv({ variant = 'primary' }: { variant?: V } = {}): string { return variants[variant]; }
export function opt(o: { a?: number } | undefined = {}) { return o.a; }
declare const rules: { code: string }[];
export const fl: { code: string; id: string | null }[] = rules.flatMap((r) =>
  r.code ? [{ code: r.code, id: null }] : []);
export const byKey = new Map(rules.map((r) => [r.code, r]));
export const got: { code: string } | undefined = byKey.get('x');
export function el(xs: Record<string, unknown>[], m: Map<string, Record<string, unknown>>) {
  const s = m.get('a');
  if (!s) return '';
  (s.items as unknown[])[0] = 1;
  return String(s.end) + (xs[0] ? String(xs[0].id) : '');
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        vec![
            "15:14 TS2322: Type '{ x: \"a\"; } | { x: \"c\"; }' is not assignable to type '{ x: \"a\" | \"b\"; }'.\n  Type '{ x: \"c\"; }' is not assignable to type '{ x: \"a\" | \"b\"; }'.\n    Types of property 'x' are incompatible.\n      Type '\"c\"' is not assignable to type '\"a\" | \"b\"'."
        ]
    );
}

#[test]
fn huge_template_unions_stay_bounded() {
    // 10^4 members: related member by member without blowing up.
    let Some(out) = check(
        r#"type Dg = "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9";
type B4 = `${Dg}${Dg}${Dg}${Dg}`;
const ok: B4 = "1234";
const k: `k${B4}` = "k0042";
type Many = `${Dg}${Dg}${Dg}${Dg}${Dg}${Dg}${Dg}${Dg}`;
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        vec!["5:13 TS2590: Expression produces a union type that is too complex to represent."]
    );
}

#[test]
fn template_constraints_through_their_own_parameter_terminate() {
    // tsc also reports the circular constraints (TS2313 at 1:22 and 2:22) and the spans
    // that are not string-like (TS2322 at 1:26, 2:25 and 2:29); ts-r does not detect
    // circularity through a template yet.
    let Some(out) = check(
        "function f<T extends `x${T}`>(x: T) { x.foo; }\nfunction g<T extends `${T}${T}`, U extends T>(x: U) { x.bar; }\n",
    ) else {
        return;
    };
    assert_eq!(
        out,
        vec![
            "1:41 TS2339: Property 'foo' does not exist on type 'T'.",
            "2:57 TS2339: Property 'bar' does not exist on type 'U'.",
        ]
    );
}

#[test]
fn many_keys_remapped_to_one_property_stay_linear() {
    // 4^6 = 4096 keys, all mapped to `x`: the key union is built once (0.4 s in a debug
    // build; 30 s when it was rebuilt for every key).
    let start = std::time::Instant::now();
    let Some(out) = check(
        r#"type K = "a" | "b" | "c" | "d";
type Keys = `${K}${K}${K}${K}${K}${K}`;
type M = { [P in Keys as "x"]: P };
declare const m: M;
const s: Keys = m.x;
const t: "x" = m.x;
"#,
    ) else {
        return;
    };
    assert_eq!(out.len(), 1, "{out:?}");
    assert!(
        out[0].starts_with("6:7 TS2322: Type '\"")
            && out[0].contains("not assignable to type '\"x\"'"),
        "{out:?}"
    );
    assert!(start.elapsed().as_secs() < 10, "took {:?}", start.elapsed());
}

#[test]
fn keys_remapped_to_one_property_keep_the_first_key_s_modifiers() {
    let Some(out) = check(
        r#"type M<T> = { [K in keyof T as "x"]: T[K] };
const x1: M<{ a?: number; b?: number }> = {};
const x2: M<{ a?: number; b: number }> = {};
interface P3 { a: number; b?: number }
const x3: M<P3> = {};
declare const r: M<{ readonly a: number; readonly b: number }>;
r.x = 1;
declare const r2: M<{ readonly a: number; b: number }>;
r2.x = 1;
declare const r3: M<{ a: number; readonly b: number }>;
r3.x = 1;
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        vec![
            "5:7 TS2741: Property 'x' is missing in type '{}' but required in type 'M<P3>'.",
            "7:3 TS2540: Cannot assign to 'x' because it is a read-only property.",
            "9:4 TS2540: Cannot assign to 'x' because it is a read-only property.",
        ]
    );
}

#[test]
fn template_literal_union_members_sort_by_content() {
    // Union order is by content, not by creation order (`B` is created first here; tsc,
    // which orders by creation, prints `b${number}` first).
    let Some(out) = check(
        r#"type B = `b${number}`;
type A = `a${number}`;
let bad: B | A = 1;
let bad2: `x${string}` | `x${number}` = 1;
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        vec![
            "3:5 TS2322: Type '1' is not assignable to type '`a${number}` | `b${number}`'.",
            "4:5 TS2322: Type '1' is not assignable to type '`x${string}` | `x${number}`'.",
        ]
    );
}

#[test]
fn inferred_numbers_that_do_not_spell_back_widen() {
    let Some(out) = check(
        r#"type Num<S> = S extends `${infer N extends number}` ? N : never;
const x: Num<"01"> = 1;
const y: Num<"1.0"> = 2;
const z: Num<"1e2"> = 3;
const a1: Num<"0x10"> = "s";
const a4: Num<"-0"> = "s";
const a5: Num<"1.5"> = "s";
const a6: Num<"abc"> = "s";
type L<S> = S extends `${infer N extends 1 | 2}` ? N : never;
const l1: L<"01"> = "s";
type Big<S> = S extends `${infer N extends bigint}` ? N : never;
const b1: Big<"12"> = "s";
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        vec![
            "5:7 TS2322: Type 'string' is not assignable to type 'number'.",
            "6:7 TS2322: Type 'string' is not assignable to type 'number'.",
            "7:7 TS2322: Type '\"s\"' is not assignable to type '1.5'.",
            "8:7 TS2322: Type '\"s\"' is not assignable to type 'never'.",
            "10:7 TS2322: Type '\"s\"' is not assignable to type 'never'.",
            "12:7 TS2322: Type '\"s\"' is not assignable to type '12n'.",
        ]
    );
}
