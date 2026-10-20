// An override that narrows a parameter of the base method.
class Base {
  accept(v: string | number): string {
    return String(v);
  }
}
class Sub extends Base {
  override accept(v: string): string { // @expect TSR0601
    return v.toUpperCase();
  }
}
export const out: string = new Sub().accept("a");
