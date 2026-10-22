// Destructuring past the end of a tuple: TS2493, TS2339 past every tuple of a union.
declare const t: [number];
let a: any, b: any, c: any;
[a, b, c] = t;
const [d, e] = t;
declare const o: { x: any, y: any };
[o.x, o.y] = t;
const [d1, e1 = 1] = t;
const [, e2] = t;
const [d3, ...r3] = t;
function f([p, q]: [number]) {}
declare const t2: [number, string?];
const [g1, g2, g3] = t2;
declare const t3: [number, ...string[]];
const [h1, h2, h3] = t3;
declare const u: [number] | [number, string];
const [i1, i2, i3] = u;
declare const ro: readonly [number];
const [j1, j2] = ro;
const [[k1, k2]] = [t] as [[number]];
[a, b = 2] = t;
for (const [m1, m2] of [t]) {}
const { 0: n0, 1: n1 } = t;
namespace Literals {
const [a, b] = [1];
const [c, d = 2] = [1];
const [[e, f]] = [[1]];
function g([h, i] = [1]) {}
function g2([h, i]: [number] = [1]) {}
const [j, k]: [number] = [1];
let l: any, m: any;
[l, m] = [1];
declare const tt: [number];
const [n, o] = tt as [number];
function g3(...[p, q]: [number]) {}
declare function cb(f: (x: [number]) => void): void;
cb(([r, s]) => {});
const { 0: t0, 1: t1 } = [1] as [number];
const [u0, [u1, u2]] = [1, [2]] as [number, [number]];
}
export {};
