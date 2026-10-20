// Cause:    extra TS2344: Type '…' does not satisfy the constraint '…'.
// Found in: type-fest@5 source/absolute.d.ts (183 diagnostics across type-fest)
// Owner:    ts_r_checker
// Expected: tsc 7 reports nothing: a type inferred from a template literal placeholder
//           (`-${infer M}`) is implicitly constrained to string.
// Actual:   ts-r-check reports TS2344 "Type 'M' does not satisfy the constraint 'string'."
type Upper<S extends string> = Uppercase<S>;
export type Abs<N extends number | string> = `${N}` extends `-${infer M}` ? Upper<M> : N;
