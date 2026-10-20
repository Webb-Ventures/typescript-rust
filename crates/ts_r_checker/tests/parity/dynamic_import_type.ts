// `import("./m")` of a TypeScript module is `Promise<typeof import("./m")>`.
// @filename: m.ts
export const n = 1
export function f(): string {
  return "x"
}
export default { d: true }
// @filename: a.ts
const load = async () => import("./m")
export async function g() {
  const { n, f } = await load()
  const s: string = n
  const t: number = f()
  const m = await import("./m")
  const u: number = m.default.d
  return [s, t, u]
}
