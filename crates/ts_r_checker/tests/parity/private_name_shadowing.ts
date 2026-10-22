// A nested class that redeclares a private name shadows the outer one (TS18014).
class A {
    #foo = 1;
    static #s = 2;
    m() {
        class B {
            #foo = 3;
            #s = 4;
            baz(a: A) { return a.#foo; }
            quux(b: B) { return b.#foo; }
            st() { return A.#s; }
        }
        class C {
            ok(a: A) { return a.#foo; }
        }
        return [B, C];
    }
}
class D {
    #foo = 1;
    other(a: A) { return a.#foo; }
}
export {};
