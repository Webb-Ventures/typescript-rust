// A generic guard that always says yes.
function isPresent<T>(x: T | null): x is T { // @expect TSR0701
  return true;
}
function orEmpty(v: string | null): string {
  return isPresent(v) ? v : "";
}
export const s: string = orEmpty(null);
