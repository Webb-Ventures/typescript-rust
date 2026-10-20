// The error type is a subtype of nothing but any/unknown: overload resolution's subtype pass
// skips `(n: number)` for an error-typed argument, as for `any`.
import { err } from "nope"
declare const a: any
declare function q(n: number): "num"
declare function q<T extends object>(b: T): "gen"
declare function v(n: number): "num"
declare function v<T>(b: T): "gen"
const r1: 1 = q(err)
const r2: 1 = q(a)
const r3: 1 = v(err)
const r4: 1 = new Uint8Array(err)
const r5: 1 = new Uint8Array(a)
const r6: 1 = Math.max(err)
export {}
