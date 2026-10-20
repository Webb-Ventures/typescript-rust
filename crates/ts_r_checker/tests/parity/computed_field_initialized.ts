// A computed field name assigned in the constructor under another spelling of the same key
// (`this.p`, `this['p']`, `this[16]`, a const naming it) is initialized; an unassigned one is
// still TS2564, and so is `[Symbol.iterator]` (its name is not a literal). `[sy]` and
// `this[sy]` name one constant.
const k = "kk";
const j = "jj";
declare const u: unique symbol;
const sy = Symbol();
export class C {
  ["p"]: number;
  ["q"]: number;
  ['r']: number;
  [1]: number;
  [0x10]: number;
  [k]: number;
  ["jj"]: number;
  [u]: number;
  [sy]: number;
  [Symbol.iterator]: number;
  ["s"]: number;
  constructor() {
    this.p = 1;
    this['q'] = 1;
    this["r"] = 1;
    this[1] = 1;
    this[16] = 1;
    this.kk = 1;
    this[j] = 1;
    this[u] = 1;
    this[sy] = 1;
    this[Symbol.iterator] = 1;
  }
}
