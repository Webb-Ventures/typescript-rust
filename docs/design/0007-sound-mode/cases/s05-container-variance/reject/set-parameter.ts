// Set<Dog> passed where Set<Animal> is expected.
import { makeDog, type Animal, type Dog } from "../../_shared/models";

function addStray(target: Set<Animal>): void {
  target.add({ name: "stray" });
}
const dogSet = new Set<Dog>([makeDog("a")]);
addStray(dogSet); // @expect TSR0501
export {};
