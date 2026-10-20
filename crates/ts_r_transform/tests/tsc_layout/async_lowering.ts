// @target: es2015
// @module: esnext
declare let p: Promise<number>;
async function f1() { await p; }
async function f2(a: number, b = 1, ...c: number[]) {
    const x = await p;
    return x + a;
}
async function f3() { return arguments.length; }
const a1 = async () => { await p; };
const a2 = async (x: number) => await p + x;
const a3 = async () => this;
const a4 = async function () { return this; };
const a5 = async function named() { return arguments; };
class C {
    async m() { await p; return this; }
    static async s() { return super.toString(); }
    async n() { return super.toString(); }
    async o() { super.x = 1; return super["y"]; }
}
const o = {
    async m() { await p; },
    async ["k"]() { },
};
function outer() {
    return async () => arguments[0];
}
function outer2() {
    const f = async () => this;
}
async function f4({ a }: any, [b]: any) { await a; }
async function f5(x = await1()) { }
declare function await1(): any;
export default async function () { }
