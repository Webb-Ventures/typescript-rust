declare function k(x: number): string;
let a: number;
let b: number;
let c: number;
let d: number;
let e: number;
class A {
    [(a = 1, "m")]() {}
    [(b = 1, "p")] = 1;
    static [(c = 1, "s")] = 1;
    get [(d = 1, "g")]() { return 1; }
}
const o = { [(e = 1, "k")]: 1 };
a; b; c; d; e;
let f: number;
const C2 = class { [(f = 1, "m")]() {} };
f;
let g: number;
class G { [(g = 1, "k")]() {} static { g; } }
let h: number;
class H { [k(h)]() {} static { h = 1; } }
h;
let i: number;
class I { static { i = 1; } [k(i)]() {} }
i;
let v: number;
class V { [(v = 1, "a")]() {} [k(v)]() {} }
let w: number;
class W { [k(w)]() {} }
let x: number;
class X { static { x; } static { x = 1; } }
x;
let y: number;
class Y { static p = (y = 1); }
y;
export {};
