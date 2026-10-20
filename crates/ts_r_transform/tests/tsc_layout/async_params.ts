// @target: es2015
// @module: esnext
declare let p: Promise<number>;
const a1 = async (a = 1, { b }: any = {}) => { await p; };
const a6 = async ({ b }: any) => b;
async function f2(a: number, b = 1) { return arguments[0]; }
async function* g1() { yield; return; }
class B { m() { return 1; } }
class C extends B {
    async *g() { yield super.m(); }
}
async function f9() {
    if (p) {
        return;
    }
}
function* gen() { const x = await p; }
async function f10() {
    "use strict";
    await p;
}
async function f11() {
    // comment
    await p; // trailing
}
