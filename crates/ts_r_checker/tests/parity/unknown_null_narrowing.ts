// `unknown` narrowed by truthiness or a null/undefined check becomes `{}` (with the other
// nullish value), and `typeof === "object"` then gives `object` (observability logger.ts).
declare const v: unknown
if (v) { const a: number = v }
if (typeof v === "object") { const a: number = v }
if (v !== null && typeof v === "object") { const a: number = v }
if (v !== null) { const a: number = v }
if (v !== undefined) { const a: number = v }
if (v != null) { const a: number = v }
if (v === null) {} else { const a: number = v }
if (v && typeof v === "object") { const a: number = v }
if (v !== undefined && typeof v === "string") { const a: number = v }
if (v !== null && typeof v === "function") { const a: number = v }
if (v != null && typeof v !== "object") { const a: number = v }
function f(value: unknown, seen: WeakSet<object>) {
  if (value === null || typeof value !== "object") return value
  seen.has(value)
  const a: number = value
}
declare const x: {}
if (typeof x === "object") { const a: number = x }
if (typeof x === "function") { const a: number = x }
const b: number = x
declare const y: {} | undefined
if (typeof y === "object") { const a: number = y }
declare const z: { a: 1 }
if (typeof z === "object") { const a: number = z }
