// rxjs 7 (emit-repos): a return context's contravariant inference wins over a covariant
// one that fits none of them, and a conditional of two callbacks fixes what their shared
// contextual signature mentions.
interface Obs<T> {
  x: T
  subscribe(next?: (value: T) => void): void
  pipe<A, B>(op1: Op<T, A>, op2: Op<A, B>): Obs<B>
}
interface Op<T, R> {
  (s: Obs<T>): Obs<R>
}
interface Mono<T> extends Op<T, T> {}
declare function sub<T>(n: number): Mono<T>
declare function sub3<T>(n: number): (s: Obs<T>) => Obs<T>
export function piped<T>(o: Obs<T>) {
  return o.pipe(sub(1), sub(2))
}
export const m4: Op<string, number> = sub(1)
export const m5: Op<string, number> = sub3(1)
export const m6: (s: Obs<string>) => Obs<number> = sub(1)

declare function reduce<V, A = V>(acc: (acc: A | V, value: V, index: number) => A): Op<V, V | A>
declare function reduce<V, A>(acc: (acc: A, value: V, index: number) => A, seed: A): Op<V, A>
declare function isFunction(x: any): x is (...args: any[]) => any
export function max<T>(comparer?: (x: T, y: T) => number): Mono<T> {
  return reduce(isFunction(comparer) ? (x, y) => (comparer(x, y) > 0 ? x : y) : (x, y) => (x > y ? x : y))
}
