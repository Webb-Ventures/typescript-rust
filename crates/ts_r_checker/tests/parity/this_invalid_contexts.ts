// `this` in a namespace body (TS2331) or an enum member (TS2332) is `any`, implicitly so.
namespace N {
    this.x = 5;
    var a = this;
    const f = () => this;
    function g() { return this; }
    class C { m() { return this; } p = this; }
    enum E2 { A = this }
    namespace Inner { var b = this; }
}
enum E { A = this, B = this.foo, C = (() => this)() }
declare namespace D { var d: typeof this; }

// A module's top-level `this` is `undefined`.
const t1 = this.no;
const t2 = () => this.x;
let t3: typeof this.no = 1;

// `typeof this` is the `this` expression there, narrowed.
class Test10 {
    a?: { b?: string }
    foo() {
        if (this.a) {
            let a: typeof this.a = undefined as any;
            a.b;
            let b: typeof this.a.b = undefined as any;
        }
    }
}
class A0 { a = 1; }
class B0 extends A0 { b() {} }
class T9 {
    f() {
        if (this instanceof B0) {
            const d1: typeof this = this;
            d1.b();
        }
    }
}
function T4(this: { no: number } | undefined) {
    let x: typeof this.no = 1;
}
function T2() {
    let x: typeof this.no = 1;
}
declare const v: { a: number } | undefined;
function q() {
    let y: typeof v.a = 1;
}
export {};
