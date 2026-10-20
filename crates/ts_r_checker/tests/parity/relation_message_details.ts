// Relation message details: wrapper objects (TS2692), the `Object` type (TS2696),
// construct signatures (TS2658), unions against type parameters, constraints of generic
// sources against unions, literal widening against `void` and string mappings, alike names
// (TS2719), index signature keys (TS2330), numeric property names as written, class
// expressions named by their variable, `boolean` and enum receivers of TS2339.
declare let n: Number;
let a1: number = n;
declare let o: Object;
let a2: () => void = o;
let a3: { x: number } = o;
let a4: new () => void = () => {};
function f1<T, U>(t: T | U, k: T) { k = t; }
function f2<V extends string, W extends Top, Top>(v: V, w: W) {
    let x: number | boolean = v;
    let y: Top | number = w;
}
declare let v: void;
v = 5;
let a5: Uppercase<string> = "abc";
function f3<T>(x: T) {
    return function <T>(y: T) { x = y; };
}
function f4<T>(): T { return; }
interface I { [s: string]: boolean; [s: number]: boolean }
let a6: I = { [+"x"]: "" };
interface J { 2.0: string }
interface K extends J { 2: number }
const C = class { p = 1 };
let a7: { q: number } = C;
declare let b: boolean;
b.foo;
enum E { A, B }
function f5<X extends E>(x: X) { x.foo; }
