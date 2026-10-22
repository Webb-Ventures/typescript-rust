// An interface extending an intersection gives its members its own `this`.
interface Thing1 {
    a: number;
    self(): this;
}
interface Thing2 {
    b: number;
    me(): this;
}
type Thing3 = Thing1 & Thing2;
type Thing4 = Thing3 & string[];
interface Thing5 extends Thing4 {
    c: string;
}
interface Thing6 extends Thing3 {
    c: string;
}
interface Thing7 extends Thing1, Thing2 {
    c: string;
}
function f2(t: Thing5, u: Thing6, v: Thing7) {
    t = t.self();
    u = u.self();
    v = v.self();
}
function f1(t: Thing3, u: Thing4) {
    t = t.self();
    t = t.me().self().me();
    u = u.me().self();
    const n: number = u.me().length;
}
