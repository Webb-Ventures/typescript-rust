// Two instantiations of one alias of a generic mapped type relate by the alias's variances
// before the mapped type's keys are asked for (tRPC's `createCallerFactory`:
// `DecorateRouterRecord<any>` is a `DecorateRouterRecord<TRecord>`).
type Rec = { [k: string]: unknown };
type D<T extends Rec> = {
  [K in keyof T]: T[K] extends infer V ? (V extends string ? 1 : 2) : never;
};
declare const anyD: D<any>;
declare const numD: D<{ a: number }>;
export function f<T extends Rec>(): D<T> {
  return anyD;
}
export function g<T extends Rec>(): D<T> {
  return numD;
}
type Caller<T extends Rec> = (ctx: unknown) => D<T>;
export function h<T extends Rec>(): Caller<T> {
  return function (ctx) {
    return anyD;
  };
}
type P<T> = { [K in keyof T]?: T[K] };
export function k<T, U extends T>(x: P<T>, y: P<U>, z: P<any>, w: P<unknown>): P<U> {
  if (Math.random()) return z;
  if (Math.random()) return w;
  if (Math.random()) return y;
  return x;
}
