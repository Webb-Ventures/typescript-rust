// date-fns `_lib/tp` and `constructFrom`: `"p" in x` on a type parameter (or a union with
// one) intersects it with `Record<"p", unknown>`; a symbol key keeps the members that have it.
declare const sym: unique symbol;
type DateArg<D extends Date> = D | number | string;
interface CD extends Date {
  [sym]: <D extends Date = Date>(value: DateArg<Date> & {}) => D;
}
type ContextFn<D extends Date> = (value: DateArg<Date> & {}) => D;

function tz<D extends Date>(date: DateArg<D>): string {
  if (typeof date === "object" && "timeZone" in date && typeof date.timeZone === "string")
    return date.timeZone;
  return "";
}

export function constructFrom<D extends Date | CD, R extends Date = D>(
  date: DateArg<D> | ContextFn<R> | undefined,
  value: DateArg<Date> & {},
): R {
  if (typeof date === "function") return date(value);
  if (date && typeof date === "object" && sym in date) return date[sym](value);
  return new Date(value) as R;
}

function neither(x: { a: 1 } | { b: 2 }) {
  if ("c" in x) {
    const c: number = x.c;
  }
}
export { tz, neither };
