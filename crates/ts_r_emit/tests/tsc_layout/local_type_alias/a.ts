interface Err<T> {
  e: T
}
interface Opts<A, B> {
  a?: A
  b?: B
}
export function f<T>() {
  type E = Err<T>
  function g(path: string, opts?: Opts<unknown, E>): Opts<unknown, E> {
    return opts ?? {}
  }
  function h(opts: E): void {}
  return { g, h }
}
