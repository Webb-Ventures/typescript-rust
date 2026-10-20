// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ES2016", "lib": ["esnext"], "importHelpers": true, "module": "commonjs", "experimentalDecorators": true, "noEmit": true}, "files": ["f1.ts", "f2.ts", "f3.ts", "f4.ts", "f5.ts", "f6.ts", "f7.ts", "f8.ts", "f9.ts", "f10.ts", "f11.ts", "f12.ts", "f13.ts", "f14.ts", "f15.ts", "f16.ts", "f17.ts", "f18.ts", "f19.ts", "f20.ts", "f21.ts"]}
// @filename: f1.ts
// Without a tslib, the first syntax that needs a helper is TS2354, once per file.
export {};
const a = async () => 1;
const b = async () => 2;
// @filename: f2.ts
export {};
class C { async m() {} }
// @filename: f3.ts
export {};
const f = async function () {};
// @filename: f4.ts
export {};
class P { #x = 1; get() { return this.#x; } }
// @filename: f5.ts
export {};
declare const dec: any;
class D { @dec m() {} }
// @filename: f6.ts
export {};
declare const o: any;
const { a, ...rest } = o;
// @filename: f7.ts
export {};
declare const o: any;
let r: any;
({ ...r } = o);
// @filename: f8.ts
export {};
async function* g() { yield 1; }
// @filename: f9.ts
export {};
{ using u = null; }
// @filename: f10.ts
export {};
class Q { #m() {} n(o: Q) { return #m in o; } }
// @filename: f11.ts
export {};
declare const dec: any;
@dec class E {}
// @filename: f12.ts
export {};
declare const dec: any;
class F { constructor(@dec x: number) {} }
// @filename: f13.ts
// Nothing here needs a helper at ES2016.
export {};
const o2 = { ...{ a: 1 } };
const xs = [...[1]];
function rr(...a: any[]) {}
class G { x = 1; static y = 2; static { } accessor z = 1; }
// @filename: f14.ts
// Ambient code needs no helpers.
export {};
declare class Z { m({ a, ...r }: any): void; }
declare function q({ a, ...r }: any): void;
declare const dec: any;
// @filename: f15.ts
// Not a module.
const s = async () => 1;
// @filename: f16.ts
export default async function () {}
// @filename: f17.ts
export {};
const obj = { async m() {} };
// @filename: f18.ts
export {};
function k(x: number, { a, ...r }: any) {}
// @filename: f19.ts
export {};
class S { #x = 1; m() { this.#x += 2; } }
// @filename: f20.ts
import * as ns from "./f19";
export * from "./f18";
ns;
// @filename: f21.ts
export {};
async function h(xs: any) { for await (const x of xs) {} }
