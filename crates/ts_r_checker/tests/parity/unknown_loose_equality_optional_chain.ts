// A loose comparison never narrows `unknown` (but with null); `u?.x` reads `{}`.
enum E { A, B }
const enum S { X = "x" }
declare const sym: unique symbol;
declare const u: unknown;
if (u == "a") { const a: number = u; }
if (u == E.A) { const b: string = u; }
if (u == S.X) { const c: number = u; }
if (u == sym) { const d: number = u; }
if (u == 1n) { const e: number = u; }
if (u == false) { const f: number = u; }
if (u == E.A) {} else { const g: number = u; }
u == 42 ? u : 0;
export function h(err: unknown, t: unknown, v: unknown, w: unknown) {
  const a = err?.toString();
  const b = t?.foo;
  const c = v?.["x"];
  const d: string = w!;
  return [a, b, c, d];
}
