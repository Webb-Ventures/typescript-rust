// An unresolved type reference is an error type printed as written, with its type
// arguments resolved (private-app apps/api, apps/web: `Pick<z.infer<any>, ...>`,
// `Pick<Problem, ...>`). Unions and intersections with it print as `any`; indexing it gives it.
import type { Problem } from "not-installed"
import { z } from "zod"
import { val } from "not-installed"
namespace N { export type Q1 = 1 }
declare const env: Missing
type Env = z.infer<typeof env>
declare const v: Problem
declare const t: [Problem]
declare function r(): Problem
declare function id<T>(x: T): T[]

declare function f1(p: Pick<Problem, "a" | "b">): void
f1({ a: 1 })
declare function f2(p: Pick<Env, "a" | "b">): void
f2({ a: 1 })
declare function f3(p: Pick<Foo<string, number>, "a">): void
f3({})
declare function f4(p: Pick<z.Bar, "a">): void
f4({})
declare function f5(p: Pick<N.Q2, "a">): void
f5({})
declare function f6(p: Pick<Problem | string, "a">): void
f6({})
declare function f7(p: Pick<Problem & { x: 1 }, "a">): void
f7({})
declare function f8(p: Pick<Problem["x"], "a">): void
f8({})
declare function f9(p: Pick<typeof v, "a">): void
f9({})
declare function f10(p: Record<"x", Problem>): void
f10(1)
declare function f11(p: Partial<Problem>): void
f11(1)
declare function f12(p: Problem | undefined): void
f12(1)

const p1: Pick<Problem, "a"> = { a: 1 }
const n1: number = p1.a
const s1: string = p1
const s2: string = t
const s3: string = r
const s4: string = [val]
const s5: string = id(v)
const s6: string = { x: v }
const s7: string = [v]
const s8: string = { x: v } as { x: Problem | undefined }
const s9: string = { x: v } as { x: Problem[] | number }
let w = v
const s10: string = [w]
let u1: Problem = 1
