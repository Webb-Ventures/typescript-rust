// Passing Dog[] to a parameter that writes Animals.
import { makeDog, type Animal, type Dog } from "../../_shared/models";

function adopt(xs: Animal[]): void {
  xs.push({ name: "stray" });
}
const dogs: Dog[] = [makeDog("a")];
adopt(dogs); // @expect TSR0501
export {};
