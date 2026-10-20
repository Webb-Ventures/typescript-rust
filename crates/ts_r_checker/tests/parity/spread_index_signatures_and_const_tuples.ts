// Spreading an intersection keeps its index signatures (value types intersected), also
// through a union of intersections; spread signatures are writable.
interface Out { [h: string]: number | string | string[] | undefined; accept?: string | string[] | undefined; age?: string }
interface ReqOpts { headers?: Out | readonly string[] | undefined; agent?: number; host?: string }
type HO = Readonly<{ [name: string]: string }>
type Opts = Omit<ReqOpts, "agent"> & { headers: HO }
declare const o: Opts
declare function rm(h: { [name: string]: string }): void
const headers = { ...o.headers }
rm(headers)
declare const h3: Out & HO
const headers3 = { ...h3 }
rm(headers3)
declare const ro: { readonly [k: string]: string }
const sp = { ...ro }
sp.x = "1"
const n1: number = sp
interface O2 { accept?: string | string[] }
declare const y: O2 & { [name: string]: string }
rm({ ...y })

// A const assertion's tuple is mutable when its contextual type takes a mutable array.
type Theme = "a" | "b"
export const k1: { label: string; value: Theme }[] = [
  { label: "A", value: "a" },
] as const satisfies { label: string; value: Theme }[]
export const k2 = [1, 2] as const satisfies number[]
export const k3: number[] = [1, 2] as const
declare function f(x: string[]): void
f(["a"] as const)
export const k4 = ["a"] as const satisfies readonly string[] | string[]
const n3: number = k2
const n4: number = k4
const k5 = [1] as const
k5.push(2)

// Without return values, a function whose contextual return type takes `undefined`
// returns `undefined`.
type Prog = (done: number, total: number) => undefined
function clientProgress(): Prog {
  return (done: number, total: number) => {
    void done
  }
}
const p2: Prog = (done) => { void done }
const p4: () => undefined = function () {}
const p5: () => string | undefined = () => {}
const p6: () => number = () => {}
export function split(progress: Prog, weights: number[]): Prog[] {
  return weights.map((_, i) => {
    return (done: number, total: number) => {
      progress(done + i, total)
    }
  })
}
export { n1, n3, n4, clientProgress, p2, p4, p5, p6 }
