// Reverse mapped inference: `T` behind `{ [K in keyof T]: X<T[K]> }` gets the source's keys,
// each typed by what its value infers for `T[K]` against `X`. Optionality is kept unless the
// mapped type adds `?`; readonly is kept unless it adds `readonly`. Other type parameters of
// the template infer from the source with `T` reverse-mapped (not from `any`). A recursive
// mapped type meeting the same source again is a silent `any`. A `const` `T` keeps literals.
export {};
type Box<T> = { value: T };
declare function d1<U>(descs: { [K in keyof U]: Box<U[K]> }): U;
declare function d2<U>(descs: { readonly [K in keyof U]?: Box<U[K]> }): U;
declare const opt: { a?: Box<number>; readonly b: Box<string> };
declare const arr: Box<number>[];
declare const tup: [Box<number>, Box<string>?];
const r2: string = d1(opt);
const r3: string = d2(opt);
const r4: string = d1(arr);
const r5: string = d1(tup);
const r6: string = d1({ a: { value: 1 }, b: { value: "x" as const } });
type Funcs<A, B> = { [K in keyof B]: { fn: (a: A, b: B) => void; thing: B[K] } };
declare function foo<A, B>(fns: Funcs<A, B>): [A, B];
const f1: string = foo({ bar: { fn: (a: string) => {}, thing: "asd" } });
type Sel<S, R> = (state: S) => R;
declare function sel<S, T>(s: { [K in keyof T]: Sel<S, T[K]> }): Sel<S, T>;
const s1: string = sel({ a: (state: any) => 1 });
type Deep<T> = { [K in keyof T]: Deep<T[K]> };
declare function deep<T>(d: Deep<T>): T;
interface A { a: A }
declare let a: A;
const o = deep(a);
o.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a.a;
declare function h1<const V>(route: { [R in keyof V]: { mw: () => V[R] } }): V;
const a1 = h1({ ex: { mw: () => "someValue" } });
const n1: number = a1;
declare function h4<V>(route: { [R in keyof V]: () => V[R] }): V;
const a4 = h4({ ex: () => "someValue" });
const n4: number = a4;
type PropDesc<T> = { value?: T; get?(): T };
type PropDescMap<T> = { [K in keyof T]: PropDesc<T[K]> };
declare function defineProps<T, U>(obj: T, descs: PropDescMap<U> & ThisType<T>): T & U;
declare const p1: { x: number };
const dp: string = defineProps(p1, { bar: { get(): number { return this.x; } } });
