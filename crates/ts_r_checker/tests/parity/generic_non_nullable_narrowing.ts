// trpc `operators.ts` (`isDeepEqual`: `const bothAreObjects = a && b && typeof a === 'object'
// && typeof b === 'object'; ... !!bothAreObjects && Object.keys(a)`): a truthy or non-null
// generic `T` is `NonNullable<T>` (`T & {}`), `typeof t === "object"` on it gives `T & object`,
// and a const alias of a condition narrows through its initializer.
function f<T, U extends string | undefined, V extends object | null>(a: T, u: U, v: V) {
  if (a) {
    const n: null = a;
  }
  if (!a) {
    const n: null = a;
  }
  if (u) {
    const n: null = u;
  }
  if (v) {
    const n: null = v;
  }
  if (a != null) {
    const n: null = a;
  }
  const x = a!;
  const n2: null = x;
}
function g<T>(a: T, b: T) {
  if (typeof a === "object") {
    const n: null = a;
  }
  const both = a && b && typeof a === "object" && typeof b === "object";
  if (both) {
    const n: null = a;
  }
  const r = !!both && Object.keys(a).length;
  if (!!both) {
    const n2: null = b;
  }
}
function h(x: string | number) {
  const isS = typeof x === "string";
  if (!!isS) {
    const n: null = x;
  }
  const r = isS && x.length;
}
declare function foo(p: string): void;
function k<T extends string | undefined, U extends string>(one: T, two: U) {
  let three = Boolean() ? one : two;
  foo(one!);
  foo(two!);
  foo(three!);
}
export {};
