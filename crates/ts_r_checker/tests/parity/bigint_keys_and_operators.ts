type Q = 6n | 7n | 8n;
type T = { [t in Q]: string };
type U = { [t in boolean]: string };
type V = { [t in string]: string };
declare const big: bigint;
const obj = { 1n: 1, a: 2 };
interface WithBig { 2n: string; b: number }
class KBig { 3n = 0; c = 1 }
const w: WithBig = { b: 1 };
let s1 = +big;
function plus<S extends number | bigint>(x: S) { return +x; }
let sh = big >>> 1n;
let mixed = "s" & 1n;
enum Shift { A = 1 << 32, B = 1 << -33, C = (8 >> 40) }
export {};
