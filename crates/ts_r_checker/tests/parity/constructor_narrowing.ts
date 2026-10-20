// `x.constructor === C` narrows `x` to the members `C` constructs (true branch only).
class C1 { p1!: string; }
class C2 extends C1 { p2!: number; }
class C3 { p1!: string; }

declare let a: C1 | number;
if (a.constructor === C1) {
    a.p1;
} else {
    a.p1;
}
if (C1 == a["constructor"]) {
    a.p1;
}
if (a.constructor !== C1) {
    a.p1;
} else {
    a.p1;
}

declare let b: C2 | string;
if (b.constructor === C1) {
    b.p1;
}
if (b.constructor === C2) {
    b.p2;
}
declare let c: C1 | string;
if (c.constructor === C3) {
    c;
}

let v: string | number | boolean | any[];
if (v.constructor === String) {
    const s: number = v;
}
if (v.constructor === Array) {
    const s: number = v;
}
