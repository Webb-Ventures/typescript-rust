class Base<T, U> { x!: T; y!: U; }
interface BaseConstructor {
    new (x: string, y: string): Base<string, string>;
    new <T>(x: T): Base<T, T>;
    new <T>(x: T, y: T): Base<T, T>;
    new <T, U>(x: T, y: U): Base<T, U>;
}
declare function getBase(): BaseConstructor;
class D2 extends getBase() <number> { }
class D3 extends getBase() <string, number> { }
interface BadBaseConstructor {
    new (x: string): Base<string, string>;
    new (x: number): Base<number, number>;
}
declare function getBadBase(): BadBaseConstructor;
class D5 extends getBadBase() { }
declare const Same: { new (x: string): Base<string, string>; new (x: number): Base<string, string> };
class D6 extends Same { }
declare const Ctor: typeof Base;
class D7 extends Ctor<string, number> { }
interface M { m: number }
declare function Mix<T extends new (...a: any[]) => {}>(b: T): T & (new (...a: any[]) => M);
class D8 extends Mix(Base)<string, string> { }
declare const Arr: ArrayConstructor;
class D9 extends Arr<number> { }
