// Inference: enum literal unions widen to their enum; intersection members of the return type
// are at its top level (no widening).
export {};
enum Enum { A, B, C }
interface Interface<T extends Enum> {
    type: T;
}
declare function foo<T extends Enum>(x: Interface<T>): T;
declare function foo2<T>(x: Interface<T & Enum>): T;
declare function bar<T>(x: { type: T }): T;
declare const x: Interface<Enum.A | Enum.B> | Interface<Enum.C>;
const r1 = foo(x);
const n1: string = r1;
const r2 = bar(x);
const n2: string = r2;
declare const y: { type: 1 | 2 } | { type: 3 };
const r3 = bar(y);
const n3: string = r3;
declare function baz<T extends number>(x: { type: T }): T;
const r4 = baz(y);
const n4: string = r4;
declare function f<T>(x: T): NonNullable<T>;
declare function g<T>(x: T): T & {};
declare function h<T>(x: T): T | undefined;
enum E { A, B }
const a = f(E.A);
const b: E.A = a;
const c = g(E.A);
const d: E.A = c;
const e = h(E.A);
const k: E.A | undefined = e;
const l = f("x");
const m: "x" = l;
