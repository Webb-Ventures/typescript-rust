// TS2365 names compound operators as written and widens operand literals (except when
// mixing number and bigint with `+`); a missing member of `this` names the class type.
declare let n: number;
declare let b: bigint;
declare let o: {};
export const c1 = 1 + o;
export const c2 = n + false;
export const c3 = b + 2;
export const c4 = b - 2;
b += 2;
b -= 2;
n += 1n;
n *= 1n;
export const c5 = true + 1;

export class Box<T> {
  value?: T;
  read() {
    return this.missing;
  }
}
export class Plain {
  run() {
    this.nope = 1;
  }
}
