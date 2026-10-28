// `a.constructor !== b.constructor`: `b.constructor` is `Function`, whose `prototype` is
// `any`, so nothing narrows (rxjs `packages/test/src/assertions.ts`).
function f(actual: unknown, expected: unknown) {
  if (typeof actual !== "object" || actual === null || typeof expected !== "object" || expected === null) return;
  if (ArrayBuffer.isView(actual) && ArrayBuffer.isView(expected)) {
    if (actual.constructor !== expected.constructor) return;
    actual.buffer;
  }
}
function g(x: string | number | Date, y: object) {
  if (x.constructor === String) { const s: string = x; }
  if (x.constructor === Date) { const d: Date = x; }
  if (y.constructor === Date) { const d: never = y; }
  if (x.constructor === Object) { const n: number = x; }
}
class A { a = 1 }
class B { a = 1 }
declare const ctor: new () => A;
function h(v: A | B, o: { a: number }) {
  if (v.constructor === A) { const a: A = v; const b: B = v; }
  if (o.constructor === A) { const n: never = o; }
  if (v.constructor === ctor) { const n: never = v; }
}
export { f, g, h };
