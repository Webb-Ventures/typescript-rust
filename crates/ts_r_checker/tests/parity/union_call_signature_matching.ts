// Union call signatures: a member signature every other member accepts stands for the
// union; strict subtype reduction keeps an optional parameter over a required one.
type F1 = (a: string, b?: string) => void
type F2 = (a: string, b?: string, c?: string) => void
type F5 = (a: string, b: string) => void
declare const f12: F1 | F2
f12("a", "b", "c")
declare const f125: F1 | F2 | F5
f125("a")
f125("a", "b", "c")
function f3(x: () => void, y: (x?: string) => void) {
  const f = Math.random() > 0.5 ? x : y
  f()
  f("hello")
}
function f6(x: (x: "hello" | undefined) => void, y: (x?: string) => void) {
  const f = Math.random() > 0.5 ? x : y
  f()
  f("hello")
}
interface A {
  (this: void, b?: number): void
}
interface B {
  (this: number, b?: number): void
}
interface C {
  (i: number): void
}
declare const abc: A | B | C
abc(0)
declare const rest: ((a: string, ...b: number[]) => string) | ((a: string) => number)
rest()
void [f3, f6]
