// Narrowing through destructuring and aliases: sibling bindings of a destructured union
// (const, parameter and for-of patterns, inside closures too), a const alias of a
// discriminant narrowing its parent, and `switch (true)` clauses (earlier tests false).
type E = { type: "x"; payload: string } | { type: "y"; payload: number };
type A = { kind: "a"; a: string } | { kind: "b"; b: number };
function d1({ type, payload }: E) {
  if (type === "x") { const s: string = payload; }
  switch (type) { case "y": { const n: number = payload; } }
  if (type === "x") { const g = () => { const s: string = payload; }; }
  const run = () => { if (type === "x") { const s: string = payload; } };
}
function d2(e: E) {
  const { type, payload } = e;
  if (type === "x") { const s: string = payload; const s2: string = e.payload; }
}
function d3(v: A) {
  const { kind } = v;
  if (kind === "a") { const s: string = v.a; }
  const k = v.kind;
  if (k === "b") { const n: number = v.b; }
}
function d4([k, v]: ["s", string] | ["n", number]) {
  if (k === "s") { const s: string = v; }
}
function d5(es: E[]) {
  for (const { type, payload } of es) {
    if (type === "x") { const s: string = payload; }
  }
}
function d6({ type, payload }: E = { type: "x", payload: "" }) {
  if (type === "x") { const s: string = payload; }
}
type F = { ok: true; v: string } | { ok: false; v: number };
function d7({ ok, v }: F) {
  if (ok) { const s: string = v; } else { const n: number = v; }
}
// Not narrowed: `let`, an assigned binding, a default, a non-discriminant.
function n1(v: A) {
  let { kind } = v;
  if (kind === "a") { const s: string = v.a; }
}
function n2({ type, payload }: E) {
  type = "y" as "x" | "y";
  if (type === "x") { const s: string = payload; }
}
function n3({ type, payload = 1 }: E) {
  if (type === "x") { const s: string = payload; }
}
function n4(e: E) {
  const { type, payload } = e;
  if (typeof payload === "string") { const t: "x" = type; }
}
// switch (true)
function t1(x: string | number | boolean) {
  switch (true) {
    case typeof x === "string": { const s: string = x; break; }
    case typeof x !== "number": { const n: 1 = x; break; }
    case x === 1:
    case x === 2: { const k: 1 = x; break; }
    default: { const d: 1 = x; }
  }
}
function t2(x: string | number) {
  switch (true) {
    case typeof x === "string":
      return;
  }
  const n: number = x;
}
