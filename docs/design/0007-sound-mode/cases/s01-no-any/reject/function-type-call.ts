// Calling a value of type Function returns any.
function invoke(fn: Function): unknown {
  return fn(); // @expect TSR0103
}
export const r: unknown = invoke(() => 1);
