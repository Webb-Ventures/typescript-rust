// @target: es2015
// @module: esnext
declare let p: Promise<number>;
const a3 = async () => this;
class B { x = 1; m() { return 1; } }
class C extends B {
    async o() { return super["y"] + super.m(); }
    async q(k: string) { return super[k](1); }
    async r() { const f = () => super.m(); super.x++; }
    async s() { [super.x] = [1]; }
}
const o = {
    __proto__: B.prototype,
    async m() { return super.m(); },
};
async function f6(this: any, a: number) { await p; }
async function f7() {
    var _a = 1;
    const g = async () => { await p; };
}
async function f8(a, b) { return arguments[0]; }
