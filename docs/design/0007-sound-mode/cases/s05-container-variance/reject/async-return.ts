// The awaited return type of an async function is compared the same way.
import { makeDog, type Animal, type Dog } from "../../_shared/models";

const dogs: Dog[] = [makeDog("a")];
async function load(): Promise<Animal[]> {
  return dogs; // @expect TSR0501
}
export { load };
