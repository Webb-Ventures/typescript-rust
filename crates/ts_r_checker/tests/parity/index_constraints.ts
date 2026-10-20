// Index signature constraints (TS2411/TS2413): static index signatures, computed keys
// (literal, unique symbol and dynamic), enum member types, indexes implied by dynamic
// entity-name keys, inherited class members and duplicate implementations.
declare var s: string;
declare var n: number;
class Foo { x = 1 }
class Foo2 { x = 1; y = 2 }
class X {
    static [index: string]: string;
    static x = 12;
    static foo() {}
}
const xs: number = X["foo"];
enum E { A }
interface I3 {
    [x: string]: string;
    foo: E;
}
interface I5 {
    [x: string]: boolean;
    foo: E.A;
}
class C {
    [s: string]: Foo2;
    get [1 << 6]() { return new Foo }
    set [1 << 6](p: Foo2) { }
    [""]() { return new Foo }
    ["get1"]: Foo = new Foo;
}
interface S {
    [Symbol.iterator]: number;
    [s: symbol]: string;
}
class Implied {
    [s]: number = 1;
    [n]: number = 1;
    [+s]: string = "";
    x: string = "";
}
class NumberOnly {
    [n]: number = 1;
    [s + s]: string = "";
    [n + 1]: string = "";
}
declare const im: Implied;
const z1: null = im["q"];
class Accessors {
    [s]() { }
    get [s]() { return 1 }
    get [s + s]() { return 1 }
}
class Base {
    get ["get1"]() { return new Foo }
    z: string = "";
}
class Derived extends Base {
    [s: string]: Foo2;
    set ["set1"](p: Foo) { }
}
class Dup {
    [s: string]: () => Foo2;
    a() { return new Foo }
    a() { return new Foo2 }
}
class SB {
    static readonly [s: string]: number;
    static readonly [s: number]: 42 | 233;
}
class SD extends SB {
    static readonly [s: string]: boolean;
    static readonly [s: number]: 1;
}
interface H {
    [n: number]: Foo;
}
interface HI extends H {
    [s: string]: Foo2;
}
