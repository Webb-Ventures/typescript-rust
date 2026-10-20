// A branch behind a constant-false condition never runs, so its return does not count against an overload.
function label(value: string): string;
function label(value: number): number;
function label(value: string | number): string | number {
  if (false) return 0;
  while (false) return 0;
  if (typeof value === "string") return value;
  return value + 1;
}
export const l: string = label("a");
