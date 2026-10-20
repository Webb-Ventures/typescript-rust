// Declaration-file modules export every top-level declaration unless they have an export
// clause or export assignment; missing imports of module locals get TS2459/TS2460; named
// imports from `declare module "x"` are checked; relative imports inside one are TS2439.
// @filename: base.d.ts
type A = 1;
declare class C {}
declare const v: number;
interface I {}
export class B {}
// @filename: b.d.ts
type A = 1;
export class B {}
export {};
// @filename: c.d.ts
type A = 1;
export class B {}
export { A as AA };
// @filename: d.d.ts
declare const x: number;
type A = 1;
export default x;
// @filename: f.d.ts
type A = 1;
export * from './base';
// @filename: g.d.ts
type A = 1;
export default class X {}
// @filename: h.d.ts
type A = 1;
export as namespace Foo;
export class B {}
// @filename: p.d.ts
type A = 1;
import y = require('./base');
export declare const q: number;
// @filename: amb.d.ts
declare module 'amb1' { type A = 1; export * from './base'; }
declare module 'amb3' { type A = 1; export default class X {} }
declare module 'amb5' { type A = 1; export {}; }
declare module 'amb6' { type A = 1; export const z: number; }
// @filename: m.ts
import { A, C, v, I, B } from "./base";
import { A as A2 } from "./b";
import { A as A3, AA } from "./c";
import { A as A4 } from "./d";
import { A as A5 } from "./f";
import { A as A6 } from "./g";
import { A as A7 } from "./h";
import { A as A8, y } from "./p";
import { A as A9 } from "amb1";
import { A as A10 } from "amb3";
import { A as A11, Q } from "amb5";
import { A as A12, z, zz } from "amb6";
const n: A = 1; const m: number = v; new C();
// @filename: loc.ts
type Abc = 1;
export type Abcdeg = 2;
type Abcdef = 3;
const w = 1;
export { w as w1, w as w2 };
// @filename: n.ts
import { Abc, Abcdef, w, nope } from "./loc";
