// `export as namespace X` is dropped when a global `X` already exists (lib's Promise, an
// earlier `declare var`, an earlier interface).
// @filename: g.d.ts
declare var Foo: { a: number };
interface Bar { b: number }
// @filename: u.d.ts
export as namespace Promise;
export declare const ux: number;
// @filename: v.d.ts
export as namespace Foo;
export declare const vx: number;
// @filename: w.d.ts
export as namespace Bar;
export declare const wx: number;
// @filename: s.ts
/// <reference path='u.d.ts' />
/// <reference path='v.d.ts' />
/// <reference path='w.d.ts' />
const a: number = Foo.a;
const b: number = Foo.vx;
const c: number = Bar.wx;
let d: Bar = { b: 1 };
const e: number = Promise.ux;
