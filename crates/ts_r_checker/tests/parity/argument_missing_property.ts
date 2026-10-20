// A missing-property error replaces an argument's TS2345 head when it is reported for the
// argument and parameter types themselves (not for a member of a union parameter).
interface A { a: string; b: number; c: 1 }
interface B { b: number }
interface C { a: string; c: 1 }
declare function f(x: A): void
declare const b: B
declare const c: C
f(b)
f(c)
declare function h(x: { o: A }): void
h({ o: b })
new Promise<A>((res) => res(b))
declare function k(...x: A[]): void
k(b)
declare function u(x: A | number): void
u(b)
