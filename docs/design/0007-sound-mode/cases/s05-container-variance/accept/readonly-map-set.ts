// ReadonlyMap and ReadonlySet stay covariant.
import { makeDog, type Animal, type Dog } from "../../_shared/models";

const dogMap = new Map<string, Dog>([["a", makeDog("a")]]);
const dogSet = new Set<Dog>([makeDog("b")]);
const view: ReadonlyMap<string, Animal> = dogMap;
const seen: ReadonlySet<Animal> = dogSet;
export const total: number = view.size + seen.size;
