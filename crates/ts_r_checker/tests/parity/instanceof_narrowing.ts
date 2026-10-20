// instanceof narrowing: derived members stay, else instance types that are subtypes of members, else the instance type or an intersection; prototype wins over construct signatures.
declare function n(x: never): void;
class A { a = 1 }
class B { a = 1 }
class C { c = 1 }
class D extends A { d = 1 }
function f1(x: A) { if (x instanceof B) n(x); else n(x); }
function f2(x: A | C) { if (x instanceof B) n(x); else n(x); }
function f4(x: A) { if (x instanceof D) n(x); else n(x); }
function f5(x: D) { if (x instanceof A) n(x); else n(x); }
interface I { a: number }
function f7(x: I | C) { if (x instanceof A) n(x); else n(x); }
function f8(x: C) { if (x instanceof A) n(x); else n(x); }
function f10(x: object) { if (x instanceof A) n(x); else n(x); }
function f11(x: Object | C) { if (x instanceof A) n(x); else n(x); }
function g1(x: string | number | Date) { if (x instanceof Object) n(x); else n(x); }
function g2(x: string) { if (x instanceof Object) n(x); else n(x); }
function g3(x: string | { a: 1 }) { if (x instanceof Object) n(x); else n(x); }
function g6(x: object | string) { if (x instanceof Date) n(x); else n(x); }
function g7(x: number | C) { if (x instanceof C) n(x); else n(x); }
function g8(x: Function | object) { if (x instanceof Function) n(x); else n(x); }
declare const ab: typeof A | typeof C;
function h1(x: A | B | C) { if (x instanceof ab) n(x); else n(x); }
function h2(x: A | B | C, k: typeof A) { if (x instanceof k) n(x); else n(x); }
function h3(x: D | B | C) { if (x instanceof ab) n(x); else n(x); }
function k1<T>(x: T) { if (x instanceof A) n(x); else n(x); }
function k3<T>(x: T | A) { if (x instanceof A) n(x); else n(x); }
function k4(x: any) { if (x instanceof A) n(x); else n(x); }
function k5(x: unknown) { if (x instanceof A) n(x); else n(x); }
function k6(x: A[] | string) { if (x instanceof Array) n(x); else n(x); }
function k7(x: readonly number[] | number) { if (x instanceof Array) n(x); else n(x); }
function k8(x: [number, string] | number) { if (x instanceof Array) n(x); else n(x); }
interface GC { prototype: G1; new (): G2 }
interface G1 { foo1: number }
interface G2 { foo2: boolean }
declare var G: GC;
function m1(x: G1 | G2) { if (x instanceof G) n(x); else n(x); }
function m2(x: any) { if (x instanceof Object) n(x); if (x instanceof Function) n(x); }
class E { x: string | undefined }
function m3<T extends E>(v: T | { x: string }) { if (v instanceof E) n(v); else n(v); }
function m4<T extends A>(v: T | string) { if (v instanceof A) n(v); else n(v); }
