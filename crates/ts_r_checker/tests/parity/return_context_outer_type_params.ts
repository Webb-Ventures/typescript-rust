// date-fns `constructFrom`/`setISOWeek`: the type parameters of the enclosing function
// are fixed types for return-type inference (`return make()` in `f<R>(): R` infers `R`),
// and an inference from the context that breaks the constraint gives way to the default.
declare const sym: unique symbol;
interface CD extends Date {
  [sym]: <D extends Date = Date>(value: number) => D;
}
export function f1<R extends Date>(date: CD): R {
  return date[sym](1);
}

type DateArg<D extends Date> = D | number | string;
declare function setW<D extends Date, R extends Date = D>(date: DateArg<D>, week: number): R;
declare function startW<D extends Date, R extends Date = D>(date: DateArg<D>): R;

export function set<D extends Date>(date: D, value: number): D {
  const a = setW(date, value);
  const b: D = startW(a);
  return startW(setW(date, value));
}
export function set2<D extends Date>(date: D, value: number) {
  return startW(setW(date, value));
}
declare function make<T>(): T;
export function g<T extends string>(): T {
  const n: number = make();
  return make();
}
