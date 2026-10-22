// Rest elements of destructuring assignments receive the rest of the source as an array
// (also of other iterables, whose elements may be missing under noUncheckedIndexedAccess).
declare const nums: number[];
declare const it: Iterable<number>;
declare const tup: [number, string, boolean];
let a: number, b: string[];
[a, ...b] = nums;
[a, ...b] = it;
[a, ...b] = tup;
let c: [string, boolean];
[a, ...c] = tup;
declare const it2: Iterable<number>;
const [x, ...r] = it2;
const y: number = x;
const z: string[] = r;
function f([p]: Iterable<number>) { const q: number = p; }
var cc = { bogus: 0 };
[...cc] = ["", 0];
let tt: (string | [string, string])[];
[...tt] = ["t", ["a", "b"]];
type E = B | "t";
type B = ["and", ...E[]] | ["not", E];
declare function ev(e: E): boolean;
function g(e: B) {
  const [op, ...rest] = e;
  if (op === "and") rest.every((c) => ev(c));
}
