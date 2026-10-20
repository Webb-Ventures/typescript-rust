// TS2339 for properties an object binding pattern reads that its parent type lacks, and a
// binding pattern parameter contextually typed by a declared `unknown` (private-app
// apps/web, packages/ui).
import type { Base } from 'nope'
export interface P extends Base<number> { v?: string }
export function f({ v, className, ...rest }: P) { return [v, className, rest] }
declare const un: { a: number } | { b: string }
declare const s: string
declare const o: object
declare const unk: unknown
export function g1<T extends { a: number }>({ a, b }: T) { return [a, b] }
export function g3({ a, b }: { a: number } = { a: 1 }) { return [a, b] }
const { a: a1, zz } = { a: 1 }
const { a: a2, withDefault = 1 } = { a: 1 }
const { length, foo } = s
const { x1 } = o
const { a: a4, b: b4 } = un
for (const { q } of [{ a: 1 }]) { q }
export const arr = ({ m }: { n: number }) => m
const { x8 } = unk
declare const u: { a: number } | undefined
const { a: a5 } = u
type X = { kind: 'a'; a: string } | { kind: 'b'; b: string }
export function narrowed<T extends X>(value: T) {
  if (value.kind === 'a') {
    const { a } = value
    return a
  }
  return ''
}
const xs = [1, 2].map(({ toFixed, nope }) => nope)
declare function cu(f: (x: unknown) => void): void
cu(({ data }) => data)
cu(([p0, p1]) => p0)
declare const p: Promise<unknown>
p.then(({ data, error }) => [data, error])
const fn: (x: unknown) => void = ({ data }) => data
export { a1, a2, zz, withDefault, length, foo, x1, a4, b4, x8, a5, xs, fn }
