declare let _: any;
declare const g1: Generator<number, void, string>;
declare const g2: Generator<number, void, undefined>;
[...g1];
[...g2];
let [x1] = g1;
let [x2] = g2;
[_] = g1;
for (_ of g1);
for (_ of g2);
for (const c of "ab");
for (const n of [1, 2]);
async function f() {
    for await (_ of g1);
}
export {};
