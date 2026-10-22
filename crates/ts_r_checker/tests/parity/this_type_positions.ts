// The 'this' type is only available in a non-static member of a class or interface (TS2526).
var x1: this;
type T1 = this;
interface I1 {
    a: this;
    b: { x: this };
    m(): this;
    n: () => this;
    o: Array<this>;
}
declare class Base<T> {}
class C1 extends Base<this> {
    a: this;
    b = (x: this) => x;
    c: { x: this };
    m(): this { const f = (y: this) => y; return this; }
    static s: this;
    get g(): this { return this; }
    [k: string]: any;
    p = { q(z: this) {} };
    constructor(public r: this) {}
}
class C2<T extends this> {}
interface I2 extends Base<this> {}
const o = { m(x: this) {} };
function f(this: any, x: this) {}
class C3 {
    n = 1;
    constructor(a: this) {
        let self: this = this;
    }
    static m() { return undefined as unknown as this; }
    f() {
        type ContainingThis = this;
        function g(x: this): this { return x; }
    }
}
const obj2 = {
    isNumber(): this is { b: string } { return true; },
};
export {};
