// Called through the number overload, the string test fails and the body returns undefined.
function label(value: string): string;
function label(value: number): string;
function label(value: string | number): string | undefined {
  if (typeof value === "string") return value;
  return undefined; // @expect TSR1205
}
export const u: string = label(1);
