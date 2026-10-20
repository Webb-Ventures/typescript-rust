// A function that only reads takes a readonly array, so callers can pass subtypes.
import { makeDog, type Animal, type Dog } from "../../_shared/models";

function names(xs: readonly Animal[]): string[] {
  return xs.map((x) => x.name);
}
const dogs: Dog[] = [makeDog("a")];
export const n: string[] = names(dogs);
