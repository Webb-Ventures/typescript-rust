// A rest parameter typed by a union of tuples that mentions a type parameter
// (`next(...[value]: [] | [N])`) makes the measured variance unreliable, so failures are
// explained structurally; a covariant `void` target argument that fails structurally too is
// explained by the arguments.
export {};
interface G1<T, R, N> { next(...[value]: [] | [N]): IteratorResult<T, R>; }
interface G2<T, R, N> { next(value: N): IteratorResult<T, R>; }
interface A1<T, N> { next(...args: [N]): T; }
interface A4<T, N> { next: (...args: [] | [N]) => T; }
declare const g1: G1<number, void, any>;
const h1: G1<never, unknown, unknown> = g1;
declare const g2: G2<number, void, any>;
const h2: G2<never, unknown, unknown> = g2;
declare const a1: A1<number, any>;
const b1: A1<never, unknown> = a1;
declare const a4: A4<number, any>;
const b4: A4<never, unknown> = a4;
interface State { foo: number }
declare const r1: IteratorReturnResult<State>;
const r2: IteratorReturnResult<void> = r1;
