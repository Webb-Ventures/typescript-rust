// A variable initialized by a call to a function with one non-generic call signature
// takes that signature's return type without checking the arguments, so a callback
// argument whose body reaches back to the enclosing function is no circularity.
interface Obs<T> {
  subscribe(o: (v: T) => void): void;
}
type Link<T> = (opts: { op: number; next: (op: number) => Obs<T> }) => Obs<T>;
export function chain<T>(links: Link<T>[], op0: number): Obs<T> {
  function execute(index = 0, op = op0) {
    const next = links[index];
    if (!next) {
      throw new Error("none");
    }
    const subscription = next({
      op,
      next(nextOp) {
        const nextObserver = execute(index + 1, nextOp);
        return nextObserver;
      },
    });
    return subscription;
  }
  return execute();
}
declare function wrap(o: { next(): unknown }): number;
export function e1() {
  const s = wrap({
    next() {
      const n = e1();
      return n;
    },
  });
  return s;
}
export function e2() {
  const s = (wrap)({
    next() {
      const n = e2();
      return n;
    },
  });
  return s;
}
export const n1: string = e1();
