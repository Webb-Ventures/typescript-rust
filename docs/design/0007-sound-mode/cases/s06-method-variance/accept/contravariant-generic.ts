// A sink of Animals can be used as a sink of Dogs.
import { makeDog, type Animal, type Dog } from "../../_shared/models";

interface Sink<T> {
  put(value: T): void;
}
const log: string[] = [];
const animalSink: Sink<Animal> = {
  put(a: Animal): void {
    log.push(a.name);
  },
};
const dogSink: Sink<Dog> = animalSink;
dogSink.put(makeDog("rex"));
export {};
