// Ecosystem check repro (was ecosystem/repros/check/extra-ts2344-infer-in-template.d.ts): extra TS2344: Type '…' does not satisfy the constraint '…'.
// @filename: index.d.ts
type Upper<S extends string> = Uppercase<S>;
export type Abs<N extends number | string> = `${N}` extends `-${infer M}` ? Upper<M> : N;
