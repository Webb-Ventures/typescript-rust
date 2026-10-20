// A generic tuple read by a numeric literal in an expression: a required prefix element is
// that element; at a single variadic `...T` (then required elements) it is `T[number]`
// joined with those (`[...T][0]` is `T[number]`). The type-level `[...T][0]` defers.
interface Item { _def: { n: number } }
export function a<TS extends Item[]>(...list: [...TS]) {
  const x = list[0]?._def.n;
  const y: string = list[0]?._def.n;
}
export function b<TS extends Item[]>(list: [string, ...TS]) {
  const c0: boolean = list[0];
  const c1: boolean = list[1];
  const c2: boolean = list[2];
}
export function c<TS extends Item[]>(list: [...TS, string]) {
  const c0: boolean = list[0];
}
export function d<TS extends unknown[]>(list: [...TS]) {
  const c0: boolean = list[0];
}
export function e<T extends [Item, ...Item[]]>(list: [...T]) {
  const c0: boolean = list[0];
}
type Head<T extends unknown[]> = [...T][0];
const h: Head<[number, string]> = "x";
