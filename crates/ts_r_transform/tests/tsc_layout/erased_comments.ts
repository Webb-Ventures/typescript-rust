// @target: es2022
// @module: commonjs
// @useDefineForClassFields: false
declare function D(): void; // c1
interface I { } // c2
type T = 1; // c3
function f(a: number): void; // c4
function f(a: any) { } // c5
class C {
    m(): void; // c6
    m() { } // c7
    x: number; // c8
    declare y: number; // c9
}
let v: number = 1; // c10
import { type Q } from "./q"; // c11
export type { I }; // c12
/* c13 */ interface J { }
f(1);
function fn(/*u1*/ a: number /*u2*/, /*u3*/ b /*u4*/: string /*u5*/) /*u6*/: void /*u7*/ { }
var m = v as /*s1*/ any;
var n = <any>/*s2*/v;
