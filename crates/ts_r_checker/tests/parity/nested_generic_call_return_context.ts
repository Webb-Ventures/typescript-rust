// A generic call as an argument of another generic call sees what the outer call's
// contextual type implies for the outer type parameters (`new Promise(r => r())` inside
// `race(...)` returned from a `Promise<void>` function infers `void`).
interface Progress {
  race<T>(promise: Promise<T> | Promise<T>[]): Promise<T>
}
declare function race2<T>(promise: Promise<T>): Promise<T>
declare function race3<T>(promise: T[]): T[]
declare function mk<U>(f: (r: (u: U) => void) => void): U[]
async function a(progress: Progress): Promise<void> {
  return await progress.race(new Promise((resolve, reject) => {
    if (Math.random()) reject(1)
    resolve()
  }))
}
function d(): Promise<void> {
  return race2(new Promise((resolve) => {
    resolve()
  }))
}
const e: Promise<void> = race2(new Promise((resolve) => {
  resolve()
}))
async function g(): Promise<void> {
  const x: void = await race2(new Promise((resolve) => {
    resolve()
  }))
  return x
}
async function h(): Promise<number> {
  return await race2(new Promise((resolve) => {
    resolve(1)
  }))
}
const e4: number[] = race3(mk((r) => r(1)))
const e5: "a"[] = race3(mk((r) => r("a")))
const e6: string[] = race3(mk((r) => r(1)))

// Several naked type parameters (`TResult1 | TResult2`) each infer what the contextual
// type implies when nothing else infers them.
interface Ret { a: number; b: string }
declare function send<T extends keyof Ret>(m: T): Promise<Ret[T]>
async function all(cb: (s: number) => Promise<void>): Promise<void> {
  await cb(1)
}
async function u<T extends keyof Ret>(m: T): Promise<void> {
  await all(s => send(m).then())
}
declare const p: Promise<number>
const w: Promise<void> = p.then()
const w2: Promise<string> = p.then(x => x)
declare function two<A = number, B = never>(): Promise<A | B>
const y: Promise<void> = two()
export { a, d, e, g, h, e4, e5, e6, u, w, w2, y }
