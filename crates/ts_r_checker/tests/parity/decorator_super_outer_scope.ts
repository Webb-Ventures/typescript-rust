// A decorator runs in the scope around the class it decorates: `super` in a member's
// decorator names the super of that outer scope (TS2660 in a namespace, TS2335 in a
// class without `extends`, fine in a derived class's method).
namespace M {
    class S { decorator() { } }
    class C extends S {
        @(super.decorator as any)
        method() { }
    }
}
class B { m() {} }
class Outer extends B {
    m() {
        class Inner {
            @(super.m as any)
            x() {}
        }
        return Inner;
    }
}
class P {
    m() {
        class I { @(super.x as any) y() {} }
        return I;
    }
}
@(super.y as any)
class Q {}
const o = { m() { @(super.toString as any) class R {} return R; } };
export { Q, o };
