// A generic homomorphic mapped type over a type parameter constrained to tuples relates as
// the mapped type applied to the constraint: a readonly member makes it no `any[]`.
type Hom<T> = { [P in keyof T]: T[P] extends string ? boolean : null };
function test1<T extends [number] | [string]>(args: T) {
    const arr: any[] = [] as Hom<T>;
    const arr2: readonly any[] = [] as Hom<T>;
}
function test2<T extends [number] | readonly [string]>(args: T) {
    const arr: any[] = [] as Hom<T>;
    const arr2: readonly any[] = [] as Hom<T>;
}
declare class Base<T> {
    someProp: T;
    method<U extends unknown[]>(x: { [K in keyof U]: U[K] }): Base<U>;
}
declare class Derived<T> extends Base<T> {
    method<V extends unknown[]>(x: { [K in keyof V]: V[K] }): Base<V>;
}
