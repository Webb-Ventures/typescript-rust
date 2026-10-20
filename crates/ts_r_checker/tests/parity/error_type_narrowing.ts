// The error type (an unresolved import) narrows like `any`: by type predicates, `typeof`
// and `instanceof`.
import { M } from 'missing-module'
type R = 'a' | 'b'
declare function isR(v: string): v is R
const [d] = M.slice(2)
const e = M
export function f1(): R {
  if (!isR(d)) return 'a'
  d.nope1
  return d
}
export function f2() {
  if (typeof e === 'number') e.nope2
  if (e instanceof Date) e.nope3
  if (isR(e)) e.nope4
  return e.fine
}
