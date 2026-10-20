// An object spread is contextually typed by its object literal's contextual type, and a
// static field of a class expression by the class's contextual type's property.
export {};
interface I {
    a: "a";
}
let i: I = { a: "a" };
i = { ...{ a: "a" } };
i = { ...{ a: "b" } };
interface S {
    x: { a: "a" };
}
let c: S = class {
    static x = { a: "a" };
};
let d: S = class {
    static x = { a: "b" };
};
class E {
    static x = { a: "a" };
}
let e: S = E;
