// Evolving arrays and auto types (noImplicitAny): `let xs = []` takes the element types
// pushed before each read (`push`, `unshift`, `xs[n] = v`); a read where nothing was pushed
// is `any[]` with TS7005 and TS7034. `let x = null` / `= undefined` is auto-typed like
// `let x;`: assignments decide its type. A closure reading an auto variable that is
// assigned after it reports TS7005/TS7034.
function f1() {
  let a = []
  a.push(1)
  a.push("x")
  const s: boolean = a
}
function f2() {
  const a = []
  a[0] = 1
  const s: boolean = a
}
function f3() {
  let a = []
  for (const x of [1, 2]) a.push(x)
  return a
}
const r3: boolean = f3()
function f4(c: boolean) {
  let a = []
  if (c) a.push(1)
  else a.push("s")
  const s: boolean = a
}
function f5() {
  let a = []
  a.unshift(true)
  const n = a.length
  const s: string = a
}
function f6() {
  let a = []
  const s: string = a
}
function f7() {
  let a = []
  a.push(1)
  a = ["x"]
  const s: boolean = a
}
function f8() {
  let a = []
  const b = a.map(x => x)
  a.push(1)
}
function f9() {
  let a: any[] = []
  a.push(1)
  const s: string = a
}
function h1() {
  let a = []
  function inner() { return a }
  a.push(1)
  return inner
}
function h4() {
  let a = []
  const b = a
  const c = a
  a.push(1)
  return a
}
function h6() {
  let a = []
  a.length
  a[0] = "x"
  for (const v of a) { const s: number = v }
}
function h7() {
  let a = []
  const f = () => a.push(1)
  return a
}

function g1() {
  var a = null
  a = 1
  const s: string = a
}
function g2() {
  let a = null
  const s: string = a
}
function g3() {
  let a = undefined
  if (Math.random()) a = "x"
  const s: number = a
}
function g4() {
  let a = null
  a = { x: 1 }
  a.x
  a = "s"
  return a
}
const r4: number = g4()
function g5(c: boolean) {
  let a = null
  if (c) a = 1
  return a
}
const r5: string = g5(true)
export let e1 = null
const z1: string = e1

function c1() {
  let x
  const f = () => x
}
function c2() {
  let x
  const f = () => x
  x = 1
}
function c3() {
  let x
  x = 1
  const f = () => x
}
function c4() {
  var x
  const f = () => x
  x = "s"
}
