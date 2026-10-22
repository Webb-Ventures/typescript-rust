// A `#private` name no enclosing class declares does not exist on `any`; spreads drop
// `#private` fields.
class A {
    #foo = true;
    static #baz = 10;
    method(thing: any) {
        thing.#foo;
        thing.#baz;
        thing.#bar;
    }
    methodU(thing: unknown) {
        thing.#foo;
        thing.#bar;
    }
}
class C {
    #prop = 1;
    static #propStatic = 1;
    method(other: C) {
        const obj = { ...other };
        obj.#prop;
        const { ...rest } = other;
        rest.#prop;
        const statics = { ...C };
        statics.#propStatic;
    }
}
