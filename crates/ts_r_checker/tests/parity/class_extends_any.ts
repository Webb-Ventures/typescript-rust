// A class extending an any value inherits a string index signature of any.
declare const base: any;
const C = class extends base {
  constructor(a: number) {
    super(a);
  }
  own = 1;
};
class D extends base {}
declare const k: string;
export function f() {
  C.prototype[k] = 1;
  const c = new C(1);
  const x: number = c[k];
  const y: string = c.foo;
  const d = new D();
  const z: string = d[k];
  const w: string = d.own2;
  const v: string = c.own;
}
