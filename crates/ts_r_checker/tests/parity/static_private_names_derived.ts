// Static `#private` fields belong to their class's constructor only; an instance field a
// base class declares is reached through a derived instance inside the base.
class A {
    static #foo: number;
    static #bar: number;
    m() {
        A.#foo = 3;
        B.#foo;
        B.#bar;
    }
}
class B extends A {
    static #foo: string;
    m2() {
        B.#foo = "some string";
    }
}
class Parent {
    #foo = 3;
    static #bar = 5;
    accessChildProps() {
        new Child().#foo;
        Child.#bar;
    }
}
class Child extends Parent {
    #foo = "foo";
    #bar = "bar";
}
const willErrorSomeDay: typeof A = class {};
