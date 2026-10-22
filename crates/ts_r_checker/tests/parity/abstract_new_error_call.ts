// `new` of an abstract class is TS2511 and an error call: the arguments are not matched
// against the constructor (no TS2554 for `new B(1)`), and the result is the error type.
abstract class B {
  constructor() {}
}
const b = new B(1);
const n: number = b;
