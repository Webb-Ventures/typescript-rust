// A failed relation to a type parameter says why: the source fits the constraint but the
// parameter may be narrower, or the parameter may be anything at all.
export function f<T, K extends "a" | "b", S extends T>(t: T, k: K, s: S) {
    const a: T = "x";
    const b: K = "a";
    const c: S = t;
    const d: K = 1;
    take<K>("b");
    return [a, b, c, d, s];
}
declare function take<X>(x: X): void;

export class C {
    self = this;
    c = new C();
    f() {
        this.self = this.c;
    }
}
