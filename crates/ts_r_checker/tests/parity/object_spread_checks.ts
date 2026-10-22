declare let ab: { a: number, b: number };
declare let abq: { a: number, b?: number };
let o1 = { b: 1, ...ab };
let o2 = { b: 1, ...abq };
let o3 = { ...ab, b: 1 };
function h(obj: { x: number } | { x: string }) {
    return { x: 1, ...obj };
}
function f(obj: { x: number } | undefined) {
    return { x: 1, ...obj };
}
let n = { ...12 };
n.toFixed();
let z = { ...0 };
let s = { ...'foo' };
declare const nu: null | undefined;
let u = { ...nu };
function g1<T extends number>(arg: T) {
    return { ...arg };
}
function g2<T>(arg: T, b: T & undefined) {
    return [{ ...arg }, { ...b }];
}
class P { private x?: number; }
declare let pub: { x: number };
declare let priv: P;
let o4 = { ...pub, ...priv };
o4.x;
let o5 = { ...{ set b(_v: number) { } } };
o5.b = 12;
class K { p = 12; m() { } get g() { return 0; } }
let sk = { ...new K() };
sk.g;
sk.p;
function t(x?: { a: number } | null) {
    let y = { ...x };
    let c: number = y.a;
}
export { o1, o2, o3, h, f, n, z, s, u, g1, g2, o4, o5, sk, t };
