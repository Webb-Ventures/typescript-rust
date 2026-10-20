// Destructuring assignments and `for (target of xs)` check each target against the part of
// the source it receives (narrowed when the source is a reference); a target that is not
// a reference is TS2364.
export {};
var v: string;
for (v of [0]) { }
for (v of [0, ""]) { }
let a: string, b: number;
[a, b] = [1, "x"];
[a, b] = ["s", 2];
declare const arr: number[];
[a] = arr;
[a = "d"] = arr;
({ a, b } = { a: 1, b: 2 });
({ a: b } = { a: "s" });
({ a = 1 } = { a: "s" });
for ([a, b] of [[1, "x"]]) { }
for ({ a } of [{ a: 1 }]) { }
let c: string | undefined;
[c = "x"] = [undefined];
const t: [number, string] = [1, "s"];
[b, a] = t;
[a, b] = t;
function f2(obj: [number, string] | [null, null]) {
  let c0: number, c1: string;
  if (obj[0] && obj[1]) {
    [c0, c1] = obj;
  }
  [c0, c1] = obj;
}
declare function g(): number;
g() = 1;
for ([""] of [[""]]) { }
let n = 1;
(n as any) = 2;
