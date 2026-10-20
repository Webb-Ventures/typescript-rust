// Exact element types, and generic inference that picks the element type.
import { makeDog, type Dog } from "../../_shared/models";

function append<T>(xs: T[], x: T): void {
  xs.push(x);
}
const dogs: Dog[] = [makeDog("a")];
const same: Dog[] = dogs;
append(same, makeDog("b"));
export const count: number = dogs.length;
