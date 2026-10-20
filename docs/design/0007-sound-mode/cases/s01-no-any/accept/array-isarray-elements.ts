// Array.isArray narrows unknown to unknown[] in sound mode; each element is narrowed before use.
function countChars(input: unknown): number {
  if (!Array.isArray(input)) return 0;
  let n = 0;
  for (const item of input) {
    if (typeof item === "string") n += item.length;
  }
  return n;
}
export const total: number = countChars(["a", "bc"]);
