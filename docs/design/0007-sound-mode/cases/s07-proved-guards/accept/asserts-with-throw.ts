// An assertion function that throws on every path where the value isn't a string.
function assertString(x: unknown): asserts x is string {
  if (typeof x !== "string") throw new TypeError("not a string");
}
const v: unknown = "a";
assertString(v);
export const s: string = v;
