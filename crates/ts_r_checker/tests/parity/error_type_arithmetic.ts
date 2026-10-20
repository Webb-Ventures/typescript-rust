// Arithmetic on the error type (an unresolved import): `*`, `-`, `/`, `%`, `**`, shifts and
// bitwise operators give `number` (`bigint` next to a bigint), `+` gives `string` next to a
// string and the error type otherwise.
import { DAY } from 'missing-module'
export const a = 7 * DAY
export const b = DAY - 1
export const c = DAY + 's'
export const d = 's' + DAY
export const e = DAY + 1
export const f = DAY * 7n
export const g = DAY << 2
export const h = DAY ** DAY
a.foo
b.foo
c.foo
d.foo
e.foo
f.foo
g.foo
h.foo
