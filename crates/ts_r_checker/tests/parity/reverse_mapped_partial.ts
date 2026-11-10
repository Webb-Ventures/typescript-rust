// Partial reverse mapped inference: before its context-sensitive functions are typed, a
// literal argument whose parameter is `{ [K in keyof T]: X }` infers `T` from its other
// members (a function's key reverse-maps to `unknown`; nothing inferable at any depth infers
// nothing). The functions then fix `T` when their contextual parameters read it, and see
// `unknown` members when it was fixed to `unknown`. Array literal elements of a generic
// mapped context are typed by index (`T["0"]`), so literals stay under a string constraint.
export {};
type Box<T> = { contents?: T; contains?(content: T): boolean };
type Mapped<T> = { [K in keyof T]: Box<T[K]> };
declare function id<T>(arg: Mapped<T>): Mapped<T>;
const o2 = id({ foo: { contents: "", contains(k) { return k.length > 0; } } });
const o3 = id({ foo: { contains(k) { return k.length > 0; } } });
const c2: number = o2;
const c3: number = o3;

declare function f<T>(x: { [K in keyof T]: (v: T[K]) => void }): T;
const r1_ = f({ a: (v) => {}, b: (x: number) => {} });
const r1: number = r1_;
declare function g<T>(x: { [K in keyof T]: Box<T[K]> }): T;
const r2_ = g({ a: { contents: 1, contains: (k) => k > 0 }, b: { contains(k) { return true; } } });
const r2: number = r2_;
declare function p2<T>(x: { [K in keyof T]: (s: string) => T[K] }): T;
const r3_ = p2({ a: (s) => 1, b: (): string => "" });
const r3: number = r3_;
declare function q<T>(x: { [K in keyof T]: T[K] }): T;
const r4_ = q({ a: 1, b: (v) => v });
const r4: number = r4_;
declare function q2<T>(x: { [K in keyof T]?: { v: T[K], f: (x: T[K]) => void } }): T;
const r5_ = q2({ a: { v: 1, f: (x) => {} }, b: { f: (x) => {} } });
const r5: number = r5_;
declare function arr<T extends unknown[]>(x: { [K in keyof T]: { v: T[K]; cb: (x: T[K]) => void } }): T;
const r6_ = arr([{ v: 1, cb: x => { const y: string = x; } }, { v: "s", cb: x => {} }]);
const r6: number = r6_;

declare function tup<T extends readonly string[]>(x: { [K in keyof T]: { type: T[K]; cb: (x: T[K]) => void } }): T;
const t1_ = tup([{ type: "a", cb: (x) => {} }, { type: "b", cb: (x: string) => {} }]);
const t1: number = t1_;
declare function t3<T extends readonly string[]>(x: { [K in keyof T]: T[K] }): T;
const t2_ = t3(["a", "b"]);
const t2: number = t2_;
interface Ev { onclick: (e: { x: number }) => void; onkey: (e: { k: string }) => void; other: 1 }
declare function bindAll<T extends readonly [keyof Ev & `on${any}`, ...(keyof Ev & `on${any}`)[]]>(
  bindings: { [K in keyof T]: { type: T[K]; listener: (ev: Parameters<Ev[T[K]]>[0]) => void } },
): T;
const t4_ = bindAll([{ type: "onclick", listener: (ev) => { const n: string = ev.x; } }, { type: "onkey", listener: (ev) => {} }]);
const t4: number = t4_;
