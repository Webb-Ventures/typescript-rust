// An `infer` with nothing inferred falls back to an implied constraint: `unknown[]` in a rest
// position (`Parameters<any>`), the constraint of the type parameter it is passed to
// (private-app apps/web: `...[error, request]: Parameters<Unresolved>`).
import type { Instr } from 'nope'
export function f(...[e, r]: Parameters<Instr.onErr>) { return [e, r.method] }
declare const a: Parameters<any>
export const b: string = a
type R<T> = T extends [infer H, ...infer Rest] ? Rest : never
declare const c: R<any>
export const d: string = c
type F<T> = T extends (a: infer A, ...r: infer Q) => void ? [A, Q] : never
declare const e: F<any>
export const g: string = e
type Box<X extends string> = { x: X }
type H<T> = T extends Box<infer U> ? U : never
declare const h: H<any>
export const i: number = h
type S3<T> = T extends (...args: [any, ...infer R]) => void ? R : never
declare const j: S3<(a: string, b: number) => void>
export const k: number = j
import { withR } from 'nope2'
type A3 = Parameters<typeof withR>
export const a3: string = null! as A3
