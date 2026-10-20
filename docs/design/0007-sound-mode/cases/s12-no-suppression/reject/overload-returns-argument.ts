// Called through the second overload, the body returns the number it was given, not a string.
function first(value: string): string;
function first(value: number): string;
function first(value: string | number): string | number {
  return value; // @expect TSR1205
}
export const n: string = first(7);
