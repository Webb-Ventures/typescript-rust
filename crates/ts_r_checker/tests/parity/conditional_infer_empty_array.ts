// An empty array literal's element type is a candidate for a conditional type's `infer`
// (`never[] extends ArrayLike<infer T>` gives `never`), so `from([])` against a return
// context is `Box<never>`.
declare class Box<T> { value: T }
type Input<T> = Box<T> | PromiseLike<T> | ArrayLike<T> | Iterable<T>;
type ValueOf<O> = O extends Input<infer T> ? T : never;
declare function from<O extends Input<any>>(input: O): Box<ValueOf<O>>;
type E<O> = O extends ArrayLike<infer T> ? T : 0;
type F<O> = O extends Array<infer T> ? T : 0;
declare function h<O>(o: O): E<O>;
declare function k<O>(o: O): F<O>;
declare function f<T>(x: T[]): T;
export function comb<O extends Input<any>, R>(s: any): Box<R> | Box<ValueOf<O>[]> {
  if (s) {
    return from([]);
  }
  const x: number = from([]);
  const a: string = f([]);
  const c: string = h([]);
  const d: string = k([]);
  return null!;
}
