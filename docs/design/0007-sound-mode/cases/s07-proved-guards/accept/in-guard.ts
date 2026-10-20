// An in check that separates the union members.
import { makeCat, type Cat, type Dog } from "../../_shared/models";

function isCat(a: Cat | Dog): a is Cat {
  return "meow" in a;
}
function speak(pet: Cat | Dog): string {
  return isCat(pet) ? pet.meow() : pet.bark();
}
export const sound: string = speak(makeCat("tom"));
