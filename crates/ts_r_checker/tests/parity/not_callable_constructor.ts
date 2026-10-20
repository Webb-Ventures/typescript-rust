// A value with construct signatures but no call signatures is not callable (TS2348),
// even though it is assignable to `Function`.
class C { x = 1; }
abstract class A {}
declare const ctor: new () => C;
declare const both: { new (): C; (): number };
declare const fn: Function;
declare const fc: Function & (new () => C);
interface I { new (n: number): C }
declare const i: I;
const a = C();
const b = A();
const c = ctor();
const d: number = both();
const e = fn();
const f = fc();
const g = i(1);
const h = Date();
const k = new fn();
export { a, b, c, d, e, f, g, h, k };
