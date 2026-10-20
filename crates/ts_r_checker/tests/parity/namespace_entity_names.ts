namespace a {
    export namespace b { export var x = 1; }
    export interface I { q: number }
    export class K { p = 1 }
    export enum E { A }
    export interface IMode { }
}
namespace c { }
interface T1 { q: number }
interface Empty { }
class C { static s = 1; p = 1; }
function g() { }
var v1 = { p: 1 };
enum Color { Red }
namespace NI { export interface J { } }
namespace Mods { export var m = 1; }

// import-equals entities
import i1 = a.b;
import i2 = a.zz;
import i3 = a.b.yy;
import i4 = c.q;
import i5 = nope.q;
import i6 = a.b.x;
import i7 = nope2;
import i8 = T1;
import i9 = v1;
import i10 = C;
import i11 = g.q;
import i12 = C.p;
import i13 = C.s;
import i14 = a.I.q;
import i15 = a.b.x.y;
import i16 = NI;
import i17 = i16.Z;
import i18 = globalThis;
import i19 = Color.Red.toString;
namespace d { import m7 = a.missing; }

// qualified type names
var t1: T1.q;
var t2: Empty.nope;
var t3: Empty.toString;
var t4: a.I.q;
var t5: C.s;
var t6: C.p;
var t7: g.q;
var t8: v1.p;
var t9: i16.Z;
var t10: a.K.z;
var t11: a.b.x.y;
var t12: a.E.A.foo;
var t13: Color.Red.toFixed;
var t14: a.Mode;
var t15: mods.m;
function h() {
    const NI = 1;
    var y1: NI.J;
}
