// Iterable<T> is read-only, so it stays covariant.
import { makeDog, type Animal, type Dog } from "../../_shared/models";

function count(xs: Iterable<Animal>): number {
  let n = 0;
  for (const x of xs) n += x.name.length;
  return n;
}
const dogs: Dog[] = [makeDog("a")];
export const c: number = count(dogs);
