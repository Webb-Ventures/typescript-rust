// @target: es2022
// @useDefineForClassFields: false
export class A {
  m() {}
  static s = 2;
  x = 1;
  static #q = 3;
  static { console.log(1); }
  static t = A.s;
  static [Symbol.iterator] = 5;
}
