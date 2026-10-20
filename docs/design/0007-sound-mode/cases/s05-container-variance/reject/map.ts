// Map<string, Dog> to Map<string, Animal>.
import { makeDog, type Animal, type Dog } from "../../_shared/models";

const dogMap = new Map<string, Dog>([["a", makeDog("a")]]);
const byName: Map<string, Animal> = dogMap; // @expect TSR0501
export const size: number = byName.size;
