// Returning Dog[] as Animal[].
import { makeDog, type Animal, type Dog } from "../../_shared/models";

const dogs: Dog[] = [makeDog("a")];
function all(): Animal[] {
  return dogs; // @expect TSR0501
}
export const n: number = all().length;
