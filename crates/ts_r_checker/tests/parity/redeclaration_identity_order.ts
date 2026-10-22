// TS2403 needs identical types: overloads pair up by position, a generic mapped type is
// never identical to a non-generic one, and inferred types (`this` arrays, both index
// signatures for a `string | number` key, an enum call result, a callback whose parameter
// shadows the variable) are compared like any other.
interface A {
  (x: { s: string }): string;
  (x: { n: number }): number;
}
interface B {
  (x: { s: string }): string;
  (x: { n: number }): number;
}
interface C {
  (x: { n: number }): number;
  (x: { s: string }): string;
}
declare var v: A;
declare var v: B;
declare var w: A;
declare var w: C;

class K {
  self = this;
  f() {
    var b: this[];
    var b = [this, this.self, null, undefined];
  }
}

class P { a!: number }
class Q extends P { b!: number }
declare var both: { [n: string]: P; [m: number]: Q };
declare var key: string | number;
var x3 = both[key];
var x3: P;

enum E { A }
declare function f15(x: string): string;
declare function f15(x: E): E;
var r4 = f15(E.A);
declare function f16(x: Object): Object;
declare function f16(x: E): E;
var r4 = f16(E.A);

var n: number;
declare function fn5(f: (n: string) => void): string;
declare function fn5(f: (n: number) => void): number;
var n = fn5((n) => n.length);

type Deep<T> = { [K in keyof T]?: Deep<T[K]> };
function g<T>() {
  var d: Deep<any>;
  var d: Deep<T>;
}
