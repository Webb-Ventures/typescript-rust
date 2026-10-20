// date-fns `intlFormatDistance.bind(null, a, b, { unit: "wrongValue" })` and `max`'s
// `constructFrom.bind(null, date)`: `CallableFunction.bind`'s `(this: T, ...args: [...A,
// ...B]) => R` splits the receiver's parameters at the arity the bound arguments give `A`,
// and a generic receiver infers through its base signature.
interface O { unit?: "a" | "b"; n?: number }
declare function f(x: Date, y: Date, o?: O): string;
const g = f.bind(null, new Date(), new Date(), { unit: "wrong" });
const h = f.bind(null, new Date(), new Date(), { n: 1 });
const k = f.bind(null, new Date(), 5);
const m = f.bind(null);
const m2: number = m;
const m3: number = f.bind(null, new Date());
class C {
  x = 1;
  meth(a: string, b: number) {
    return this.x;
  }
}
const c = new C();
const bm: string = c.meth.bind(c, "s");
declare function b<A extends any[], B extends any[], R>(fn: (...args: [...A, ...B]) => R, ...args: A): (...args: B) => R;
const k2: number = b(f, new Date(), 5);
const k3: number = b(f, new Date());
interface CD extends Date { x: 1 }
type DateArg<D extends Date> = D | number | string;
type ContextFn<D extends Date> = (value: DateArg<Date> & {}) => D;
declare function constructFrom<DateType extends Date | CD, ResultDate extends Date = DateType>(
  date: DateArg<DateType> | ContextFn<ResultDate> | undefined,
  value: DateArg<Date> & {},
): ResultDate;
function max<R extends Date>(d: Date) {
  const cf = constructFrom.bind(null, d) as ContextFn<R>;
  const n: number = constructFrom.bind(null, d);
}
// A tuple read past its end at the type level is `undefined` (TS2493 at the index), its
// rest element type past the fixed elements.
type P = { t: [a: string, b: number] };
const x: P["t"][2] = 1;
type R = [a: string, ...b: boolean[]];
const r: R[5] = 1;
function fn(a: unknown, b: unknown) {
  return "";
}
const q: Parameters<typeof fn>[3] = 1;
export {};
