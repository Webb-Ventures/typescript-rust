// trpc `loggerLink.ts` (`next(op).pipe(tap({ ... }))`) and `TRPCUntypedClient.ts`
// (`chain$.pipe(share())`): an inner call's contextual type that mentions an outer call's type
// parameters still infers from the rest (`TValue` from the source parameter), the enclosing
// function's type parameters count as known, and a generic alias that resolves to a branch
// type prints as that type.
interface Sub<V, E> {
  subscribe(o: Partial<Obs<V, E>>): void;
}
interface Obs<V, E> {
  next: (v: V) => void;
  error: (e: E) => void;
}
type OpFn<A, B, C, D> = (source: Sub<A, B>) => Sub<C, D>;
type MonoOp<V, E> = OpFn<V, E, V, E>;
interface Ob<V, E> extends Sub<V, E> {
  pipe<V1, E1>(op1: OpFn<V, E, V1, E1>): Ob<V1, E1>;
}
type InferValue<T> = T extends Ob<infer X, unknown> ? X : never;
declare function tap<V, E>(observer: Partial<Obs<V, E>>): MonoOp<V, E>;
declare function share<V, E>(): MonoOp<V, E>;
declare const o: Ob<{ a: 1 }, Error>;
type Val = InferValue<typeof o>;
declare const v: Val;
const vn: null = v;
const piped = o.pipe(
  tap({
    next(r) {
      const q: null = r;
    },
    error(e) {
      const z: null = e;
    },
  }),
);
const pn: null = piped;
const sh = o.pipe(share());
const shn: null = sh;
export function gen<T>(x: Ob<T, Error>) {
  const p = x.pipe(share());
  const pn: null = p;
  type W = InferValue<typeof p>;
  const w: W = null as any;
  const wn: null = w;
}
type C<T> = T extends string ? 1 : 2;
type D = C<"x" | 3>;
declare const d: D;
const dn: null = d;
