declare const o: { a: number };
let n: number;
for (n in o) { }
let s: string;
for (s in o) { }
let u: string | number;
for (u in o) { }
let a: any;
for (a in o) { }
let k: "a";
for (k in o) { }
for (var [x, y] in o) { }
for ({} in o) { }
for ([] in o) { }
for (let {p} in o) { }
for (var q: number in o) { }
let un: unknown;
for (un in o) { }
for (o.a in o) { }
declare const anyv2: any;
declare const rec: Record<string, number>;
declare const arr: number[];
declare const both: { a: 1, b: 2 };
let k2: "a";
for (k2 in anyv2) { }
for (k2 in rec) { }
for (k2 in arr) { }
let kb: "a" | "b";
for (kb in both) { }
let num: number;
for (num in arr) { }
function f<T extends object>(t: T) {
    let kk: keyof T;
    for (kk in t) { }
    let ks: string;
    for (ks in t) { }
    let kn: number;
    for (kn in t) { }
}
declare const ro: { readonly x: number };
for (ro.x in {}) {}
for (ro["x"] in {}) {}
