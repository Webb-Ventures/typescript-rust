declare let a1: () => { x: string };
declare let a2: () => { x: number };
a1 = a2;
declare let b1: () => () => { x: string };
declare let b2: () => () => { x: number };
b1 = b2;
declare let c1: { f(): { x: string } };
declare let c2: { f(): { x: number } };
c1 = c2;
declare let d1: { f: { g(): string } };
declare let d2: { f: { g(): number } };
d1 = d2;
declare let e1: { f(): () => string };
declare let e2: { f(): () => number };
e1 = e2;
declare let f1: () => { x: () => string };
declare let f2: () => { x: () => number };
f1 = f2;
declare let g1: (a: number) => { x: { y: string } };
declare let g2: (a: number) => { x: { y: number } };
g1 = g2;
declare let h1: { a: { b: string } };
declare let h2: { a: { b: number } };
h1 = h2;
declare let i1: { a: (cb: (x: number) => void) => void };
declare let i2: { a: (cb: (x: string) => void) => void };
i1 = i2;
interface L1 { forEach(cb: (item: number) => boolean): void }
interface L2 { forEach(cb: (item: number) => void): void }
declare let l1: L1;
declare let l2: L2;
l2 = l1;
declare let m1: { a: (cb: () => void) => void };
declare let m2: { a: (cb: () => boolean) => void };
m2 = m1;
function foo(x: "hi", items: string[]): typeof foo { return foo; }
const bar: (x: "hi", items: string[]) => number = foo;
class P { m = 1 }
declare let pc: typeof P;
pc = class { m = "s" };
class Q { m = 1 }
const o = { k: class { m = "" } };
const a: typeof Q = o.k;
let x: any;
x = class { m = "" };
const b: typeof Q = x;
const c: typeof Q = (class { m = "" });
function f(q = class { m = "" }) { const d: typeof Q = q; }
const o2 = { ["z"]: class { m = "" } };
const e: typeof Q = o2.z;
let y = 1 as any; y ||= class { m = "" };
export default class { m = "" }
