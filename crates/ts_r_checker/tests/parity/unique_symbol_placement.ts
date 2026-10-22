// `unique symbol` placement and operand grammar (TS1005, TS1330-TS1335); for-of
// annotations (TS2483).
// @filename: a.ts
declare const a: unique number;
let b: unique symbol | 1;
type T = unique symbol[];
type U = unique keyof X;
declare const c: unique (symbol);
type V = unique unique symbol;
declare const d: unique symbol;
class C { static readonly s: unique symbol; declare static readonly t: unique symbol; static readonly [x]: unique symbol; }
interface I { readonly a: unique symbol; readonly [k: string]: unique symbol; m(): unique symbol }
for (const e: unique symbol of []) {}
export const f: unique symbol = Symbol();
namespace N { export declare const g: unique symbol; }
declare const h: typeof d | unique symbol;
const {i}: unique symbol = null as any;
declare const j: unique symbol, k: unique symbol;
using l: unique symbol = null as any;
let o = { p: null as unknown as unique symbol };
class D { static readonly accessor q: unique symbol; readonly static r: unique symbol; static readonly "s": unique symbol; static readonly #t: unique symbol;}
// @filename: b.ts
declare const a: (unique symbol);
let {b}: unique symbol = null as any;
for (let c: unique symbol;;) {}
for (const d: unique symbol = null as any;;) {}
class C { static readonly e?: unique symbol; static f: unique symbol; readonly g: unique symbol; h: unique symbol }
declare const o: { readonly a: unique symbol, b: unique symbol, readonly c?: unique symbol };
const K = class { static readonly k: unique symbol; };
function f(): void { const z: unique symbol = Symbol(); let y: unique symbol; }
export declare const [m]: unique symbol;
declare var v: unique symbol, w: unique number;
type P = { readonly [K in "a"]: unique symbol };
interface J { a: unique symbol | 1; }
declare const X: any[];
for (const [a]: any of X) {}
for (let {b}: any of X) {}
for (var c: any of X) {}
for (const d: number = 1 of X) {}
async function f2() { for await (const e: any of X) {} }
for (const g: any, h of X) {}
// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ESNext", "module": "ESNext", "moduleResolution": "bundler", "noEmit": true}, "files": ["a.ts", "b.ts"]}
