// A generic spread stays generic: `{ ...t }` is `T`, `{ ...t, a }` is `T & { a }`.
export function f<T extends { lastmod: Date }>(r: T) {
  const a: number = { ...r }
  const b: number = { ...r, lastmod: 1 }
  const c: number = { x: 1, ...r }
}
export function g<T>(r: T, u: T | undefined) {
  const a: number = { ...r }
  const b: number = { ...r, lastmod: 1 }
  const c: number = { ...u, ...r }
}
export const dated = <T extends { lastmod: Date }>(rows: T[]) =>
  rows.map((r) => ({ ...r, lastmod: String(r.lastmod) }))
const z: number = dated
