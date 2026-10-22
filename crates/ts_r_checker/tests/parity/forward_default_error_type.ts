// Type parameters not supplied yet map to the error type while defaults are filled in, so
// a default that refers to a later type parameter is a silent error type there.
interface A { a: number }
interface B { b: number }
interface C { c: number }
declare const a: A;
declare const b: B;
declare const c: C;
declare function f14<T, U = V, V = C>(a?: T, b?: U, c?: V): [T, U, V];
f14<A>(a, b);
f14<A>(a, b, c);
f14<A>(a, b, b);
type Box<T, U = W, W = string> = { t: T; u: U; w: W };
const x: Box<number> = { t: 1, u: "anything", w: "s" };
const y: Box<number> = { t: 1, u: 2, w: 3 };
