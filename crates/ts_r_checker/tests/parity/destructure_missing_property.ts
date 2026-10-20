// Destructuring a property that does not exist (of `unknown`, or of an object type without
// it) gives the error type: arithmetic on it is `number`, `keyof` it is `keyof any`.
export async function f(u: unknown, p: Promise<unknown>, o: { a: number }) {
  const { id } = u
  const { q } = await p
  const { b } = o
  const n: string = id * 2
  const k: keyof typeof q = true
  const m: string = b * 2
  return [n, k, m]
}
