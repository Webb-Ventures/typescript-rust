namespace A {
    export function B<T>(x: T) { return x; }
    export namespace N { export type T = number; }
    export namespace V { export const v = 1; }
    export const c = 1;
    export class K { }
}
var b: A.B<number>;
var n: A.N;
var v: A.V;
var cc: A.c;
var k: A.K;
var nn: A.N.T;
var vv: A.V.v;
