// `keyof P` of a type parameter constrained by a string index signature accepts numbers
// too (`keyof { [k: string]: X }` is `string | number`): typebox's `TFromProperty`.
type Assert<T, E> = T extends E ? T : never;
type AK<T> = Assert<T, string | number>;
interface S { x: 1 }
type P = Record<string | number, S>;
type F1<Pr extends P, Key extends PropertyKey> = Pr[AK<Key>];
type F2<Pr extends { [k: string]: S }, Key extends PropertyKey> = Pr[Key extends string | number ? Key : never];
function f<K extends PropertyKey, Q extends { [k: string]: 1 }>(k: K & number, n: number, s: string) {
  const a: keyof Q = n;
  const b: keyof Q = k;
  const c: keyof Q = s;
  const d: keyof Q = Symbol();
}
// A mapped `Record<string, X>` has only `string` keys.
type F3<Pr extends Record<string, S>, Key extends PropertyKey> = Pr[Key extends string | number ? Key : never];
export { f };
