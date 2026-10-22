// An intersection of classes declaring the same property separately, private in one, is
// `never`; accesses, assignments and `extends` say why.
class A { private x: unknown; y?: string; }
class B { private x: unknown; y?: string; }
declare let ab: A & B;
ab.y = 'hello';
ab = {};
declare let kk: { k: "a" } & { k: "b" };
kk = { k: "a" };
kk.k;
class C { protected x: unknown; }
class D extends C { }
declare let cd: C & D;
cd = {} as any;
class E { x: unknown; }
declare let ae: A & E;
ae.y;
let n: never = ab;
class P1 { private p = 1; }
class P2 { private p = 2; }
declare function Mix<A, B>(a: new () => A, b: new () => B): new () => A & B;
class C1 extends Mix(P1, P2) { }
