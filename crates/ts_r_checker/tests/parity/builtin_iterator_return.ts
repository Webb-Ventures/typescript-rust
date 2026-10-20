// The lib's `type BuiltinIteratorReturn = intrinsic` is `undefined` under
// `strictBuiltinIteratorReturn` (part of `strict`): a built-in iterator's `next()` gives
// `IteratorResult<T, undefined>`, and a function returning its value has a written type.
export function single(arr: number[]) {
  const s = new Set<number>(arr.map((x) => x + 1));
  if (s.size !== 1) {
    return 207;
  }
  const v = s.values().next().value;
  return v!;
}
export const n: string = single([]);
export const m: string = new Set<number>().values().next();
export const k: string = [1].entries().next().value;
