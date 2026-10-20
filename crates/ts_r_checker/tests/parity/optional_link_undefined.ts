// An optional link adds `undefined` to the chain's type only when its object may be
// `null` or `undefined`.
interface I { a: 1; f(): number; g?: () => string; n: { m: string } | null }
declare const one: I
declare const maybe: I | undefined
declare const nul: I | null
declare const items: I[]
declare const fn: () => I
declare const an: any
declare function gen<T>(t: T): void
const s1: string = one?.a
const s2: string = maybe?.a
const s3: string = nul?.n?.m
const s4: string = one?.n?.m
const s5: string = one?.f()
const s6: string = one.g?.()
const s7: string = items[0]?.n
const s8: string = fn?.().a
const s9: string = one?.["a"]
const s10: string = maybe?.n!.m
const s11: number = an?.x
function t1<T extends I | undefined>(t: T) { const x: string = t?.a; return x }
function t2<T extends I>(t: T) { const x: string = t?.a; return x }
function t3<T>(t: T, k: T[]) { const x: string = k[0]?.toString(); return x }
if (maybe) { const s12: string = maybe?.a }
const s13: string = (one?.n)?.m
gen(one?.a)
export {}
