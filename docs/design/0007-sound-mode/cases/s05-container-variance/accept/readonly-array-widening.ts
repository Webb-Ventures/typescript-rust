// A mutable array may be widened to a readonly array of a supertype.
import { makeDog, type Animal, type Dog } from "../../_shared/models";

const dogs: Dog[] = [makeDog("a")];
const animals: readonly Animal[] = dogs;
export const count: number = animals.length;
