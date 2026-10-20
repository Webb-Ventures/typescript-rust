// A fresh array literal is contextually typed, so it can be an Animal[] and be written to.
import { makeDog, type Animal, type Dog } from "../../_shared/models";

const dogs: Dog[] = [makeDog("a")];
const animals: Animal[] = [...dogs];
animals.push({ name: "tom" });
export const count: number = animals.length;
