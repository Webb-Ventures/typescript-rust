// @target: es2015
// @module: esnext
declare let p: Promise<number>, it: AsyncIterable<number>, xs: number[];
async function* g1() { await p; yield 1; }
async function* g2(a: number) {
    const x = yield* g1();
    yield await p;
    return 5;
}
const g3 = async function* () { yield 1; };
class C {
    async *m() { yield 1; }
}
async function f1() {
    for await (const x of it) { console.log(x); }
}
async function f2() {
    for await (let [a, b] of it) { }
    for await (x1 of xs) { }
}
async function* g4() {
    for await (const x of it) { yield x; }
}
const o1 = { a: 1, ...xs, b: 2 };
const o2 = { ...xs };
const o3 = { ...xs, ...o1 };
