// Overloads merged across interface declarations: later declarations first,
// each declaration's own overloads in order. (tsc prints the merged type
// literal in declaration order; we print it in resolution order.)
const k: unique symbol = Symbol();
const j: unique symbol = Symbol();
interface Box {
  [k](a: string): 1;
  [k](a: number): 2;
  [k](a: boolean, b: string): 3;
}
interface Box {
  [k](a: 1[]): 4;
}
declare const b: Box;
const r1: 1 = b[k]("x");
const r2: 2 = b[k](1);
const r4: 4 = b[k]([1]);
b[k]({});

interface Named {
  m(a: string): 1;
  m(a: number): 2;
}
interface Named {
  m(a: boolean): 3;
  m(a: null): 4;
}
declare const n: Named;
const n1: 1 = n.m("x");
const n3: 3 = n.m(true);
n.m({});
