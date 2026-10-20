// A generic helper that claims the element exists.
function firstOf<T>(xs: readonly T[]): T {
  return xs[0]; // @expect TSR0401
}
export const f: number = firstOf([1, 2]);
