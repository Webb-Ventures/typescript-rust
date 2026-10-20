// `t || u` with a generic `t` is `U | NonNullable<T>`; the left operand of `&&` has no
// contextual type, so a union-constrained `T` there keeps its type parameter.
export {};
function fn1<T, U>(t: T, u: U) {
    var r1 = t || t;
    var r2: T = t || t;
    var r3 = t || u;
    var r4: {} = t || u;
    var q1: number = t || u;
    var q2: number = t && u;
    var q3: number = t ?? u;
}
function fn3<T extends { a: string } | undefined, U extends string | null>(t: T, u: U) {
    var q1: number = t || u;
    var q2: number = u || t;
    var q3: number = t && u;
}
declare const s: string | undefined;
declare const n: number;
var z1: boolean = s || n;
var z2: boolean = n || s;
