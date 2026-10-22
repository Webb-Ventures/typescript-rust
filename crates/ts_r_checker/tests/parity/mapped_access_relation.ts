// `{ [P in keyof T]?: X }[K]` relates as `X` with `P` replaced by `K`, plus `undefined`
// for an optional modifier. Intersections with `{}` or `null` are normalized member by
// member. An unconstrained `T[K]` against a union is explained through `T[keyof T]`.
function f11<T, K extends keyof T>(x: T, y: Partial<T>, k: K) {
    const z: [] = y[k];
    x[k] = y[k];
}
function f13<T, U extends T, K extends keyof T>(x: T, y: Partial<U>, k: K) {
    y[k] = x[k];
}
function f3<T, K extends keyof T>(t: T[K], p1: Partial<T>[K] & {}, p2: Partial<T>[K] & ({} | null)) {
    t = p1;
    t = p2;
}
function f4<T, U extends T, K extends keyof T>(x: T[K]) {
    let y: U[K] | undefined = x;
    let z: U[K] | string = x;
}
// A generic key must be a key of the object (TS2536); writing through `Readonly<T>[K]`
// reports TS2542. A key valid for each union member is valid for the union.
function f5<T, U extends T, K extends keyof T>(x: T, y: Readonly<T>, k: K, ku: keyof U) {
    x[ku] = x[k];
    y[k] = x[k];
}
type Table<K extends string> = { [k in K]: 0 | 1 };
function f6<F extends string>(r: ({ a: number } | { a: string }) & Table<F>, f: F) {
    return r[f];
}
