// Element access with a key no property matches: TS7053, or the property error tsc picks.
interface J { abc: number; foof: number; hello: string }
declare const j: J;
j["zzz"];
j["abd"];
j["foo"];
j["helo"];
j["foo"] = 1;
const o = { foof: 0 };
o["foo"];
class C { foof = 1; static bar = 2; }
declare const c: C;
c["foo"];
c["bar"];
c['bar'];
declare const u: J | C;
u["foo"];

// Fresh object literals: each missing literal key is TS2339 when read.
({ a: 1 })["zzz"];
({ a: 1 })["zzz"] = 2;
function quux(id: 'a' | 'y' | 'z') {
  const r = {
      a: 1,
      b: ""
  }[id];
  return r;
}

// Primitives are indexed as their apparent types.
declare const n: number;
n["zzz"];
n["toFixd"];
declare const b: boolean;
b["zzz"];
declare const s: string;
s["zzz"];

// `const` enum objects.
const enum E { A, B = 2 }
declare const k: "A";
const e1 = E[k];
const e2 = E["A"];
const e3 = E["Z"];
const e4 = E[0];
const e5 = E;
const e6 = [E];
const e7 = (E).A;
declare function foo(x: any): void;
foo(E);
type T = typeof E;
export default E;
