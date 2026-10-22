// `private` and `protected` constructors: `new` outside the class (or, for protected, its
// subclasses) is TS2673/TS2674, extending a private one TS2675, and a less visible
// constructor type is not assignable to a more visible one (TS2672).
class Foo { constructor(public x: number) { } }
class Baz { protected constructor(public x: number) { } }
class Qux {
    private constructor(public x: number) { }
    static make() { return new Qux(1); }
}
class DerivedBaz extends Baz {
    static make() { return new Baz(1); }
}
class Unrelated {
    static fake() { new Baz(1); }
}
class DerivedQux extends Qux { }
var bz = new Baz(1);
var qx = new Qux(1);
var ctor: () => void = qx.constructor;
let a = Foo;
a = Baz;
a = Qux;
let b = Baz;
b = Foo;
b = Qux;
let c = Qux;
c = Baz;
