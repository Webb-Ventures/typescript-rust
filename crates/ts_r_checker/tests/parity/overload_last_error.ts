// TS2769 reports only the last overload's error, elaborated like a single signature's
// (into array elements and object literal properties). Signatures merged from several
// interface declarations, lib files included, list the later declarations first. An arrow
// with an annotated parameter is not elaborated into its body.
interface I { (a: string): 1 }
interface I { (a: number): 2 }
declare const i: I
i(true)
declare function f(a: string): void
declare function f(a: number): void
f(true)
declare function g(a: { x: string }): void
declare function g(a: { y: string }): void
g({ y: 1 })
g({ z: 1 })
declare function h(a: string, b: number): void
declare function h(a: number): void
h(1, 2)
h("x", "y")
declare function k(a: number[]): void
declare function k(a: string[]): void
k([1, true])
declare function m(cb: (x: number) => number): void
declare function m(cb: (x: string) => string): void
m((x: string) => 1)
m((x) => true)
declare function m1(cb: (x: string) => string): void
m1((x: string) => 1)
m1((x) => 1)
declare function p(a: { n: number; s: string }): void
declare function p(a: number): void
p({ n: 1, s: 2 })
