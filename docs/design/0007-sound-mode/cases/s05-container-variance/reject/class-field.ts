// Storing a Dog[] in a field typed Animal[].
import { makeDog, type Animal, type Dog } from "../../_shared/models";

class Shelter {
  residents: Animal[];
  constructor(dogs: Dog[]) {
    this.residents = dogs; // @expect TSR0501
  }
}
const shelter = new Shelter([makeDog("a")]);
export const n: number = shelter.residents.length;
