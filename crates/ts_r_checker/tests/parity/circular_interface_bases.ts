interface Foo extends Foo {}
interface Foo2<T> extends Foo2<T> {}
interface Foo3<T> extends Foo3<string> {}
interface A extends B {}
interface B extends C {}
interface C extends A {}
interface D extends A {}
interface E extends E, F {}
interface F { x: number }
interface G<T> extends H<T> {}
interface H<T> extends G<T[]> {}
interface J extends K {}
interface J extends L {}
interface K {}
interface L extends J {}
type M1 = M2;
interface M2 extends M1 {}
interface N extends Array<N> {}
class X1 implements X1 {}
interface Y extends Z {}
class Z implements Y {}
class P extends Q {}
class Q extends P {}
interface R extends S {}
class S extends R2 {}
interface R2 extends S {}
declare class T1 extends T2 {}
interface T2 extends T1 {}
interface J2 extends J2 {}
interface J2 { x: number }
declare class CC {}
interface CC extends CC {}
namespace NS { export interface Q extends NS.Q {} }
export {};
