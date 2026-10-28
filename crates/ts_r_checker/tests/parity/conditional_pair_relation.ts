type Eq<A, B> = (<T>() => T extends A ? 1 : 2) extends (<T>() => T extends B ? 1 : 2) ? true : false;
const a: Eq<number[], string[]> = false;
const b: Eq<number[], [number, ...number[]]> = false;
const c: Eq<number[], number[]> = true;
const d: Eq<number[], string[]> = true;
function f<T>() {
  type One = T extends string ? string : string;
  type A = T extends number ? One : never;
  const x: One = null as any as A;
}
