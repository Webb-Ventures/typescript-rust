// `x satisfies T` with an unresolved `T` is the error type: no errors on its members.
import type { Plugin } from "not-installed"
const m = { a: 1 } satisfies Plugin
m.zzz
const n = { a: 1 } satisfies Missing
n.zzz
const o = { a: 1 } satisfies { a: number }
o.zzz
export {}
