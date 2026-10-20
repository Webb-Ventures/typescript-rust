// A union written with an aliased union prints the way it was written
// (private-app apps/web: `HeadersInit | undefined`; packages/providers:
// `BodyInit | null | undefined`): other members first, then the aliases; `null` and
// `undefined` last. Removing members keeps the aliases that survive whole.
type A = { a: 1 } | { b: 2 }
type L = "x" | "y"
type B = A | undefined
type N = number | undefined
interface I { p?: A; r: A | null; s?: A | null; u: A | L }
declare const i: I
const s1: number = i.p
const s2: number = i.r
const s3: number = i.s
const s4: number = i.u
declare let v1: A | undefined
const s5: number = v1
if (v1) { const s6: number = v1 }
const s7: number = v1!
declare function f(x?: A): void
const s8: number = f
declare let v3: A | string
const s9: number = v3
declare let v4: (A | undefined)[]
const s10: number = v4
declare let v5: A | A
const s11: number = v5
declare let v6: A | { a: 1 }
const s12: number = v6
declare let v7: B | null
const s13: number = v7
declare let v8: N | string
const s14: number = v8
let v9 = Math.random() ? v1 : v3
const s15: number = v9
const s16: { a: 1 } = v1

// Elaboration picks the member sharing the most properties with the source, counting
// index signatures on either side; a source sharing none is not elaborated.
type Init = [string, string][] | Record<string, string> | { append(): void }
declare const x: { [k: string]: unknown }
const h1: Init | undefined = x
declare const z: { q: 1 }
const h2: string[] | { b: 1 } = z
declare const w: { [k: string]: number }
const h3: string[] | { b: 1 } = w
declare const e: {}
const h4: string[] | { b: 1 } = e
declare const fn: () => void
const h5: string[] | { b: 1 } = fn
const h6: ((n: number) => number) | ((n: number) => string) = (n: number) => true
