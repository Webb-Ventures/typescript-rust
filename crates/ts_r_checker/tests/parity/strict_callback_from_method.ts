// A strict function type's callback parameter relates its parameters contravariantly
// even when the callback type comes from a method.
interface Animal { animal: void }
interface Dog extends Animal { dog: void }
class Foo {
    static f1(x: Animal): Animal { throw "wat"; }
    static f2(x: Dog): Animal { throw "wat"; };
}
declare let f1: (cb: typeof Foo.f1) => void;
declare let f2: (cb: typeof Foo.f2) => void;
f1 = f2;
f2 = f1;
type BivariantHack<Input, Output> = { foo(x: Input): Output }["foo"];
declare let g1: (cb: BivariantHack<Animal, Animal>) => void;
declare let g2: (cb: BivariantHack<Dog, Animal>) => void;
g1 = g2;
g2 = g1;
declare let h1: { m(cb: typeof Foo.f1): void };
declare let h2: { m(cb: typeof Foo.f2): void };
h1 = h2;
h2 = h1;
