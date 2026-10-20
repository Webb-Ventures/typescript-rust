// A generic function whose return type is inferred is inferred through from the call's
// return context even when that return type has not been resolved yet (the callee is
// declared later): `return make(0)` in `(): Box<T>` infers `make`'s `T` as `T`.
declare class Sink<T> { next(v: T): void }
class Box<T> {
  value!: T;
  constructor(f?: (sink: Sink<T>) => void) {}
}
declare function isTagged(x: any): x is { tag: 1 };
export function first<T>(input: Box<T> | PromiseLike<T>): Box<T> {
  if (input instanceof Box) {
    return input;
  }
  if (input != null) {
    if (isTagged(input)) {
      return make(0);
    }
  }
  return later();
}
export function second<T>(): Box<T> {
  return make(1);
}
export function third(): Box<string> {
  const b: Box<number> = make(2);
  return later();
}
function make<T>(obj: any) {
  return new Box((sink: Sink<T>) => {
    sink.next(obj);
  });
}
function later<T>() {
  return new Box<T>();
}
