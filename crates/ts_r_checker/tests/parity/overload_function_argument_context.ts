// The first overload that checks a context-sensitive function argument fixes its
// parameter types; later overloads see the function as typed then.
declare function m(cb: (x: number) => number): void;
declare function m(cb: (x: string) => string): void;
m((x) => "" + x);
declare function v(a: (x: number) => void, y: number): void;
declare function v(a: (x: string) => void, y: number): void;
v((x) => { x.toFixed() }, 1);
declare function f(cb: (x: number) => void, y: number): void;
declare function f(cb: (x: string) => void, y: string): void;
f((x) => { x.toUpperCase() }, "s");

// An overload whose other arguments don't fit never checks the function; one whose
// parameter no function fits is passed over too.
declare function u(a: string, y: number): void;
declare function u(a: (x: number) => void, y: number): void;
u((x) => { x.foo }, 1);
declare function u3(a: { foo: string }, y: number): void;
declare function u3(a: (x: number) => void, y: number): void;
u3((x) => { x.foo }, 1);
declare function u5(a: Function, y: number): void;
declare function u5(a: (x: number) => void, y: number): void;
u5((x) => { x.foo }, 1);
declare function u6<T>(a: T, y: number): void;
declare function u6(a: (x: number) => void, y: number): void;
u6((x) => { x.foo }, 1);
declare function ev(p: BooleanConstructor): boolean;
declare function ev(p: (v: number) => boolean): boolean;
ev((value) => value > 0);
declare class C { constructor(x: string) }
declare function ct(p: typeof C): boolean;
declare function ct(p: (v: number) => boolean): boolean;
ct((value) => value > 0);
declare function p2(p: { prototype: C }): boolean;
declare function p2(p: (v: number) => boolean): boolean;
p2((value) => { const s: string = value; return true });
declare function p7(p: { length: number }): boolean;
declare function p7(p: (v: number) => boolean): boolean;
p7((value) => { const s: string = value; return true });
declare function p9(p: { foo?: string }): boolean;
declare function p9(p: (v: number) => boolean): boolean;
p9((value) => { const s: string = value; return true });
declare function w<T>(a: (x: number) => T, y: number): T;
declare function w(a: (x: string) => void, y: number): void;
w((x) => { x.toUpperCase() }, 1);

// When every overload fails, a function keeps the context that first fixed it...
interface Val<T> { get(): T }
interface Obs<T> {
  v: T
  pr<S extends Val<unknown>>(a: number | undefined, b: number | undefined, sel: (shared: Obs<T>) => S): Obs<S>
  pr(a: number | undefined, b: number | undefined, sel: undefined, x: string): Obs<T>
  pr(a?: number, b?: number, x?: string): Obs<T>
}
declare const source: Obs<number>
export const r = source.pr(2, 100, (shared) => shared)
declare function two<S extends Val<unknown>>(sel: (shared: number) => S): S
declare function two(x: string): number
export const r2 = two((shared) => shared)
// ...or the last overload's, when its check reaches the function before an error...
declare function h(cb: (x: number) => void, y: number): void;
declare function h(cb: (x: string) => void, y: string): void;
declare function h(cb: (x: boolean) => void, y: string, z: number): void;
h((x) => { x.toFixed() }, true as any as boolean);
// ...else the first overload's when one is generic, the parameter type all agree on, or none.
declare function k(y: number, cb: (x: number) => void): void;
declare function k(y: string, cb: (x: string) => void): void;
k(true as any as boolean, (x) => { x.foo });
declare function z<T>(y: number, cb: (x: T) => void): void;
declare function z(y: string, cb: (x: string) => void): void;
z(true as any as boolean, (x) => { x.foo });
declare function z2(y: string, cb: (x: string) => void): void;
declare function z2<T>(y: number, cb: (x: number) => void): T;
z2(true as any as boolean, (x) => { x.foo });
declare function z3(y: string, cb: (x: string) => void): void;
declare function z3(y: number, cb: (x: string) => void): void;
z3(true as any as boolean, (x) => { x.foo });
declare function z6(y: string, cb: (x: number) => void, z?: number): void;
declare function z6<T>(y: number, cb: (x: T) => void): T;
z6(true as any as boolean, (x) => { x.foo });
// A single overload always gives its context.
declare function q(y: number, cb: (x: number) => void): void;
q("s", (x) => { x.foo });
declare function g1<T>(y: number, cb: (x: T) => void, z: T): void;
g1("s", (x) => { x.foo }, "a");

// A failed call with a generic overload stands for the overload with the most parameters
// (the first of those), its type arguments inferred without context-sensitive arguments.
declare function pk<T>(a: T, cb: (x: T) => void, z?: number): T[];
declare function pk(a: string, cb: (x: string) => void, z?: string): string;
export const rp: boolean = pk(1, (x) => { x.foo }, true as any as boolean);
declare function gb<K, E>(key: (v: number) => K, el: (v: number) => E, d?: (g: [K, E]) => void, c?: number): [K, E];
declare function gb<K>(key: (v: number) => K, el: undefined, d: (g: [K, number]) => void): [K, number];
export const rg: string = gb((v) => v % 2, (v) => `${v}`, (g) => { const s: string = g }, true as any as boolean);
