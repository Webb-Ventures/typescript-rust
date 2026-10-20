// TS2347 for type arguments on a call of an `any` or `Function` value, but not on the error
// type of an unresolved import (private-app domains/discovery: Kysely's `eb.fn.countAll<number>()`
// with `db: any`).
import { err } from 'nope'
declare const a: any
declare const fn: Function
a<number>()
a.b.c<string>(1)
new a<number>()
err<number>()
err.x<number>()
fn<number>()
export function f(p) { return p.q<number>() }
a.select((eb) => eb.fn.countAll<number>())
declare function takes(cb: (x: <T>(p: T) => T) => void): void
takes((x) => x<number>(1))
