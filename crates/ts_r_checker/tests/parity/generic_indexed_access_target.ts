// A concrete source relates to a generic `T[K]` only through the constraint of `T[K]` for
// writing: the object's and the key's constraints indexed together, a union key asking for
// every property at once. The hint names the indexed access by its object.
function g<T, K extends keyof T>(t: T, k: K) {
    const x: T[K] = 1;
}
function h<T extends { a: string; b: number }, K extends keyof T>(t: T, k: K) {
    const a: T["a"] = "x";
    const b: T["a"] = 1;
    const c: T[K] = "x";
    t[k] = 1;
    t.a = "s";
}
function i<K extends "a" | "b">(o: { a: string; b: number }, k: K) {
    o[k] = 1;
    const v: { a: string; b: number }[K] = 1;
}
function j<T extends Record<string, number>>(t: T) {
    const v: T[string] = 1;
}
type TypeMap = { A: "a"; B: "b" };
function m<T2 extends keyof TypeMap>(v: TypeMap[T2]) {
    const z: TypeMap[T2] = "a";
}
