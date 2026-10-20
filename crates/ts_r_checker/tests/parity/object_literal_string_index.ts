// Indexing an object literal expression (or its `as const`) by `string`/`number`
// reads any property or undefined; a missing literal key is TS2339.
declare const s: string;
declare const n: number;
const a = { H1: 1, H2: "x" }[s];
const a2: string = a;
const o = { a: 1 };
const b = o[s];
const c = ({ x: true } as const)[s];
const c2: boolean = c;
const d = { 1: "a" }[n];
const e = { a: 1 }["zz"];
const f = { a: 1 }[s as `x${string}`];
class K { a = 1 }
const g = new K()[s];
const h = ({} as { a: number })[s];
let w = { q: 1 };
w[s] = 2;
