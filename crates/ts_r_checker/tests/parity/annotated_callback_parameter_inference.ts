// A context-sensitive callback (`part` has no annotation) whose other parameter is
// annotated: only the unannotated positions fix type parameters, so `U` of `prev: U` is
// still inferred from the callback's return (`reduce((set: string[], part) => set, [])`
// is `string[]`, not `never[]`).
declare const parts: string[];
const r1 = parts.reduce((set: string[], part) => {
  set.push(part);
  return set;
}, []);
const n1: number = r1;
declare function red<U>(cb: (prev: U, cur: string) => U, init: U): U;
const n2: number = red((set: string[], part) => set, []);
const n3: number = red((set: string[]) => set, []);
const n4: number = red((set, part) => set, []);
declare function red2<U>(init: U, cb: (prev: U, cur: string) => U): U;
const n5: number = red2([], (set: string[], part) => set);
const n6: number = red2([] as number[], function (this: void, set: number[], part) { return set; });
