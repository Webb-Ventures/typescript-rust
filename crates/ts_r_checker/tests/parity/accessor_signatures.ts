// Accessor signatures in interfaces and type literals: reads have the getter's type, writes
// the setter parameter's (as lib.dom's `Window.location`).
interface Loc {
  href: string
}
interface W {
  get location(): Loc
  set location(href: string | Loc)
  get only(): number
  set only(v: number)
}
declare const w: W
const a: number = w.location
const b: string = w.location.href
w.location = "x"
w.location = { href: "y" }
w.location = 1
const c: string = w.only
w.only = "s"
declare const t: { get v(): string; set v(x: string | number) }
t.v = 1
t.v = true
const d: number = t.v
class C {
  get p(): string {
    return ""
  }
  set p(v: string | number) {
    void v
  }
}
new C().p = 1
new C().p = false
export { a, b, c, d }
