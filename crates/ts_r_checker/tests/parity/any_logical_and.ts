// `&&` keeps an `any` or `unknown` left operand in its result type: the definitely-falsy
// part of `any` is `any` (and of `unknown`, `unknown`), so `row && make(row)` is `any`.
declare const a: any
declare const u: unknown
declare function make(x: unknown): { x: number }
const r1 = a && make(a)
const r2 = u && make(u)
let r3 = make(1)
const r4 = (r3 &&= a)
r1.foo
r2.foo
r4.foo
