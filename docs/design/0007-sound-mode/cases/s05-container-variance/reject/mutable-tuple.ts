// Mutable tuples are invariant too.
import { makeDog, type Animal, type Dog } from "../../_shared/models";

const dogPair: [Dog, number] = [makeDog("a"), 1];
const pair: [Animal, number] = dogPair; // @expect TSR0501
export const n: number = pair[1];
