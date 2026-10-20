// Contextual signatures from unions, discriminated object literal contexts, async
// contextual returns and inference from a function-typed return context.
const f1: ((a: string) => void) | ((a: number) => void) = (a) => { const z: 1 = a; };
const f2: ((a: string) => void) | ((a: string) => number) = (a) => { const z: 1 = a; };
const f3: ((a: string) => void) | ((a: string, b: number) => void) = (a, b) => { const z: 1 = a; const y: 1 = b; };
const f6: (<T>(a: T) => void) | ((a: string) => void) = (a) => { const z: 1 = a; };
const f8: ((a?: string) => void) | ((a: string) => void) = (a) => { const z: 1 = a; };
declare function test(arg: Record<string, (arg: string) => void> | Array<(arg: number) => void>): void;
test([(arg) => { arg; }]);

type S = { d: "s"; cb: (x: string) => void };
type N = { d: "n"; cb: (x: number) => void };
declare function foo(foo: S | N): void;
foo({ d: `${"s"}`, cb: (x) => { const z: 1 = x; } });
foo({ cb: (x) => { const z: 1 = x; }, d: "n" });
enum K { A, B }
type EA = { k: K.A; f: (x: string) => void } | { k: K.B; f: (x: number) => void };
const e1: EA = { k: K.B, f: (x) => { const z: 1 = x; } };
type O = { t?: "a"; f: (x: string) => void } | { t: "b"; f: (x: number) => void };
const o1: O = { f: (x) => { const z: 1 = x; } };
type P = { href: string; on?: (e: string) => void } | { href?: never; on?: (e: number) => void };
const p3: P = { href: `2${undefined}3`, on: (ev) => { const z: 1 = ev; } };
const p4: P = { on: (ev) => { const z: 1 = ev; } };

declare class SM<T> { onDone: (a: T) => void; }
declare function mk<T>(): T;
type Z = { zz: 1 };
const a1: () => Promise<{ count: number }> | SM<{ count: number }> = async () => Promise.reject("e");
const a2: (() => { a: string }) & Z = async () => mk();
const a3: (() => Promise<{ c: number } | { a: string }> | string) & Z = async () => mk();

interface Effect<out A> { readonly E: { readonly _A: (_: never) => A } }
interface RO<A> { until?: (_: A) => boolean }
declare function pipe<A, B>(a: A, ab: (a: A) => B): B;
declare const r1: <A>(options: RO<A>) => (self: Effect<A>) => Effect<A>;
declare const r2: <O extends RO<A>, A>(options: O) => (self: Effect<A>) => Effect<A>;
pipe({} as Effect<boolean>, r1({ until: (x) => { const z: 1 = x; return x; } }));
pipe({} as Effect<boolean>, r2({ until: (x) => { const z: 1 = x; return x; } }));
const c1: (self: Effect<boolean>) => unknown = r2({ until: (x) => { const z: 1 = x; return x; } });
