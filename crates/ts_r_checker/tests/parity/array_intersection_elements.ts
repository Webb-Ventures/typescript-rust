// Iterating or destructuring an intersection of arrays gives the intersection of their
// element types (playwright's testTree.ts `GroupItem['children']`); array methods use
// the first array's signatures.
declare const v: { x: number }[] & { y: string }[];
for (const c of v) {
  const t: number = c.y;
  const u: string = c.x;
}
const [g] = v;
const h: number = g!.y;
declare const w: string[] & number[];
for (const c of w) {
  const t: boolean = c;
}
type A = { kind: 'a'; x: number };
type B = { kind: 'b'; y: string };
type C = { kind: 'c' };
declare const k: (A | B | C)[] & (A | B)[];
for (const c of k) {
  const t: 'a' | 'b' = c.kind;
}
k.forEach(c => { const t: 'a' | 'b' = c.kind; });
