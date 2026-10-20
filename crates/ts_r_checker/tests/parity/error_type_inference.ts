// The error type infers itself into `infer` positions: `Parameters<E>` and `ReturnType<E>`
// of an error-typed `E` are the error type. An unresolved type name is different: it
// infers nothing, so `Parameters<Missing>` is `unknown[]`.
import { withRollback } from "not-installed"
type P = Parameters<typeof withRollback>
type P1 = Parameters<P[1]>[0]
type R = ReturnType<typeof withRollback>
type N = NonNullable<typeof withRollback>
export const p: P = true
export const p1: P1 = true
export const r: R = true
export const n: N = true
export const kp: keyof P = true
type M = Parameters<Missing>
export const m: M = true
export const call = (x: P1) => x.foo
type A = Awaited<typeof withRollback>
export const ka: keyof A = true
export async function g() {
  const [a, b] = await Promise.all([withRollback(), withRollback.x])
  return [a.map((x) => x), b.map((y) => y)]
}
