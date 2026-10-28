declare class Tuple<Items extends ReadonlyArray<unknown> = []> extends Array<Items[number]> {}
type F<T extends readonly any[]> = T;
type A<M> = M extends ReadonlyArray<number> ? F<[...M]> : never;
type B<M> = M extends Tuple<infer X> ? F<X> : M extends ReadonlyArray<number> ? F<[...M]> : never;
type C<M> = M extends string ? 1 : M extends readonly unknown[] ? [string, ...M] : never;
type D<M> = M extends string ? [...M] : never;
type E<M> = M extends ReadonlyArray<number> ? 1 : [...M];
type G<M, N> = M extends readonly unknown[] ? [...N] : never;
export type { A, B, C, D, E, G };
