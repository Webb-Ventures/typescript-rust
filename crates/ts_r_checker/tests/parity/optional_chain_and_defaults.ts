// `typeof ref?.a === "string"` makes `ref` non-nullish (providers stripe.ts); a parameter
// with an initializer accepts `undefined` from callers (providers payments.test.ts); a
// function returning a non-boolean implies no type predicate.
interface SO { [key: string]: unknown }
declare const object: SO | undefined
if (typeof object?.id === "string") { const s: string = object.id }
if (typeof object?.id !== "string") {} else { const s: string = object.id }
declare const o2: { id: unknown } | undefined
if (typeof o2?.id === "string") { const s: string = o2.id }
if (typeof o2?.id === "undefined") { o2.id } else { o2.id }
if (typeof o2?.id !== "undefined") { o2.id } else { o2.id }
class F {
  complete(ref: string, options: { paid?: boolean } = {}): void {}
}
declare const o: { paid?: boolean } | undefined
new F().complete("a", o)
function g(x: number = 1) { const y: string = x }
g(undefined)
declare const n: number | undefined
g(n)
const h = (x: { a?: 1 } = {}) => x
h(o as { a?: 1 } | undefined)
const s: string = g
h(undefined)
function k(x = 1, y: string) {}
const s3: string = k
k(undefined, "a")
