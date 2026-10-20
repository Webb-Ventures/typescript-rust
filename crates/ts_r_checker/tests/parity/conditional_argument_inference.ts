// A conditional argument infers from its arms that are not context sensitive before the
// context-sensitive ones are typed (`c ? (s) => {} : init` infers from `init`), and arms
// that are generic calls see what the earlier arguments inferred.
class Sink<T> { next(v: T) {} }
declare function opt<T>(f?: (s: Sink<T>) => void): Sink<T>;
declare function req<T>(f: (s: Sink<T>) => void): Sink<T>;
export function pick(c?: number) {
  const init = (s: Sink<never>) => {};
  const a: number = opt(c ? (s) => {} : init);
  const b: number = req(c ? init : (s) => {});
  const d: number = req(init);
}
interface Step<T, R> { (source: Flow<T>): Flow<R> }
declare class Flow<T> {
  value: T;
  pipe<A>(op1: Step<T, A>): Flow<A>;
  pipe<A, B>(op1: Step<T, A>, op2: Step<A, B>): Flow<B>;
}
declare function orElse<T, R>(d: R): Step<T, T | R>;
declare function first<T>(n: number): Step<T, T>;
declare function orThrow<T>(): Step<T, T>;
export function at<T, D>(has: boolean, d: D): Step<T, T | D> {
  return (source: Flow<T>) => source.pipe(first(1), has ? orElse(d) : orThrow());
}
export function at2<T, D>(has: boolean, d: D) {
  return (source: Flow<T>) => {
    const r: string = source.pipe(first(1), has ? orElse(d) : orThrow());
  };
}
