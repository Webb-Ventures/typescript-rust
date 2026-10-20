// The body narrows exactly to the predicate type on both sides.
function isString(x: unknown): x is string {
  return typeof x === "string";
}
const v: unknown = "a";
export const len: number = isString(v) ? v.length : 0;
