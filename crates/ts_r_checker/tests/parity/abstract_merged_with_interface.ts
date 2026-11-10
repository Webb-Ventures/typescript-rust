abstract class A {
    abstract m(): void;
}
interface A {
    m(): void;
    n(): void;
}
class B extends A {
    m() { super.m(); }
    o() { super.n(); }
}
abstract class C { abstract foo(): void; }
interface C { foo(x: number): void; }
abstract class D {
    abstract bar(): void;
    bar(x?: number): void;
}
interface E { e(): void }
abstract class E { abstract e(): void; e2() { } }
