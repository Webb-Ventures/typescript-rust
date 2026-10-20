// A generic call whose contextual type mentions an outer call's type parameter still being
// inferred (`Step<T, A>`) fixes its own type parameters, for a context-sensitive argument,
// to what the context implies without that parameter: `filter`'s `T` is the outer `T`.
interface Step<T, R> { (source: Flow<T>): Flow<R> }
declare class Flow<T> {
  value: T;
  pipe<A, B>(op1: Step<T, A>, op2: Step<A, B>): Flow<B>;
}
declare function filter<T>(p: (value: T, index: number) => boolean): Step<T, T>;
declare function orElse<T, R>(d: R): Step<T, T | R>;
export function at<T, D>(index: number, d: D): Step<T, T | D> {
  return (source: Flow<T>) => source.pipe(filter((v, i) => i === index), orElse(d));
}
export function at2<T, D>(index: number, d: D) {
  return (source: Flow<T>) => {
    const r: number = source.pipe(filter((v, i) => i === index), orElse(d));
    const s: number = source.pipe(filter((v, i) => v === null), orElse(d));
  };
}
