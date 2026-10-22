// An unannotated getter with a computed name takes the type of the setter of the same
// computed name.
const enum G { A = 1, B = 2 }
class C {
    get [G.B]() {
        return true;
    }
    set [G.B](x: number) { }
}
class D {
    get [Symbol.hasInstance]() {
        return "";
    }
    set [Symbol.hasInstance](x: number) {
    }
}
(new D)[Symbol.hasInstance] = 0;
(new D)[Symbol.hasInstance] = "";
const o = { get ["k"]() { return 1; }, set ["k"](v: string) {} };
