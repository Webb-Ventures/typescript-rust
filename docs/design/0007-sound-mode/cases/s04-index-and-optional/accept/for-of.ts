// Iteration is not indexed access.
function totalLength(names: readonly string[]): number {
  let n = 0;
  for (const name of names) n += name.length;
  return n;
}
export const t: number = totalLength(["a", "bb"]);
