// Keys that cannot index: symbol keys of binding patterns without a matching member, and
// non-key types on `any` and `unknown` (TS2538).
var a: any[[]];
var b: any[{}];
var c: any[boolean];
var d: any[string];
var e: any[undefined];
var f: any["x" | 1];
var g: any[object];
var h: any[null];
var i: unknown[[]];
const numIndexed: { [idx: number]: string } = null as any;
const symed = Symbol();
let symed2 = Symbol();
const withSym = { [symed]: 1 };
let { [symed]: p1 } = numIndexed;
let { [symed2]: p2 } = numIndexed;
let { [symed]: p3 } = withSym;
const symIndexed: { [k: symbol]: string } = null as any;
let { [symed2]: p4 } = symIndexed;
export {};
