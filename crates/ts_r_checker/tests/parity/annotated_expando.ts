// Expando properties of an annotated const type its function initializer; ones the annotation declares take the annotation's type and contextual type.
interface TestFn {
  (t: string): number;
  only: (t: string) => number;
  skip(t: string): number;
}
export function mk(): TestFn {
  const f: TestFn = (t: string) => 1;
  f.only = (t: string) => 2;
  f.skip = (t: string) => 3;
  return f;
}
export function mk2(): TestFn {
  const g = (t: string) => 1;
  g.only = (t: string) => 2;
  return g;
}
const h = (t: string) => 1;
h.only = 1;
export const n: string = h.only;
const k: () => void = () => {};
k.x = 1;
export const kx = k.x;
const m: TestFn = (t: string) => 1;
m.only = (t: string) => 2;
export const mo: string = m.only;
