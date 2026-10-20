// TS2564 for computed names and for properties only destructured from `this`; inherited
// properties against index signatures (TS2411/TS2413); enum members absorbed by `number`.
declare const s: string;
class K {
  [s]: number;
  ["a"]: number;
  [Symbol.iterator]: number;
  b: number;
  constructor() {
    this["b"] = 1;
  }
}
class C2 {
  x: string;
  y: string;
  z: string;
  constructor() {
    let { x, y: y1 } = this;
    ({ x, y: y1 } = this);
    ({ a: this.z } = { a: "" });
  }
}

interface A {
  [s: string]: { a: number };
}
interface B {
  [s: number]: { a: number; b: number };
}
interface C {
  m: {};
}
interface D extends A, B, C {}
interface E {
  0: {};
}
interface F extends A, B, E {}
interface H extends A, F {}
interface P {
  a: { n: number };
  1: string;
}
interface Q extends P {
  [n: number]: number;
}

enum e { e1, e2 }
declare const u: e | number;
export const t: string = u;
// `var p = this` has the `this` type: TS2403 against the class type.
class K2 {
  m(t = this) {
    var t: K2;
    var p = this;
    var p: K2;
  }
}
export { K, C2, K2 };
