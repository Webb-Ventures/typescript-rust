// A for-in variable is a `string`: indexing a concrete object without a string index
// signature is implicitly `any` (TS7053); over a type whose only index signature is numeric
// (arrays, tuples) the variable indexes with `number`.
const obj = { a: 1 };
for (const k in obj) { obj[k]; }
function u(o: { a: 1 } | { b: 2 }) {
  for (const k in o) { o[k]; }
}
function r(o: Record<string, number>) {
  for (const k in o) { const n: string = o[k]; n; }
}
let m = { x: 1 };
for (var k2 in m) { m[k2]; }
declare const ro: readonly string[];
for (const i in ro) { const s: number = ro[i]; s; }
const tup: [string, number] = ["a", 1];
for (const i in tup) { const s: boolean = tup[i]; s; }
declare const both: { [k: number]: string; [k: string]: string | number };
for (const i in both) { const s: string = both[i]; s; }
