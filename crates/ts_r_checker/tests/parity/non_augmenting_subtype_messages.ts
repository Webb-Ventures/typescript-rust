class Foo {
    x = "";
}
interface I extends Foo {
    y: number;
}
interface J {
    y: number;
}
interface K extends J {}
interface L extends Foo {}
class E extends Foo {}
class Q extends Foo { z = 1; }
class S extends Foo { static s = 1; }
class G<T> extends Foo { t?: T; }
class H extends E {}
class Impl extends Foo implements I {}
declare let e: E;
declare let q: Q;
declare let st: S;
declare let g: G<number>;
declare let h: H;
let i1: I = e;
let i2: I = q;
let i3: J = e;
let i4: I = g;
let i5: I = h;
let i6: I = st;
let k1: K = {};
let l1: L = {};
export { Impl };
