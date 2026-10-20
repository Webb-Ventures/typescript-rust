// A class inherits only the base construct signatures taking its extends clause's
// type argument count: none fits gives TS2508 (TS2314 for a class base) and no
// construct signatures (TS2351 on new).
interface Obs<T> { subscribe(x: unknown): void; v: T }
interface ObsCtor {
  new <T>(init: (subscriber: unknown) => void): Obs<T>;
  from<T>(value: Obs<T>): Obs<T>;
}
declare var Obs: ObsCtor;
declare const first: Obs<string>;
class My extends Obs {
  static from(source: any) {
    return new this((subscriber) => {});
  }
}
const source = My.from(first);
const n: number = source;
class A<T> { a!: T }
class B extends A {}
const nb: number = new B();
interface Ctor2 {
  new <T, U>(x: T): Obs<T>;
  new (): Obs<number>;
}
declare var C2: Ctor2;
class D extends C2 {}
class E extends C2<string> {}
class F extends C2<string, number> {}
interface Ctor3 { new <T = string>(): Obs<T> }
declare var C3: Ctor3;
class G extends C3 {}
const ng: number = new G();
