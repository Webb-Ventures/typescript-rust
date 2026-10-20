// An assertion function that can return normally with a non-number.
function assertNumber(x: unknown): asserts x is number { // @expect TSR0703
  if (typeof x !== "number") console.warn("not a number");
}
const v: unknown = "a";
assertNumber(v);
export const n: number = v;
