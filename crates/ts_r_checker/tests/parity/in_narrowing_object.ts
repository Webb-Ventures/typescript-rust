declare const v: unknown
if (typeof v === 'object' && v !== null) {
  if ('offset' in v && Number.isInteger(v.offset)) {
    const a: string = v
  }
  const b: string = v
  if ('after' in v && typeof v.after === 'object') {
    const c: string = v
  }
}
declare const o: object
if ('x' in o) { const d: string = o }
declare const u: { a: number } | { b: string } | object
if ('a' in u) { const e: string = u } else { const f: string = u }
declare const a: object | (object & {x: 1}); const a1: number = a
declare const b: {a:1} | ({a:1} & {b:2}); const b1: number = b
interface I {a: 1}
declare const c: I | (I & {b: 2}); const c1: number = c
declare const v2: object
if (Math.random()) { if ("x" in v2) {} else {} ; const d: number = v2 }
let w = v2
if ("x" in w && Math.random()) {} else { const e: number = w }
