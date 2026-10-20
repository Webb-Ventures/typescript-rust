// Classes whose extends chain leads back to them: TS2506 (not those extending into it).
declare namespace N {
    class A extends C {}
    class B extends A {}
    class C extends N.B {}
    class D extends C {}
}
declare class E<T> extends E<T> {}
export {};
