// A private member inherited by an interface with members of its own is named after the
// interface; by an empty interface or a derived class, after the declaring class.
class Foo {
    private x = "";
}
interface I extends Foo {
    y: number;
}
interface J extends Foo {}
class Bar implements I {
    x = "";
    y = 1;
}
class Baz implements J {
    x = "";
}
class A {
    private m() {}
}
class B extends A {}
class C extends B {
    public m() {}
}
