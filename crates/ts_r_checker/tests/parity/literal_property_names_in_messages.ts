// A property declared with a literal name is named as written: quotes kept for strings,
// the spelling kept for numbers (TS2416, TS2325 and TS2717).
class A {
  '2.0': number = 1
  public '1': number = 1
}
class B extends A {
  '2.0': string = ""
}
class C extends A {
  private '1': number = 1
}
declare const v: {
  "1": number
  1.0: string
}
export { B, C, v }
