// `new` through a union's construct signature is TS2511 when any member signature it
// stands for is abstract, wherever that member sorts in the union.
class ConcreteA {}
class ConcreteB {}
abstract class AbstractA { a!: string; }
declare const c1: typeof ConcreteA | typeof AbstractA;
declare const c2: typeof ConcreteA | typeof ConcreteB;
new c1();
new c2();
[ConcreteA, AbstractA].map(cls => new cls());
