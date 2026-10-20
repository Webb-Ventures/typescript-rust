// A call's `this` argument (the receiver, or `void`) against a declared `this`
// parameter (TS2684), in overloads, through union signatures, and `this` types in
// signature relations.
declare class Stream<T> {
  constructor(f: (s: T) => void);
  sub: (f: (t: T) => void) => void;
  get v(): T;
  w: (t: T) => void;
  flat<V>(this: Stream<Source<V>>): Stream<V[]>;
  flat2: {
    <V>(this: Stream<Source<V>>): Stream<V[]>;
    <V, R>(this: Stream<Source<V>>, project: (...values: V[]) => R): Stream<R>;
    <R>(this: Stream<Source<any>>, project: (...values: any[]) => R): Stream<R>;
  };
}
type Source<T> = Stream<T> | PromiseLike<T> | Iterable<T>;
declare const outer: Stream<Stream<number>>;
export const r1 = outer.flat();
export const r2 = outer.flat2();
export const r3 = outer.flat2((a, b) => a + b);
new Stream<number>(() => {}).flat();

declare function f(this: { a: number }, x: number): void;
f("x");
f(1);
declare const o: { a: string; f: typeof f };
o.f("x");
o["f"](1);
(o.f)(1);
o.f!(1);
declare const oo: { a: number; f: typeof f } | undefined;
oo?.f(1);
declare const wrong: { b: number; f: typeof f };
wrong.f(1);

declare function g(this: { a: number }, x: number): void;
declare function g(this: { a: string }, x: string): void;
declare const o2: { a: boolean; g: typeof g };
o2.g(1);

type A = { a: string };
type B = { b: number };
declare const u1: ((this: A) => void) | ((this: B) => void);
u1();
declare const u2: ((this: void) => void) | ((this: number) => void);
u2();
interface Two {
  (this: A & B & { c: 1 }): void;
  (this: A & B): void;
}
declare const u3: ((this: A & B) => void) | Two;
u3();

class C {
  n = 1;
}
class D extends C {
  d = 2;
}
declare let fc: (this: C, m: number) => number;
declare let fd: (this: D, m: number) => number;
fc = fd;
fd = fc;
declare let mc: { m(this: C): void };
declare let md: { m(this: D): void };
mc = md;
md = mc;
declare let fv: (this: void, m: number) => number;
declare let fy: (this: { y: number }, m: number) => number;
fy = fv;
fv = fy;
