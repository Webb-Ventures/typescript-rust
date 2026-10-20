// The rest of an object binding pattern (private-app domains/moderation:
// `rows.map(({ a, b, ...r }) => record(r))` on untyped rows gives `{ [x: string]: any; }`,
// TS2740 against `Row`): the remaining public properties without `readonly`, the index
// signatures, no signatures; class instances also lose methods and accessors; unions
// distribute. Generic parents (tsc: `Omit<T, "a">`) are not modelled.
interface Row { id: string; kind: string }
declare function rec(r: Row): void
declare const rows: any
rows.map(({ a, b, ...r }) => rec(r))
rows.map(({ ...r }) => rec(r))
function f1({ a, ...r }) { rec(r); return r }
const g1: string = f1
function f2({ a, ...r }: any) { rec(r) }
function f3({ a: { b, ...q }, ...r }) { rec(q); rec(r) }

declare const o: { a: 1; b: string; c?: number; readonly d: 1; m(): void; [k: string]: unknown }
const { a, ...r1 } = o
const t1: number = r1
const { ...r6 } = o
const t6: number = r6
const k = "b"
const { [k]: kb, ...r8 } = o
const t8: number = r8
class C { x = 1; private p = 2; protected q = 3; m() {} get g() { return 1 } set s(v: number) {} }
declare const c: C
const { x, ...r2 } = c
const t2: number = r2
declare const u: { a: 1; b: 2 } | { a: 3; c: 4 }
const { a: ua, ...r3 } = u
const t3: number = r3
declare const inter: { a: 1 } & { b: 2 }
const { a: ia, ...r10 } = inter
const t10: number = r10
declare const opt: { a?: 1; b?: 2 }
const { a: oa, ...r11 } = opt
const t11: number = r11
function f9({ a, ...r9 }: { a: 1; b: 2; c: { d: 1 } }) { const t9: number = r9 }

// The rest of a tuple ending in a rest element is an array; an array binding pattern
// types an array literal initializer as a tuple.
declare const tt1: [number, ...string[]]
const [a1, ...s1] = tt1
const n1: number = s1
declare const tt2: [number, string, ...boolean[]]
const [a2, ...s2] = tt2
const n2: number = s2
declare const tt3: [number, ...string[], boolean]
const [a3, ...s3] = tt3
const n3: number = s3
declare const tt6: [x: number, ...rest: string[]]
const [a6, ...s6] = tt6
const n6: number = s6
const [a10, b10] = [1, "x"]
const n10: number = a10
const n11: number = b10
const [a12, ...s12] = [1, "x", true]
const n12: number = s12
const [[a13]] = [[1, "x"]]
const n13: string = a13
