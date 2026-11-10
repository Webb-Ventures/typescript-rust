// A concrete intersection indexed by a generic key relates through the intersection
// indexed by the key's constraint, even when one member lacks some keys
// (intersectionsOfLargeUnions: `ElementTagNameMap[T]`).
interface A { a: string; b: number }
interface B { b: number; c: boolean }
type EM = A & Pick<B, Exclude<keyof B, keyof A>>;
type EM3 = A & B;
declare function get<T extends keyof EM>(t: T): EM[T];
declare function get3<T extends keyof EM3>(t: T): EM3[T];

export function f<T extends keyof EM>(key: T) {
  const b: 1 = get(key);
  return b;
}
export function h<T extends keyof EM3>(key: T, x: EM3[T]) {
  const b: 1 = get3(key);
  const c: string | number | boolean = x;
  return [b, c];
}
