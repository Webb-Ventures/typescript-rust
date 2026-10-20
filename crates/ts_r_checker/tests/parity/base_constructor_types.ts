var x: {};
function foo() {}
class A { a = 1; }
class B { b = 1; }
declare const cond: boolean;
class C1 extends undefined { }
class C2 extends null { }
class C3 extends 42 { }
class C4 extends x { }
class C5 extends foo { }
class C6 extends (cond ? A : B) { }
type Ctor<T = {}> = new (...args: any[]) => T;
function mix<T extends Ctor>(Base: T) { return class extends Base { m = 1; }; }
function mix2<T>(Base: T) { return class extends Base { m = 1; }; }
declare const anyv: any;
class C7 extends anyv { }
declare const ab: abstract new () => object;
class C8 extends ab { }
declare const inter: (new () => A) & { s: number };
class C9 extends inter { }
declare const un: typeof A | typeof B;
class C10 extends un { }
class C11 extends Missing { }
declare const fnOrCtor: { (): void; new (): A };
class C12 extends fnOrCtor { }
declare const unk: unknown;
class C13 extends unk { }
declare const nev: never;
class C14 extends nev { }
const obj = { A };
class C15 extends obj.A { }
class C16 extends obj { }
export {};
