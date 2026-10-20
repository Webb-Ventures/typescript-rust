declare const p: Promise<number>;
declare function dec(a: any): any;
async function f1(a = await p) {}
async function f2({ b = await p }) {}
const f3 = async (a = await p) => {};
class C { async m(a = await p) {} }
async function f4(a = async () => await p) {}
async function f5(a = (await p) + 1) {}
function g(a = await p) {}
async function o() { function h(a = await p) {} }
function* y() {
  class D { m(@dec(yield 1) a: number) {} }
}
async function z() {
  class E { m(@dec(await p) a: number) {} }
}
