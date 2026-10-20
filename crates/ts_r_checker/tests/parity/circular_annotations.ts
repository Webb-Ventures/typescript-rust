var a: typeof a;
var b: typeof c;
var c: typeof b;
var d: Array<typeof d>;
var e: { x: typeof e };
var f: number = f;
var g: () => typeof g;
var h: (typeof h)[];
var i: [typeof i];
var j: typeof j | string;
var k: typeof k.x;
interface I { p: number }
type T1 = { get foo(): T1["foo"] };
type T2 = { p: T2["p"] };
type T3 = { p: T3["q"]; q: T3["p"] };
class C { p: C["p"]; q: typeof C.prototype.q; static s: typeof C.s; }
function fn(x: typeof x) {}
declare const n: { a: typeof n.a };
const p: typeof p = 1 as any;
class K<T> { x!: T }
interface IK<T> { x: T }
type A6 = K<A6b>; type A6b = typeof a6; var a6: A6[];
type A7 = K<typeof a7>; var a7: A7;
var a8: K<typeof a8>;
var a9: IK<typeof a9>;
type A14 = K<A14b>; type A14b = A14c | number; type A14c = typeof a14; var a14: [string, A14[]];
type A15 = K<A15b>; type A15b = A15c | number; type A15c = typeof a15; var a15: A15[];
type A16 = K<A16b>; type A16b = A16c | number; type A16c = typeof a16; var a16: A16;
type A17 = K<A17c>; type A17c = typeof a17; var a17: A17;
var a18: Array<typeof a18>;
var a19: ReadonlyArray<typeof a19>;
var a20: Promise<typeof a20>;
var rc: typeof rc;
var rc: any;
var rd: typeof re;
var rd: any;
var re: typeof rd;
var re: any;
export {};
