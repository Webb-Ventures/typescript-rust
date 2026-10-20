// `for (k in obj)`: the object without `null` and `undefined` must be `any`, a non-primitive
// type variable or assignable to `object` (TS2407), and `obj` is not nullish in the body.
declare const o: { a: 1 } | null | undefined;
for (const k in o) {
  const n: null = o;
}
declare let o2: { a: 1 } | null | string;
for (const k in o2) {
  const n: null = o2;
}
function f<T>(obj: T) {
  for (let k in obj) {
    const n1: null = obj;
  }
  if (typeof obj === "object") {
    for (let k in obj) {
      const n1: null = obj;
      const v = obj[k];
    }
    const v2 = obj["a" as keyof T];
  }
}
const ob = { x: o };
for (const k in ob.x) {
  const n: null = ob.x;
}
let key: string;
for (key in o) {
  const n: null = o;
}
declare const u: unknown;
for (const k in u) {}
declare const a: any;
for (const k in a) {}
declare const nl: null;
for (const k in nl) {}
function g<T, U extends string, V extends object | undefined>(t: T, x: U, v: V, w: T | string, i: T & {}) {
  for (const k in t) {}
  for (const k in x) {}
  for (const k in v) {}
  for (const k in w) {}
  for (const k in i) {}
}
declare const e: {} | number;
for (const k in e) {}
declare const e2: {};
for (const k in e2) {}
declare const fn: () => void;
for (const k in fn) {}
declare const s: string;
for (const k in s) {}
export {};
