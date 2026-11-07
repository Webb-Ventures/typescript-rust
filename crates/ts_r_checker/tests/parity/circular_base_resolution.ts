// TS2310 when resolving a class's or interface's base types needs its own members
// (type arguments, defaults or indexed accesses reading it). A cycle of class names is
// TS2506 only.
type M<T> = { value: T };
interface J<T> { j: T }
interface K1 extends J<K1["x"]> { x: number }
class BaseType<T> { bar!: T }
class L1 extends BaseType<L1["q"]> { q = 1 }
class N3<C extends { someProp: any }, T = C['someProp']> { bar!: T }
class Foo3 extends N3<Foo3> { someProp!: { test: true } }
interface I1 extends I2 {}
interface I2 extends I1 {}
interface Node2 extends Array<Node2> {}
class G<T extends G<T>> {}
class H extends G<H> {}
let m: M<number> = { value: 1 };
class A1 extends B1 {}
class B1 extends A1 {}
