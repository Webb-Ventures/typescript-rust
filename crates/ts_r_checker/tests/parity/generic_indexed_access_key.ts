// TS2536 on generic indexed access types: the key must be a key of the object, read with
// the constraints implied by enclosing conditional and homomorphic mapped types.
type Obj = { [s: string]: number };
type Arr<U extends string[]> = { [K in keyof U]: Obj[K] };
type NotArr<U extends { a: 1 }> = { [K in keyof U]: Obj[K] };
type Nested<C> = C extends { types: { actors: string } } ? C["types"]["actors"] : never;
type Rest<N extends unknown[]> = N extends [unknown, ...infer R] ? R["length"] : never;
type Params<F> = (F extends (...args: infer P) => any ? P : never)[0];
type ParamsBad<F> = (F extends (...args: infer P) => any ? P : never)["x"];
type Keys<T> = { [key: string]: { [K in keyof T]: K }[keyof T] };
type Tup<T> = { [key: string]: { [K in keyof T]: [K] }[keyof T] };
type ByKey<T, Q extends Keys<T>> = { [K in keyof Q]: T[Q[K]] };
type ByTuple<T, Q extends Tup<T>> = { [K in keyof Q]: T[Q[K]["0"]] };
declare class Unconstrained<P> {
  x: P["ctx"];
}
function f<T, U extends T>(k: keyof T) {
  const x: keyof U = k;
}
function g<T, U, K extends keyof T | keyof U>(k: K) {
  const x: keyof (T & U) = k;
}
// Type arguments of an import type are not checked.
export interface Box<T> { v: T }
declare class ViaImport<P> {
  x: import("./a").Box<P["ctx"]>;
}
