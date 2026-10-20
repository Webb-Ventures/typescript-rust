// trpc `serialize.ts` (`type JsonArray = JsonValue[] | readonly JsonValue[]; type JsonValue
// = JsonPrimitive | JsonObject | JsonArray`, `Serialize<Date>` through `T extends JsonValue`):
// a non-generic type alias may refer to itself through array and tuple element types.
type JsonPrimitive = boolean | number | string | null;
type JsonArray = JsonValue[] | readonly JsonValue[];
type JsonObject = {
  readonly [key: string | number]: JsonValue;
  [key: symbol]: never;
};
type JsonValue = JsonPrimitive | JsonObject | JsonArray;
type IsJson<T> = T extends JsonValue ? true : false;
declare const dt: Date;
const v: JsonArray = dt;
const b1: IsJson<Date> = true;
const b2: IsJson<string[]> = false;
const b3: IsJson<{ a: [1, "x"] }> = false;
type Serialize<T> = IsJson<T> extends true ? T : T extends { toJSON(): infer U } ? U : never;
const s1: Serialize<Date> = 1;
type J1 = string | J1[];
declare const n: number[];
const a1: J1 = n;
const a2: J1 = ["a", ["b"]];
type T1 = [T1?];
const t1: T1 = [[[]]];
type L = string | Array<L>;
const l1: L = [["x"]];
function f(j: J1) {
  if (typeof j !== "string") {
    const x: number = j[0];
  }
}
export {};
