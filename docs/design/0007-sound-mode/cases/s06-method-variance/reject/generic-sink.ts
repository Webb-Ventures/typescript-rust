// A sink of Dogs used as a sink of Animals.
import type { Animal, Dog } from "../../_shared/models";

interface Sink<T> {
  put(value: T): void;
}
const dogSink: Sink<Dog> = {
  put(d: Dog): void {
    console.log(d.bark());
  },
};
const animalSink: Sink<Animal> = dogSink; // @expect TSR0601
animalSink.put({ name: "tom" });
export {};
