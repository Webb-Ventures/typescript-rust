// The branches are swapped: called with a string, the body returns its length.
function size(value: string): string;
function size(value: number): number;
function size(value: string | number): string | number {
  return typeof value === "string" ? value.length : value; // @expect TSR1205
}
export const s: string = size("abc");
