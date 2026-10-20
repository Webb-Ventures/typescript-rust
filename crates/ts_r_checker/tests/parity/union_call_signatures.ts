// trpc `node-http` (`req.on("data", ...)` on `IncomingMessage | Http2ServerRequest`): a union
// of overload lists keeps the signatures every member has; with none, one member's overloads
// combine with the others' single signatures. Not-callable messages name the constituents.
interface A {
  on(event: "close", listener: () => void): A;
  on(event: "data", listener: (chunk: string) => void): A;
  on(event: string | symbol, listener: (...args: any[]) => void): A;
}
interface B {
  on(event: "close", listener: () => void): B;
  on(event: "data", listener: (chunk: any) => void): B;
  on(event: "pause", listener: () => void): B;
  on(event: string | symbol, listener: (...args: any[]) => void): B;
}
declare const x: A | B;
const r1 = x.on("data", (c: string) => {});
const r2 = x.on("close", () => {});
const r3 = x.on("data", (c) => {
  const n: number = c;
});
const t1: null = r1;
const t2: null = r2;
interface C {
  f(a: string): 1;
  f(a: number): 2;
}
interface D {
  f(a: boolean): 3;
  f(a: number): 4;
}
declare const y: C | D;
y.f(1);
y.f("s");
interface E {
  g(a: string): 1;
  g(a: number): 2;
}
interface F {
  g(a: boolean): 3;
  g(a: symbol): 4;
}
declare const z: E | F;
z.g(1);
declare const v: ((a: string) => void) | { (a: number): 1; (a: boolean): 2 };
v(1);
declare const a: (() => void) | number;
a();
declare const o: { m: (() => void) | { x: 1 } | string; s: string };
o.m();
o["s"]();
o.s.length();
declare const c: (new () => 1) | number;
new c();
declare const d: { new (a: string): 1; new (a: number): 2 } | { new (a: boolean): 3; new (a: symbol): 4 };
new d(1);
(o.s)();
declare const q: { a: number } | { a: string };
q.a();
export {};
