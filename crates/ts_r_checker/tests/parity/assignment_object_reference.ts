// A condition on `(x = f()).p` (or `(x = f())?.p`) tests `x.p`. Equality and `typeof`
// tests of `(x = f())?.p` don't rule out a nullish `x`; truthiness does.
type Id = { type: "Identifier"; name: string };
type Pat = { type: "Pattern"; elems: number[] };
type Decl = { id: Id | Pat; init: number | null };
declare const ds: Decl[];
declare function last(): Decl | undefined;
declare function one(): Decl;
export function f() {
  let top: undefined | Decl = undefined;
  if ((top = last())?.id.type === "Identifier" && top?.id.name === "x") {
    return 1;
  }
  let t2: undefined | Decl = undefined;
  if ((t2 = ds[0])?.id.type === "Identifier" && t2.id.name === "x") {
    return 2;
  }
  let t3: undefined | Decl = undefined;
  if ((t3 = (ds as any).at(-1))?.id.type === "Identifier" && t3?.id.name === "x") {
    return 3;
  }
  return 0;
}
export function g() {
  let e: Decl | undefined;
  if ((e = last())?.id.type === "Identifier") {
    e.id;
  }
  let g: Decl | undefined;
  if ((g = last())?.init === 1) {
    g.init;
  }
  let h: Decl | undefined = last();
  if (h?.init === 1) {
    h.init;
  }
  let i: Decl | undefined;
  if ((i = last())?.init) {
    i.init;
  }
  let j: Decl | undefined;
  if ((j = last())?.init !== undefined) {
    j.init;
  }
  let k: Decl | undefined;
  if (typeof (k = last())?.init === "number") {
    k.init;
  }
  let m: Decl | undefined;
  if ((m = last())?.id.type === "Identifier") {
    m?.id.name;
  }
  let n: Decl | undefined;
  if ((n = one()).id.type === "Identifier") {
    n.id.name;
  }
  let o: Decl | undefined;
  if ((o = one()).id.type === "Pattern") {
    o.id.name;
  }
}
