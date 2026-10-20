// Casting to a supertype is allowed.
import { makeDog, type Animal } from "../../_shared/models";

const animal = makeDog("rex") as Animal;
export const n: string = animal.name;
