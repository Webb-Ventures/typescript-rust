// Under strictFunctionTypes, construct signatures relate their parameters
// contravariantly like call signatures; only a class `constructor` (and methods) stay
// bivariant.
interface Base { a2: new (x?: number) => number; c2: (x?: number) => number; }
interface I6 extends Base { a2: new (x: number) => number; }
interface I7 extends Base { c2: (x: number) => number; }
class Animal { a = 1 } class Dog extends Animal { d = 1 }
class A { constructor(x: Dog) {} }
class B { constructor(x: Animal) {} }
let ca: typeof A = B;
let cb: typeof B = A;
let n1: new (x: Animal) => A = A;
let n2: new (x: Dog) => B = B;
abstract class Q { constructor(x: Dog) {} }
let q: abstract new (x: Animal) => Q = Q;
interface CI { new (x: Dog): A }
let ci: CI = A;
let ci2: { new (x: Animal): A } = {} as CI;
declare class D { constructor(x: Dog); }
let d: typeof B = D;
