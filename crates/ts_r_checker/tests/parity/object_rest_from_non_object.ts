// TS2700 at a rest element's name when the destructured value is `unknown` or cannot be
// spread; `any`, objects and type parameters are fine.
declare const nu: null | undefined;
let { ...r1 } = nu;
declare const iu: ({ n: number } & { s: string }) & undefined;
let { ...r2 } = iu;
function f(x: unknown, y: any, z: { a: number } | undefined) {
  let { ...a } = x;
  let { ...b } = y;
  let { ...c } = z ?? {};
}
function g<T>(t: T) {
  let { ...d } = t;
}
