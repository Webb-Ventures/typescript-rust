// A missing private or protected property names the class declaring it, under the head
// when that class is not the target.
class A {
    private x = 1;
    foo(): number { return 1; }
}
class C {
    foo() { return 1; }
}
class C2 extends A {}
declare var c: C;
declare var c2: C2;
c2 = c;
class P { y = 1; protected z = 2; }
class P2 extends P {}
class Q { y = 1; }
declare var q: Q;
let p2: P2 = q;
class R { protected z = 2; }
let r: R = q;
