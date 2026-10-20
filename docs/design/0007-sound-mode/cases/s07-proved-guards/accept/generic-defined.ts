// A generic guard: the true side is T, the false side is undefined.
function isDefined<T>(x: T | undefined): x is T {
  return x !== undefined;
}
const xs: (number | undefined)[] = [1, undefined, 2];
export const defined: number[] = xs.filter(isDefined);
