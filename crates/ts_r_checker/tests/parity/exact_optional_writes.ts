// Under exactOptionalPropertyTypes: writes to optional properties reject `undefined` (TS2412
// for `o.p = v`), object relations name the option (TS2375, TS2379 for arguments), `'p' in o`
// and `o.hasOwnProperty('p')` narrow `o.p`, and `missing` infers like `undefined`.
type A = { a?: string };
declare const su: string | undefined;
declare const nu: number | undefined;
declare let obj: { a?: string; b?: string | undefined; c?: { x: number } };
obj.a = 5;
obj.a = nu;
obj["a"] = undefined;
obj.a ??= undefined;
obj.c = { x: undefined };
obj.c = undefined;
obj.b = undefined;
const o1: A = { a: 5 };
const o2: A = { a: undefined };
const o3: { c?: { x?: number } } = { c: { x: undefined } };
declare function f(a: A): void;
f({ a: undefined });
f({ a: su });
function g(): A { return { a: su }; }
const o4: A = { a: nu };
declare const src: { a: string | undefined };
const o5: A = src;
const o6: { a?: string | number } = 1 as any as A;
const o7: { a: number } = 1 as any as { a?: number };
const o8: { a: string } = 1 as any as { a?: number };
function h(o: { a?: string; b?: string | undefined }) {
  if ('a' in o) {
    const x: number = o.a;
  } else {
    const x: number = o.a;
  }
  if (o.hasOwnProperty('a')) {
    o.a = o.a;
  } else {
    o.a = o.a;
  }
  if ('b' in o) {
  } else {
    const x: number = o.b;
  }
}
declare function expectDefined<T>(value: T | undefined): T;
declare const oc: { c?: { x: number } };
const r: string = expectDefined(oc.c);
export { o1, o2, o3, o4, o5, o6, o7, o8, g, h, r };
// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "exactOptionalPropertyTypes": true, "noEmit": true}, "files": ["a.ts"]}
