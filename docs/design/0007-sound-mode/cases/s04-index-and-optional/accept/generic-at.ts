// A generic first-element helper that returns undefined honestly.
function firstOf<T>(xs: readonly T[]): T | undefined {
  return xs[0];
}
export const f: number | undefined = firstOf([1, 2]);
