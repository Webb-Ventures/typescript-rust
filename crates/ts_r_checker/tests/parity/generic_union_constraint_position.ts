// trpc `transformer.ts` (`Array.isArray(itemOrItems) ? ... : transformTRPCResponseItem(config, itemOrItems)`
// with `TResponse extends A | A[] | B | B[]`): a reference of a generic type with a union
// constraint reads through the constraint where its contextual type has no generic types
// (a non-generic parameter, an annotated variable) and as a callee or accessed object.
export {};
interface A { a: 1 }
interface B { b: 1 }
declare function one(x: A | B): string;
declare function gen<T>(x: T): T;
function f<T extends A | A[] | B | B[]>(x: T) {
  const r = Array.isArray(x) ? x.map((i) => one(i)) : one(x);
  if (!Array.isArray(x)) {
    one(x);
    const n: null = x;
    gen(x);
  }
}
function g<T extends string | number>(x: T) {
  if (typeof x === "string") {
    const s: string = x;
    const n: null = x;
  }
}
