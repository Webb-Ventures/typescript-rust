// date-fns `buildMatchFn` (`args.matchPatterns[width]` on an intersection of mapped types
// over type parameters): an element access with a concrete key reads the object's apparent
// properties (`PP<V> | undefined`), not a deferred `PPs<V, D>[LW]` that relates to anything,
// and `Array.isArray` narrows the result so the callback parameter is `any`.
type LW = "narrow" | "short" | "any";
type PP<V> = V extends 1 ? RegExp[] : string;
type A<V> = { [W in LW]?: PP<V> };
type B<V, D extends LW> = { [W in D]: PP<V> };
type PPs<V, D extends LW> = A<V> & B<V, D>;
declare function findIndex<Item>(array: Item[], predicate: (item: Item) => boolean): number;
function f<V, D extends LW>(c: PPs<V, D>, w: LW, d: D, z: PPs<V, D>[LW]) {
  const y3 = c[w];
  const n3: number = y3;
  const y4 = c["any"];
  const n4: number = y4;
  const n5: number = z;
  const b = w && c[w];
  if (Array.isArray(b)) {
    findIndex(b, (pattern) => pattern.test("x"));
  }
  const e = c[d];
  if (Array.isArray(e)) {
    findIndex(e, (pattern) => pattern.test("x"));
  }
}
// `DeepPartial<T>` (a conditional with a `T` branch) narrowed by `Array.isArray` is
// `DeepPartial<T> & any[]`, so `source.length` and `source[key]` are fine.
type DeepPartial<T> = T | (T extends Array<infer U> ? DeepPartial<U>[] : T extends object ? { [K in keyof T]?: DeepPartial<T[K]> } : T);
declare function g(a: any, k: number, v: any): void;
function h<T>(target: T, source: DeepPartial<T> | undefined) {
  if (Array.isArray(target) && Array.isArray(source)) {
    for (let key = 0; key < source.length; key++) {
      g(target, key, source[key]);
    }
  }
}
// trpc `Unpromise.resolve`: `T & null` from `typeof value === "object"` is possibly null, and
// `value !== null` removes it.
function r<T>(value: T | PromiseLike<T>, k: keyof T) {
  if (typeof value === "object") {
    value.toString();
  }
  const p: PromiseLike<T> | undefined =
    typeof value === "object" && value !== null && "then" in value && typeof value.then === "function"
      ? value
      : undefined;
  return p;
}
// trpc `clientCallTypeMap[clientCallType as keyof typeof clientCallTypeMap]`: a `never` key.
const callMap: Record<keyof ({ query: 1 } | { mutate: 1 }), string> = { query: "q" };
const fromNever: number = callMap["x" as keyof typeof callMap];
export {};
