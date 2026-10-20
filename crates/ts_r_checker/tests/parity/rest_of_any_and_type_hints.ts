// The rest element of an `any` is `any` (so `rest.map(e => ..)` is untyped), missing-types
// hints for type names, and an indexed-access union keeps its alias (private-app
// domains/billing, packages/images, packages/providers).
import { Fake } from 'nope'
const p = new Fake()
const [d, ...rest] = p.events
export const a = rest.map((e) => e.id)
declare const an: any
const [d2, ...rest2] = an
export const b = rest2.map((e) => e.id)
export { d, d2 }
export type A = Buffer
export type B = require
export type C = describe
type M = { a: string; b: number }
type AB = M[keyof M]
declare const x: AB
export const y: boolean = x
declare const u: Uint8Array
export const z: boolean = u
export interface P1 extends number {}
export class P2 implements string {}
export type P3 = numbr
export type P4 = Map<string, number>
