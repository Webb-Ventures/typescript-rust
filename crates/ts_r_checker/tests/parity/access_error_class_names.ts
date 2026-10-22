// TS2341/TS2445 print the declaring class's declared type (type parameters included);
// TS18013 names the class symbol, a class expression after its variable.
class D<T> {
  private x = 1
  protected z: T | undefined
}
const C = class {
  #m() { return 1 }
  static make() { return new C() }
}
declare const d: D<string>
d.x
d.z
C.make().#m
export {}
