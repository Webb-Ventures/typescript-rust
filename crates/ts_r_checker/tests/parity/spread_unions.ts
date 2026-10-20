// Spreads of unions: `X | {}` and `X | undefined` spread X's properties as optional; other
// unions distribute without normalizing the results.
const param2 = Math.random() < 0.5 ? "value2" : null;
const obj = {
  param1: "value1",
  ...(param2 ? { param2 } : {}),
};
export const o: number = obj;
declare function entries<T>(o: { [s: string]: T }): T;
export const e: number = entries(obj);

declare const union: { a: number } | { b: string };
var o3: { a: number } | { b: string };
var o3 = { ...union };
var o5: { a: number } | { b: string } | { a: number; b: string };
var o5 = { ...union, ...union };
declare const undefinedUnion: { a: number } | undefined;
export const o6: number = { ...undefinedUnion };
export const o7: number = { ...undefinedUnion, ...undefinedUnion };
