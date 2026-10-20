// A symbol key in an 'in' check needs a symbol index signature, not a string one.
interface HI {
  [Symbol.iterator](): IterableIterator<[string, string]>;
}
type H = HI | Record<string, string | undefined>;
export function f(h: H) {
  if (Symbol.iterator in h) {
    const n: number = h;
  } else {
    const n: number = h;
  }
}
