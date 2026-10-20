// trpc `stream/jsonl.ts`: return-context inferences that mention an outer call's type
// parameters still being inferred do not apply when the inner call has defaults; the
// methods of an object literal argument fix the type parameters their parameters mention.
declare class TS<I = any, O = any> {
  constructor(t?: { transform?(chunk: I, c: { enqueue(o: O): void }): void });
  readable: RS<O>;
  writable: WS<I>;
}
interface Pair<T, R> {
  writable: WS<R>;
  readable: RS<T>;
}
declare class RS<R> {
  pipeThrough<T>(p: Pair<T, R>): RS<T>;
}
declare class WS<W> {
  write(w: W): void;
}
declare const s: RS<number>;
export function f() {
  const r = s.pipeThrough(
    new TS({
      transform(chunk, c) {
        const k: null = chunk;
        c.enqueue("x");
      },
    }),
  );
  const check: null = r;
  return r;
}
export function g() {
  let st: RS<number | string> = s;
  st = st.pipeThrough(
    new TS({
      transform(chunk, c) {
        const k: null = chunk;
        c.enqueue(1);
      },
    }),
  );
}
