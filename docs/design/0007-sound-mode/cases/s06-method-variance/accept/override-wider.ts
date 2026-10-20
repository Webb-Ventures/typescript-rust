// An override may widen a parameter.
class Base {
  accept(v: string): string {
    return v;
  }
}
class Sub extends Base {
  override accept(v: string | number): string {
    return String(v);
  }
}
const b: Base = new Sub();
export const out: string = b.accept("a");
