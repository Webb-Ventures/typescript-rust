// unknown plus typeof narrowing instead of any.
function describe(value: unknown): string {
  if (typeof value === "string") return value.toUpperCase();
  if (typeof value === "number") return value.toFixed(2);
  return "other";
}
export const label: string = describe(42);
