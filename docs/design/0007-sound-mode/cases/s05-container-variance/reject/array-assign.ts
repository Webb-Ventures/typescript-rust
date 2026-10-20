// Dog[] to Animal[]: animals.push(cat) would put a Cat into dogs.
import { makeDog, type Animal, type Dog } from "../../_shared/models";

const dogs: Dog[] = [makeDog("a")];
const animals: Animal[] = dogs; // @expect TSR0501
export const count: number = animals.length;
